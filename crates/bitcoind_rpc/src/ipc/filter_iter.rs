//! Compact-block-filter (BIP158) scanner over Bitcoin Core multiprocess IPC.
//!
//! [`IpcFilterIter`] mirrors the JSON-RPC [`FilterIter`](crate::bip158::FilterIter), with one key
//! difference: filter matching happens *node-side* via `Chain::blockFilterMatchesAny`. The client
//! sends its script pubkeys per block and gets back a single bool, so filters are never
//! downloaded and full blocks only cross the socket when they match. The node must maintain the
//! BASIC filter index (`-blockfilterindex=1`).

use bdk_core::CheckPoint;
use bitcoin::{BlockHash, ScriptBuf};

use super::emitter::rewind_to;
use super::rpc::IpcInterface;
use super::Error;
use crate::bip158::Event;

/// BIP158 BASIC filter type byte (BIP157 `filter_type = 0x00`).
const FILTER_TYPE_BASIC: u8 = 0;

/// BIP158 filter scanner over Bitcoin Core's multiprocess IPC (Cap'n Proto) `Chain` interface.
///
/// This is the IPC analogue of [`FilterIter`](crate::bip158::FilterIter): iterate it to get an
/// [`Event`] per height, where [`Event::block`] is `Some` only when the block's filter matches one
/// of the watched scripts. Every event's checkpoint connects to the previous one, so applying
/// `event.cp` to a `LocalChain` tracks the header chain while only matching blocks are fetched.
///
/// Reorgs are handled like [`IpcEmitter`](super::IpcEmitter): rewind to the common ancestor and
/// rescan (unlike the JSON-RPC `FilterIter`, which errors with `ReorgDepthExceeded` when the
/// checkpoint chain disagrees entirely; here a genesis rescan is cheap since non-matching blocks
/// are never downloaded).
///
/// Requires a node built from Bitcoin Core PR #29409, run with `-blockfilterindex=1`. The scanner
/// owns its runtime and is bound to a single thread; it must be driven from a non-async context.
pub struct IpcFilterIter {
    // Current-thread runtime + LocalSet that drive the (!Send) capnp-rpc session. `block_on` on
    // this LocalSet keeps the spawned RpcSystem task making progress across calls.
    rt: tokio::runtime::Runtime,
    local: tokio::task::LocalSet,
    rpc: IpcInterface,
    /// Watched script pubkeys, sent to the node for every per-block match query.
    spks: Vec<ScriptBuf>,
    /// Checkpoint of the last scanned block that is known to be in the node's best chain.
    cp: CheckPoint<BlockHash>,
    /// Height of the last event, or `None` before the agreement point is established.
    last_height: Option<u32>,
}

impl IpcFilterIter {
    /// Connect to `bitcoin-node` over its multiprocess IPC unix socket and construct a scanner
    /// watching `spks`.
    ///
    /// `cp` is the chain the caller already knows about (e.g. a birthday checkpoint); scanning
    /// resumes from a block that connects to it. Returns [`Error::NoBlockFilterIndex`] if the node
    /// does not maintain the BASIC filter index.
    pub fn new(
        socket_path: impl AsRef<std::path::Path>,
        cp: CheckPoint<BlockHash>,
        spks: impl IntoIterator<Item = ScriptBuf>,
    ) -> Result<Self, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        let local = tokio::task::LocalSet::new();
        let path = socket_path.as_ref().to_owned();
        let rpc = local.block_on(&rt, async move {
            let stream = tokio::net::UnixStream::connect(&path).await?;
            let rpc = IpcInterface::connect(stream).await?;
            if !rpc.has_block_filter_index(FILTER_TYPE_BASIC).await? {
                return Err(Error::NoBlockFilterIndex);
            }
            Ok(rpc)
        })?;
        Ok(Self {
            rt,
            local,
            rpc,
            spks: spks.into_iter().collect(),
            cp,
            last_height: None,
        })
    }
}

