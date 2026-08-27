//! Block emitter and BIP158 filter scanner over Bitcoin Core's multiprocess IPC (Cap'n Proto)
//! interface.
//!
//! This is an experimental, feature-gated alternative to the JSON-RPC [`Emitter`](crate::Emitter)
//! and [`FilterIter`](crate::bip158::FilterIter). It talks to `bitcoin-node` over the multiprocess
//! IPC unix socket instead of JSON-RPC:
//!
//! - [`IpcEmitter`](crate::ipc::IpcEmitter) mirrors the JSON-RPC emitter: a synchronous, poll-based
//!   [`next_block`](crate::ipc::IpcEmitter::next_block) loop that yields the same
//!   [`BlockEvent`](crate::BlockEvent) values.
//! - [`IpcFilterIter`](crate::ipc::IpcFilterIter) mirrors the JSON-RPC `FilterIter`, but with a
//!   twist: BIP158 filter matching happens *node-side* (`Chain::blockFilterMatchesAny`), so filters
//!   are never downloaded and only matching blocks cross the socket. Requires
//!   `-blockfilterindex=1`.
//!
//! It is a proof of concept: no mempool emission, and each `blockFilterMatchesAny` call sends the
//! whole script set for one block (batching/pipelining across heights is future work).
//!
//! # Experimental: requires a patched Bitcoin Core
//!
//! The `Chain` IPC interface used here is **not** in released Bitcoin Core. It requires a node
//! built from [Bitcoin Core PR #29409](https://github.com/bitcoin/bitcoin/pull/29409), which adds
//! the `Chain` Cap'n Proto wrapper. Bitcoin Core is not vendored: `build.rs` reads the schemas from
//! a Bitcoin Core checkout whose path is given by the `BITCOIN_CORE_SRC` environment variable.
//!
//! Building with the `ipc` feature therefore needs the `capnp` compiler (1.x) installed and
//! `BITCOIN_CORE_SRC` set, and running needs a `bitcoin-node` built from that same checkout with
//! `-ipcbind=unix`. See `examples/README.md` for the full setup.

// Rust bindings generated at build time from the Bitcoin Core Cap'n Proto schemas in the checkout
// pointed to by BITCOIN_CORE_SRC (see `build.rs`). The generated code is not written to satisfy
// this crate's lints, so relax them for the whole module. Every schema is included as a flat
// `<name>_capnp` module under `crate::ipc::capnp_gen`, which is the path the generated cross-
// references use (note: `proxy.capnp` is emitted into an `mp/` subdirectory but still referenced
// flat, hence the explicit include path below).
#[allow(
    clippy::all,
    clippy::pedantic,
    dead_code,
    missing_docs,
    non_camel_case_types,
    non_snake_case,
    unreachable_pub,
    unused
)]
pub(crate) mod capnp_gen {
    pub mod proxy_capnp {
        include!(concat!(env!("OUT_DIR"), "/mp/proxy_capnp.rs"));
    }
    pub mod common_capnp {
        include!(concat!(env!("OUT_DIR"), "/common_capnp.rs"));
    }
    pub mod handler_capnp {
        include!(concat!(env!("OUT_DIR"), "/handler_capnp.rs"));
    }
    pub mod echo_capnp {
        include!(concat!(env!("OUT_DIR"), "/echo_capnp.rs"));
    }
    pub mod mining_capnp {
        include!(concat!(env!("OUT_DIR"), "/mining_capnp.rs"));
    }
    pub mod rpc_capnp {
        include!(concat!(env!("OUT_DIR"), "/rpc_capnp.rs"));
    }
    pub mod chain_capnp {
        include!(concat!(env!("OUT_DIR"), "/chain_capnp.rs"));
    }
    pub mod init_capnp {
        include!(concat!(env!("OUT_DIR"), "/init_capnp.rs"));
    }
}

mod emitter;
mod filter_iter;
mod rpc;

pub use emitter::IpcEmitter;
pub use filter_iter::IpcFilterIter;
// Re-exported so IPC filter events read the same as the JSON-RPC `FilterIter` ones.
pub use crate::bip158::Event;

/// Errors returned by the IPC block emitter and filter scanner.
#[derive(Debug)]
pub enum Error {
    /// I/O error connecting to or communicating over the IPC socket.
    Io(std::io::Error),
    /// A Cap'n Proto / capnp-rpc level error.
    Capnp(capnp::Error),
    /// The node returned a block height that does not fit in a `u32`.
    HeightConversion(i32),
    /// `findAncestorByHeight` did not return block data for a height we expected to exist
    /// (e.g. the node pruned the block or moved out from under us).
    BlockNotFound {
        /// The height that could not be fetched.
        height: u32,
    },
    /// Failed to decode a block served by the node.
    BlockDecode(bitcoin::consensus::encode::Error),
    /// A `(height, hash)` could not be pushed onto the checkpoint chain.
    CheckpointPush,
    /// The node and our checkpoint chain share no common ancestor (should never happen against
    /// a node on the same network).
    ReorgTooDeep,
    /// The node has no BASIC (BIP158) block filter index; start `bitcoin-node` with
    /// `-blockfilterindex=1`.
    NoBlockFilterIndex,
    /// The filter for the block at `height` is not available. This is transient while the node's
    /// filter index is still building; retry once the index reaches the scanned range.
    FilterUnavailable {
        /// Height whose filter could not be queried.
        height: u32,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Capnp(e) => write!(f, "cap'n proto error: {e}"),
            Error::HeightConversion(h) => write!(f, "node returned invalid block height: {h}"),
            Error::BlockNotFound { height } => write!(f, "node has no block at height {height}"),
            Error::BlockDecode(e) => write!(f, "failed to decode block from node: {e}"),
            Error::CheckpointPush => write!(f, "failed to extend the checkpoint chain"),
            Error::ReorgTooDeep => write!(f, "no common ancestor with the node's chain"),
            Error::NoBlockFilterIndex => write!(
                f,
                "node has no BASIC block filter index (start bitcoin-node with -blockfilterindex=1)"
            ),
            Error::FilterUnavailable { height } => write!(
                f,
                "block filter unavailable at height {height} (index still building?)"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<capnp::Error> for Error {
    fn from(e: capnp::Error) -> Self {
        Error::Capnp(e)
    }
}

impl From<bitcoin::consensus::encode::Error> for Error {
    fn from(e: bitcoin::consensus::encode::Error) -> Self {
        Error::BlockDecode(e)
    }
}
