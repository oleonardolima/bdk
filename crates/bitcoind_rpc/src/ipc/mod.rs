//! Block emitter over Bitcoin Core's multiprocess IPC (Cap'n Proto) interface.
//!
//! This is an experimental, feature-gated alternative to the JSON-RPC [`Emitter`](crate::Emitter).
//! It talks to `bitcoin-node` over the multiprocess IPC unix socket instead of JSON-RPC, and is
//! meant to mirror the JSON-RPC emitter: a synchronous, poll-based [`IpcEmitter::next_block`] loop
//! that yields the same [`BlockEvent`](crate::BlockEvent) values.
//!
//! Requires a node built from Bitcoin Core PR #29409 (which exposes the `Chain` interface over
//! IPC) and the `capnp` compiler at build time. See `examples/README.md`.

// Rust bindings generated at build time from the Bitcoin Core Cap'n Proto schemas vendored under
// `vendor/bitcoin-core` (see `build.rs`). The generated code is not written to satisfy this
// crate's lints, so relax them for the whole module. Every schema is included as a flat
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

/// Errors returned by the IPC block emitter.
//
// Skeleton: variants are added in a later commit alongside the transport and emitter.
#[derive(Debug)]
pub enum Error {}

/// Poll-based block emitter over Bitcoin Core's multiprocess IPC interface.
//
// Skeleton: fields, `new`, and `next_block` are added in later commits.
pub struct IpcEmitter {}
