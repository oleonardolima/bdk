//! Thin async wrappers around the Bitcoin Core `Chain` IPC interface.
//!
//! This layer owns the Cap'n Proto RPC session and exposes only the handful of `Chain` methods
//! the block emitter needs. It is intentionally async (capnp-rpc is future-based); the
//! synchronous [`IpcEmitter`](super::IpcEmitter) drives it via a current-thread runtime.
//!
//! Every request must carry the handshake `Thread` capability in its `Context`, otherwise the
//! call never completes. Adapted from the `darosior/core_bdk_wallet` reference PoC.

// Not every Chain wrapper is exercised by the block-only emitter (e.g. `has_blocks` is an
// optional pruning guard), so allow unused transport helpers.
#![allow(dead_code)]

use bdk_core::BlockId;
use bitcoin::{consensus::Decodable, hashes::Hash, Block, BlockHash};
use capnp_rpc::{rpc_twoparty_capnp, twoparty, RpcSystem};
use tokio::task::{self, JoinHandle};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::capnp_gen::{
    chain_capnp::chain::Client as ChainClient, init_capnp::init::Client as InitClient,
    proxy_capnp::thread::Client as ThreadClient,
};
use super::Error;

/// Owns the Cap'n Proto RPC session to `bitcoin-node` and the `Chain` capability.
///
/// All capabilities here are `!Send`; the owning [`IpcEmitter`](super::IpcEmitter) keeps this on
/// a single thread driven by a `LocalSet`.
pub(super) struct RpcInterface {
    /// The spawned `RpcSystem` future; must be continuously driven for calls to make progress.
    rpc_handle: JoinHandle<Result<(), capnp::Error>>,
    /// Used to cleanly shut down the session.
    disconnector: capnp_rpc::Disconnector<twoparty::VatId>,
    /// The per-session thread capability that every call's `Context` must reference.
    thread: ThreadClient,
    /// The `Chain` interface capability.
    chain: ChainClient,
}

impl RpcInterface {
    /// Perform the multiprocess handshake over `stream` and obtain the `Chain` interface.
    ///
    /// Bootstraps the `Init` capability, exchanges the thread map, then calls `makeChain`.
    pub(super) async fn connect(stream: tokio::net::UnixStream) -> Result<Self, Error> {
        let (reader, writer) = stream.into_split();
        let network = Box::new(twoparty::VatNetwork::new(
            reader.compat(),
            writer.compat_write(),
            rpc_twoparty_capnp::Side::Client,
            Default::default(),
        ));

        let mut rpc = RpcSystem::new(network, None);
        let init: InitClient = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
        let disconnector = rpc.get_disconnector();
        // The RpcSystem future must run for the whole session; drive it on the LocalSet.
        let rpc_handle = task::spawn_local(rpc);

        // Handshake: `construct` returns the server thread map, from which we make the thread
        // capability that every subsequent call's context must reference.
        let response = init.construct_request().send().promise.await?;
        let thread_map = response.get()?.get_thread_map()?;
        let response = thread_map.make_thread_request().send().promise.await?;
        let thread = response.get()?.get_result()?;

        // Obtain the `Chain` interface (makeChain @5 in Bitcoin Core PR #29409).
        let mut req = init.make_chain_request();
        req.get().get_context()?.set_thread(thread.clone());
        let response = req.send().promise.await?;
        let chain = response.get()?.get_result()?;

        Ok(Self {
            rpc_handle,
            disconnector,
            thread,
            chain,
        })
    }

    /// Height of the active chain tip (`Chain::getHeight`).
    pub(super) async fn tip_height(&self) -> Result<i32, Error> {
        let mut req = self.chain.get_height_request();
        req.get().get_context()?.set_thread(self.thread.clone());
        let response = req.send().promise.await?;
        Ok(response.get()?.get_result())
    }

    /// Hash of the active-chain block at `height` (`Chain::getBlockHash`).
    pub(super) async fn block_hash(&self, height: i32) -> Result<BlockHash, Error> {
        let mut req = self.chain.get_block_hash_request();
        req.get().get_context()?.set_thread(self.thread.clone());
        req.get().set_height(height);
        let response = req.send().promise.await?;
        Ok(BlockHash::from_slice(response.get()?.get_result()?)
            .expect("node must serve 32-byte block hashes"))
    }