impl Iterator for IpcFilterIter {
    type Item = Result<Event, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        // Pull the mutable state into locals so the async closure can borrow it without
        // conflicting with the immutable borrows of `self.local` / `self.rt` / `self.rpc` that
        // `block_on` needs.
        let mut cp = self.cp.clone();
        let mut last_height = self.last_height;

        let out = {
            let rpc = &self.rpc;
            let spks = &self.spks;
            self.local.block_on(
                &self.rt,
                poll_next_event(rpc, &mut cp, &mut last_height, spks),
            )
        };

        // Persist the (possibly rewound) state, whether we yielded an event or hit an error, so
        // the next call resumes correctly.
        self.cp = cp;
        self.last_height = last_height;
        out.transpose()
    }
}

/// One turn of the scan/reorg state machine. Mirrors `poll_next` in `emitter.rs`, but instead of
/// fetching every block it asks the node to match the block's BIP158 filter against `spks` and
/// only fetches block data on a match.
async fn poll_next_event(
    rpc: &IpcInterface,
    cp: &mut CheckPoint<BlockHash>,
    last_height: &mut Option<u32>,
    spks: &[ScriptBuf],
) -> Result<Option<Event>, Error> {
    loop {
        // 1. Read the node tip (height + hash). Every query this turn is pinned to this hash so a
        //    concurrent reorg cannot give us a torn view.
        let tip_height_i32 = rpc.tip_height().await?;
        let tip_hash = rpc.block_hash(tip_height_i32).await?;
        let tip_height =
            u32::try_from(tip_height_i32).map_err(|_| Error::HeightConversion(tip_height_i32))?;

        match *last_height {
            // First call: establish where our checkpoint chain connects to the node's chain.
            None => {
                let agreement = {
                    let mut found = None;
                    for c in cp.iter() {
                        if rpc.is_ancestor(&tip_hash, &c.hash()).await? {
                            found = Some(c);
                            break;
                        }
                    }
                    found
                };
                match agreement {
                    Some(c) => {
                        *last_height = Some(c.height());
                        *cp = c;
                    }
                    None => {
                        // Nothing we know is on the node's chain; reset to genesis and rescan.
                        let genesis_hash = rpc.block_hash(0).await?;
                        *cp = CheckPoint::new(0, genesis_hash);
                        *last_height = Some(0);
                    }
                }
                continue;
            }
            Some(prev_height) => {
                // 2. Make sure our last scanned block is still in the node's best chain.
                if !rpc.is_ancestor(&tip_hash, &cp.hash()).await? {
                    // Reorg: walk back to the common ancestor and rescan from there.
                    let ancestor = rpc
                        .common_ancestor(&tip_hash, &cp.hash())
                        .await?
                        .ok_or(Error::ReorgTooDeep)?;
                    rewind_to(cp, &ancestor);
                    *last_height = Some(ancestor.height);
                    continue;
                }

                // 3. Determine the next height to scan; stop at the tip.
                let next_height = prev_height.saturating_add(1);
                if next_height > tip_height {
                    return Ok(None);
                }

                // 4. Hash pinned to the tip's branch (not the active chain, which could move
                //    between the match query and a block fetch), then the node-side filter match.
                let hash = rpc
                    .block_hash_at_height(&tip_hash, next_height as i32)
                    .await?;
                let matched = rpc
                    .block_filter_matches_any(FILTER_TYPE_BASIC, &hash, spks)
                    .await?
                    .ok_or(Error::FilterUnavailable {
                        height: next_height,
                    })?;

                // 5. Fetch the block only on a match. Pinned to the same tip branch, so the block
                //    fetched is the one whose filter matched.
                let block = if matched {
                    Some(rpc.block_at_height(&tip_hash, next_height as i32).await?)
                } else {
                    None
                };

                let new_cp = cp
                    .clone()
                    .push(next_height, hash)
                    .map_err(|_| Error::CheckpointPush)?;
                *cp = new_cp.clone();
                *last_height = Some(next_height);
                return Ok(Some(Event { cp: new_cp, block }));
            }
        }
    }
}
