//! Synchronous, poll-based block emitter over Bitcoin Core multiprocess IPC.
//!
//! [`IpcEmitter`] mirrors the JSON-RPC [`Emitter`](crate::Emitter): drive it with
//! [`IpcEmitter::next_block`] in a loop and apply the yielded [`BlockEvent`]s to a `LocalChain`
//! and `IndexedTxGraph`. Internally it owns a current-thread tokio runtime and drives the async
//! Cap'n Proto session via `LocalSet::block_on`, so the public API stays synchronous.

use bdk_core::{BlockId, CheckPoint};
use bitcoin::{Block, BlockHash};

use super::rpc::RpcInterface;
use super::Error;
use crate::BlockEvent;

/// Poll-based block emitter over Bitcoin Core's multiprocess IPC (Cap'n Proto) `Chain` interface.
///
/// This is the IPC analogue of [`Emitter`](crate::Emitter), restricted to block emission (no
/// mempool). It walks the chain forward by height (pinned to the node tip) and handles reorgs by
/// walking back to the common ancestor, producing the same [`BlockEvent<Block>`] the JSON-RPC path
/// does.
///
/// Requires a node built from Bitcoin Core PR #29409. The emitter owns its runtime and is bound to
/// a single thread; it must be driven from a non-async context.
pub struct IpcEmitter {
    // Current-thread runtime + LocalSet that drive the (!Send) capnp-rpc session. `block_on` on
    // this LocalSet keeps the spawned RpcSystem task making progress across calls.
    rt: tokio::runtime::Runtime,
    local: tokio::task::LocalSet,
    rpc: RpcInterface,
    /// Checkpoint of the last-emitted block that is known to be in the node's best chain.
    last_cp: CheckPoint<BlockHash>,
    /// Minimum height to emit from; may be lowered on a deep reorg.
    start_height: u32,
    /// Height of the last-emitted block, or `None` before the first emission / after a reset.
    last_height: Option<u32>,
}

impl IpcEmitter {
    /// Connect to `bitcoin-node` over its multiprocess IPC unix socket and construct an emitter.
    ///
    /// `last_cp` is the chain the caller already knows about (e.g. a birthday checkpoint); emission
    /// resumes from a block that connects to it. `start_height` skips ahead to a minimum height.
    pub fn new(
        socket_path: impl AsRef<std::path::Path>,
        last_cp: CheckPoint<BlockHash>,
        start_height: u32,
    ) -> Result<Self, Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        let local = tokio::task::LocalSet::new();
        let path = socket_path.as_ref().to_owned();
        let rpc = local.block_on(&rt, async move {
            let stream = tokio::net::UnixStream::connect(&path).await?;
            RpcInterface::connect(stream).await
        })?;
        Ok(Self {
            rt,
            local,
            rpc,
            last_cp,
            start_height,
            last_height: None,
        })
    }

    /// Emit the next block, or `Ok(None)` when the node tip is reached.
    ///
    /// Reorgs are handled transparently: the returned [`BlockEvent::checkpoint`] connects to the
    /// previously emitted chain, so `LocalChain::apply_update` resolves any rollback.
    pub fn next_block(&mut self) -> Result<Option<BlockEvent<Block>>, Error> {
        // Pull the mutable state into locals so the async closure can borrow it without conflicting
        // with the immutable borrows of `self.local` / `self.rt` / `self.rpc` that `block_on`
        // needs.
        let mut last_cp = self.last_cp.clone();
        let mut start_height = self.start_height;
        let mut last_height = self.last_height;

        let out = {
            let rpc = &self.rpc;
            self.local.block_on(
                &self.rt,
                poll_next(rpc, &mut last_cp, &mut start_height, &mut last_height),
            )
        };

        // Persist the (possibly rewound) state, whether we emitted a block or hit an error, so the
        // next call resumes correctly.
        self.last_cp = last_cp;
        self.start_height = start_height;
        self.last_height = last_height;
        out
    }
}

/// One turn of the poll/reorg state machine. Mirrors the JSON-RPC emitter's `poll`/`poll_once`, but
/// walks by height pinned to the node tip hash (there is no `nextblockhash` over IPC).
async fn poll_next(
    rpc: &RpcInterface,
    last_cp: &mut CheckPoint<BlockHash>,
    start_height: &mut u32,
    last_height: &mut Option<u32>,
) -> Result<Option<BlockEvent<Block>>, Error> {
    loop {
        // 1. Read the node tip (height + hash). Every fetch this turn is pinned to this hash so a
        //    concurrent reorg cannot give us a torn view.
        let tip_height_i32 = rpc.tip_height().await?;
        let tip_hash = rpc.block_hash(tip_height_i32).await?;
        let tip_height =
            u32::try_from(tip_height_i32).map_err(|_| Error::HeightConversion(tip_height_i32))?;

        match *last_height {
            // First emission: establish where our checkpoint chain connects to the node's chain.
            None => {
                let agreement = {
                    let mut found = None;
                    for cp in last_cp.iter() {
                        if rpc.is_ancestor(&tip_hash, &cp.hash()).await? {
                            found = Some(cp);
                            break;
                        }
                    }
                    found
                };
                match agreement {
                    Some(cp) => {
                        *last_height = Some(cp.height());
                        *last_cp = cp;
                    }
                    None => {
                        // Nothing we know is on the node's chain; reset to genesis and re-emit.
                        let genesis_hash = rpc.block_hash(0).await?;
                        *last_cp = CheckPoint::new(0, genesis_hash);
                        *last_height = Some(0);
                    }
                }
                continue;
            }
            Some(prev_height) => {
                // 2. Make sure our last-emitted block is still in the node's best chain.
                if !rpc.is_ancestor(&tip_hash, &last_cp.hash()).await? {
                    // Reorg: walk back to the common ancestor and re-emit from there.
                    let ancestor = rpc
                        .common_ancestor(&tip_hash, &last_cp.hash())
                        .await?
                        .ok_or(Error::ReorgTooDeep)?;
                    rewind_to(last_cp, &ancestor);
                    if ancestor.height < *start_height {
                        *start_height = ancestor.height;
                    }
                    *last_height = Some(ancestor.height);
                    continue;
                }

                // 3. Determine the next height to emit, honouring the start height.
                let next_height = prev_height.saturating_add(1).max(*start_height);
                if next_height > tip_height {
                    return Ok(None);
                }

                // 4. Fetch the block at that height on the tip's branch and extend the checkpoint.
                //    Bitcoin heights fit comfortably in i32, as the node itself uses i32 heights.
                let block = rpc.block_at_height(&tip_hash, next_height as i32).await?;
                let hash = block.block_hash();
                let new_cp = last_cp
                    .clone()
                    .push(next_height, hash)
                    .map_err(|_| Error::CheckpointPush)?;
                *last_cp = new_cp.clone();
                *last_height = Some(next_height);
                return Ok(Some(BlockEvent {
                    block,
                    checkpoint: new_cp,
                }));
            }
        }
    }
}

/// Rewind `cp` to the checkpoint at `ancestor.height`. If that height is not in our (sparse)
/// checkpoint chain, rebuild from the ancestor's `(height, hash)` alone so emission can continue.
fn rewind_to(cp: &mut CheckPoint<BlockHash>, ancestor: &BlockId) {
    match cp.get(ancestor.height) {
        Some(found) => *cp = found,
        None => *cp = CheckPoint::new(ancestor.height, ancestor.hash),
    }
}