    /// Full block at `height` on the branch ending at `tip_hash` (`Chain::findAncestorByHeight`
    /// with `wantData`). Returns [`Error::BlockNotFound`] if the node has no such block.
    pub(super) async fn block_at_height(
        &self,
        tip_hash: &BlockHash,
        height: i32,
    ) -> Result<Block, Error> {
        let mut req = self.chain.find_ancestor_by_height_request();
        req.get().get_context()?.set_thread(self.thread.clone());
        req.get().set_block_hash(tip_hash.as_ref());
        req.get().set_ancestor_height(height);
        req.get().get_ancestor()?.set_want_data(true);
        let response = req.send().promise.await?;
        let ancestor = response.get()?.get_ancestor()?;
        if !ancestor.get_found() {
            let height = u32::try_from(height).map_err(|_| Error::HeightConversion(height))?;
            return Err(Error::BlockNotFound { height });
        }
        let mut data = ancestor.get_data()?;
        Ok(Block::consensus_decode(&mut data)?)
    }

    /// Whether `ancestor_hash` is an ancestor of `tip_hash`, i.e. still in the branch ending at
    /// the node tip (`Chain::findAncestorByHash`). This is the "still in best chain" check.
    pub(super) async fn is_ancestor(
        &self,
        tip_hash: &BlockHash,
        ancestor_hash: &BlockHash,
    ) -> Result<bool, Error> {
        let mut req = self.chain.find_ancestor_by_hash_request();
        req.get().get_context()?.set_thread(self.thread.clone());
        req.get().set_block_hash(tip_hash.as_ref());
        req.get().set_ancestor_hash(ancestor_hash.as_ref());
        let response = req.send().promise.await?;
        Ok(response.get()?.get_result())
    }

    /// Lowest common ancestor between the branch ending at `tip_hash` and the one ending at
    /// `other_hash` (`Chain::findCommonAncestor`). `None` if they do not connect.
    pub(super) async fn common_ancestor(
        &self,
        tip_hash: &BlockHash,
        other_hash: &BlockHash,
    ) -> Result<Option<BlockId>, Error> {
        let mut req = self.chain.find_common_ancestor_request();
        req.get().get_context()?.set_thread(self.thread.clone());
        req.get().set_block_hash1(tip_hash.as_ref());
        req.get().set_block_hash2(other_hash.as_ref());
        req.get().get_ancestor()?.set_want_height(true);
        req.get().get_ancestor()?.set_want_hash(true);
        let response = req.send().promise.await?;
        let response = response.get()?;
        let ancestor = response.get_ancestor()?;
        if !ancestor.get_found() {
            return Ok(None);
        }
        let height = u32::try_from(ancestor.get_height())
            .map_err(|_| Error::HeightConversion(ancestor.get_height()))?;
        let hash =
            BlockHash::from_slice(ancestor.get_hash()?).expect("node must serve 32-byte hashes");
        Ok(Some(BlockId { height, hash }))
    }

    /// Whether the node has block data from `min_height` up to `tip_hash` (`Chain::hasBlocks`).
    /// Used as a pruning guard before fetching.
    pub(super) async fn has_blocks(
        &self,
        tip_hash: &BlockHash,
        min_height: i32,
    ) -> Result<bool, Error> {
        let mut req = self.chain.has_blocks_request();
        req.get().get_context()?.set_thread(self.thread.clone());
        req.get().set_block_hash(tip_hash.as_ref());
        req.get().set_min_height(min_height);
        let response = req.send().promise.await?;
        Ok(response.get()?.get_result())
    }

    /// Cleanly shut down the RPC session.
    pub(super) async fn disconnect(self) -> Result<(), Error> {
        self.disconnector.await?;
        // The RpcSystem task resolves once disconnected; its join result is not interesting.
        let _ = self.rpc_handle.await;
        Ok(())
    }
}
