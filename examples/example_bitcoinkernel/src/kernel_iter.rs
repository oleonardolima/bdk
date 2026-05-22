//! A block emitter over a local Bitcoin Core block store, via `bitcoinkernel`.
//!
//! Mirrors the `bdk_bitcoind_rpc::Emitter` pattern: emitted checkpoints are linked to
//! the start checkpoint, so `LocalChain::apply_update` always finds a point of agreement.

use bdk_chain::bitcoin::consensus::deserialize;
use bdk_chain::bitcoin::hashes::Hash;
use bdk_chain::bitcoin::{Block, BlockHash};
use bdk_chain::CheckPoint;
use bdk_testenv::anyhow;
use bitcoinkernel::core::{BlockHashExt, BlockHeaderExt, BlockTreeEntry};
use bitcoinkernel::ChainstateManager;

/// Convenience conversions from a kernel [`BlockTreeEntry`] to rust-bitcoin values.
pub trait BlockTreeEntryExt {
    /// The height of this entry as a `u32`.
    fn height_u32(&self) -> u32;
    /// The block hash of this entry as a rust-bitcoin [`BlockHash`].
    fn hash(&self) -> BlockHash;
}

impl BlockTreeEntryExt for BlockTreeEntry<'_> {
    fn height_u32(&self) -> u32 {
        self.height() as u32
    }

    fn hash(&self) -> BlockHash {
        BlockHash::from_byte_array(self.header().hash().to_bytes())
    }
}

/// Convenience methods for reading rust-bitcoin values out of a kernel [`ChainstateManager`].
pub trait ChainstateManagerExt {
    /// Read the block data for `entry` and deserialize it into a rust-bitcoin [`Block`].
    fn read_bitcoin_block(&self, entry: &BlockTreeEntry) -> anyhow::Result<Block>;
}

impl ChainstateManagerExt for ChainstateManager {
    fn read_bitcoin_block(&self, entry: &BlockTreeEntry) -> anyhow::Result<Block> {
        Ok(deserialize::<Block>(
            &self.read_block_data(entry)?.consensus_encode()?,
        )?)
    }
}

/// A block emitted by [`KernelIter`].
pub struct Event {
    /// The emitted block.
    pub block: Block,
    /// Checkpoint at the emitted block's height, linked back to the start checkpoint.
    pub checkpoint: CheckPoint,
}

/// Iterates blocks of the kernel's active chain, starting after the point of agreement
/// with the given checkpoint.
pub struct KernelIter<'a> {
    chainman: &'a ChainstateManager,
    /// Last emitted (or agreed-upon) checkpoint.
    cp: CheckPoint,
    /// Height of the next block to emit. `None` until the agreement point is found.
    next_height: Option<u32>,
}

impl<'a> KernelIter<'a> {
    /// Construct a [`KernelIter`] that emits blocks above the point of agreement between
    /// `cp` and the active chain of `chainman`.
    pub fn new(chainman: &'a ChainstateManager, cp: CheckPoint) -> Self {
        Self {
            chainman,
            cp,
            next_height: None,
        }
    }

    /// Walk `self.cp` backward to the highest block still on the kernel's active chain.
    ///
    /// Returns the height of the next block to emit. This handles the case where the
    /// datadir reorged between runs: stale checkpoint blocks are skipped, so emitted
    /// checkpoints always connect to the original chain.
    fn find_agreement(&mut self) -> anyhow::Result<u32> {
        let active_chain = self.chainman.active_chain();
        for cp in self.cp.iter() {
            // Compare against the active chain by height: a checkpoint block on a stale fork
            // won't match the active-chain block at the same height, so we keep walking back.
            if let Some(entry) = active_chain.at_height(cp.height() as usize) {
                if entry.hash() == cp.hash() {
                    let agreement_height = cp.height();
                    self.cp = cp;
                    return Ok(agreement_height + 1);
                }
            }
        }
        anyhow::bail!("no point of agreement: no checkpoint block is on the kernel's active chain")
    }

    fn try_next(&mut self) -> anyhow::Result<Option<Event>> {
        let next_height = match self.next_height {
            Some(height) => height,
            None => {
                let height = self.find_agreement()?;
                self.next_height = Some(height);
                height
            }
        };

        let active_chain = self.chainman.active_chain();
        let entry = match active_chain.at_height(next_height as usize) {
            Some(entry) => entry,
            // tip reached.
            None => return Ok(None),
        };

        let height = entry.height_u32();
        let hash = entry.hash();
        let block = self.chainman.read_bitcoin_block(&entry)?;

        self.cp = self
            .cp
            .clone()
            .push(height, hash)
            .expect("emitted heights are strictly increasing");
        self.next_height = Some(height + 1);

        Ok(Some(Event {
            block,
            checkpoint: self.cp.clone(),
        }))
    }
}

impl Iterator for KernelIter<'_> {
    type Item = anyhow::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        self.try_next().transpose()
    }
}
