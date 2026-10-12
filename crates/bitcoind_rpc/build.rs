//! Build script for the `bdk_bitcoind_rpc` crate.
//!
//! When the experimental `ipc` feature is enabled, this generates Rust bindings from Bitcoin
//! Core's Cap'n Proto schemas. The schemas are not vendored: point the `BITCOIN_CORE_SRC`
//! environment variable at a Bitcoin Core source checkout built from PR #29409. The `capnp`
//! compiler must also be installed. See `examples/README.md` for the full setup.
//!
//! For the default build (no `ipc` feature) this script is a no-op, so the crate needs no capnp
//! toolchain, no Bitcoin Core checkout, and stays async-free.

fn main() {
    // Cargo sets CARGO_FEATURE_<NAME> for each active feature. Only run codegen for `ipc`.
    if std::env::var_os("CARGO_FEATURE_IPC").is_none() {
        return;
    }

    println!("cargo:rerun-if-env-changed=BITCOIN_CORE_SRC");

    let core_src = std::env::var_os("BITCOIN_CORE_SRC")
        .map(std::path::PathBuf::from)
        .expect(
            "the `ipc` feature requires BITCOIN_CORE_SRC to point at a Bitcoin Core source \
             checkout built from PR #29409; see crates/bitcoind_rpc/examples/README.md",
        );

    let ipc = core_src.join("src/ipc");
    let capnp_dir = ipc.join("capnp");
    // libmultiprocess ships `/mp/proxy.capnp`, imported by every Core schema.
    let mp_include = ipc.join("libmultiprocess/include");
    let mp_dir = mp_include.join("mp");

    assert!(
        capnp_dir.join("chain.capnp").exists(),
        "BITCOIN_CORE_SRC={} does not contain the Cap'n Proto schemas at src/ipc/capnp/ (need a \
         Bitcoin Core checkout built from PR #29409); see examples/README.md",
        core_src.display(),
    );

    println!("cargo:rerun-if-changed={}", capnp_dir.display());
    println!("cargo:rerun-if-changed={}", mp_dir.display());

    // capnpc only generates Rust for the files it is explicitly given, but the generated code
    // cross-references every imported schema, so all of them must be listed. We only *use* the
    // `Init` bootstrap and the `Chain` interface; the rest are compiled so the bindings resolve.
    // The two `src_prefix`es flatten both the Core `capnp/` dir and the libmultiprocess `mp/` dir
    // so every module lands as `<name>_capnp` under `crate::ipc::capnp_gen`.
    capnpc::CompilerCommand::new()
        .import_path(&capnp_dir)
        .import_path(&mp_include)
        .src_prefix(&capnp_dir)
        .src_prefix(&mp_dir)
        .default_parent_module(vec!["ipc".to_string(), "capnp_gen".to_string()])
        .file(capnp_dir.join("init.capnp"))
        .file(capnp_dir.join("chain.capnp"))
        .file(capnp_dir.join("common.capnp"))
        .file(capnp_dir.join("handler.capnp"))
        .file(capnp_dir.join("echo.capnp"))
        .file(capnp_dir.join("mining.capnp"))
        .file(capnp_dir.join("rpc.capnp"))
        .file(mp_dir.join("proxy.capnp"))
        .run()
        .expect("capnp codegen failed (is the `capnp` compiler installed? see examples/README.md)");
}
