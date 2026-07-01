# PoC: block emitter over Bitcoin Core multiprocess IPC (bdk_bitcoind_rpc)

> **Update (schema source):** the sections below describe vendoring Bitcoin Core as a git subtree.
> That was superseded during implementation: Bitcoin Core is **not** vendored. `build.rs` instead
> reads the Cap'n Proto schemas from a Bitcoin Core checkout given by the `BITCOIN_CORE_SRC`
> environment variable (built from PR #29409). See `examples/README.md`. Everything else in the plan
> still holds.

## Context

`bdk_bitcoind_rpc` today sources blockchain data from bitcoind over JSON-RPC (`bitcoincore-rpc`). Its
`Emitter` (`crates/bitcoind_rpc/src/lib.rs`) is a synchronous, pull-based driver: you call `next_block()`
in a loop and it walks the chain forward, handles reorgs, and yields `BlockEvent { block, checkpoint }`
that the consumer applies to a `LocalChain` + `IndexedTxGraph`.

We want to prove out an alternative transport: Bitcoin Core's multiprocess IPC interface (Cap'n Proto over
a unix socket), the future-facing way to talk to a node. This PoC is the first step (branch
`feat/introduce-bdk-ipc-client`). Goal: a new, feature-gated `ipc` module inside the existing crate (not a
new crate) that provides a block-emitter-only path over IPC, mirroring the existing `Emitter` API and
producing the same `BlockEvent<Block>` output so downstream code is unchanged.

### Hard constraint discovered during research (must accept)

The `Chain` IPC interface is NOT in released Bitcoin Core. Stock Core (30.x / master) exposes only
`makeEcho` / `makeMining` / `makeRpc` over IPC - no `makeChain`, no `chain.capnp`. Getting blocks over IPC
requires Bitcoin Core PR #29409 ("multiprocess: Add capnp wrapper for Chain interface", ryanofsky,
currently open/unmerged), which adds `makeChain` + `chain.capnp` (`getHeight`, `getBlockHash`,
`findAncestorByHeight`, `findAncestorByHash`, `findCommonAncestor`, `hasBlocks`, plus a
`ChainNotifications` interface we will not use yet).

Reference implementation: `darosior/core_bdk_wallet`, specifically the up-to-date PR #2
(`ViniciusCestarii/core_bdk_wallet@update-29409`) - the repo's `master` is stale; PR #2 re-syncs the
schemas to the current state of Core PR #29409. It uses `capnp` 0.21 + `capnp-rpc` 0.21 + `tokio` 1.41 +
`tokio-util` 0.7 and applies blocks to `bdk_chain`. Its sync is hybrid: a poll loop on startup
(`get_block(tip_hash, h)` == `findAncestorByHeight` by height, `wantData=true`) followed by notification
callbacks for live blocks. Our poll-only PoC is exactly that startup loop - the ideal template. (PR #2
also fixes a handler-drop bug in the notification path; the general lesson applies: keep the ChainClient /
ThreadClient / RpcSystem alive for the emitter's whole lifetime, never in a temporary.)

### Decisions (confirmed with user)

- Poll-based emitter mirroring `Emitter::next_block()` with a synchronous public API (internally a
  current-thread tokio runtime + `block_on` per call). Defers the notification/push model.
- Deliverable = the feature-gated `ipc` module plus a runnable `examples/ipc_emitter.rs`, verified
  manually. No CI integration tests in this PoC.
- Scope: block emitter only - no mempool, no compact block filters.
- Schema source = a git subtree of Bitcoin Core PR #29409 (pins schema + node source together).
- Implement as small, incremental commits where every commit compiles on its own (use `todo!()` for
  deferred bodies). The plan document itself is committed first.
- ASCII-only in all source comments, docs, and this plan (no em-dashes / Unicode).
- The example targets signet, reusing the descriptors and birthday constants from
  `crates/bitcoind_rpc/examples/filter_iter.rs` (which is already a signet example).

## 1. Cargo.toml (`crates/bitcoind_rpc/Cargo.toml`)

Add a new `ipc` feature that pulls the async stack in optionally (default build stays lean):

```toml
[dependencies]
# ... existing bitcoin / bitcoincore-rpc / bdk_core ...
capnp      = { version = "0.21", optional = true }
capnp-rpc  = { version = "0.21", optional = true }
tokio      = { version = "1.41", optional = true, default-features = false, features = ["net", "rt", "macros", "io-util"] }
tokio-util = { version = "0.7",  optional = true, features = ["compat"] }

[build-dependencies]
capnpc = "0.21"   # build-deps cannot be optional; its use is gated inside build.rs

[features]
default = ["std"]
std   = ["bitcoin/std", "bdk_core/std"]
serde = ["bitcoin/serde", "bdk_core/serde"]
ipc   = ["std", "dep:capnp", "dep:capnp-rpc", "dep:tokio", "dep:tokio-util"]

[[example]]
name = "ipc_emitter"
required-features = ["ipc"]
```

A current-thread runtime only needs `rt` (not `rt-multi-thread`); `net` for `UnixStream`.

## 2. File layout (new files under `crates/bitcoind_rpc/`)

```
vendor/bitcoin-core/         # git subtree: bitcoin/bitcoin @ PR #29409 (pinned schema + node source)
build.rs                     # feature-gated capnp codegen (reads schemas from the subtree)
src/lib.rs                   # add: #[cfg(feature = "ipc")] pub mod ipc;   (reuse existing BlockEvent)
src/ipc/mod.rs               # public surface: IpcEmitter, Error; include! generated capnp modules
src/ipc/rpc.rs               # connect/bootstrap/makeChain + async Chain call wrappers (only BDK's methods)
src/ipc/emitter.rs           # IpcEmitter + next_block() state machine
examples/ipc_emitter.rs      # runnable PoC (mirrors examples/filter_iter.rs)
examples/README.md           # ALL steps to run the IPC example end-to-end (see "Example README" below)
```

Schema source = a git subtree of Bitcoin Core PR #29409. Commit 2 runs
`git subtree add --prefix crates/bitcoind_rpc/vendor/bitcoin-core https://github.com/bitcoin/bitcoin
<pr29409-ref> --squash`, pinning the exact Core commit whose `.capnp` schemas we compile against and whose
source you build the node from - this eliminates schema/node drift. `build.rs` points capnpc at
`vendor/bitcoin-core/src/ipc/capnp/` and compiles `init.capnp` + `chain.capnp` (their transitive imports -
`c++`, `proxy`, `common`, `handler`, `echo`, `mining`, `rpc` - are all present in the subtree, so this
"just works" with no manual copying or trimming). We still only use BDK's ~6 Chain methods in
`src/ipc/rpc.rs`; codegen simply also produces the unused bindings.

Trade-off (flagged): a full-Core subtree is a large tree/history to carry in the bdk repo. It is the price
of exact reproducibility plus being able to build the matching node from in-repo source. Fallback if that
footprint is unwanted: drop the subtree and instead vendor a minimal, hand-trimmed `schema/` (4 files:
`c++.capnp` + `proxy.capnp` verbatim, plus `init.capnp`/`chain.capnp` trimmed to `construct`+`makeChain`
and the 6 emitter methods + `FoundBlock*` structs, ordinal gaps stubbed). Trimming is wire-safe only if
the file `@0x<id>` + type names are preserved and used ordinals/types match exactly (type id =
MD5(fileId + name)); verify with `capnp compile -ocapnp`.

## 3. build.rs (gated codegen)

Only invoke the capnp compiler when the feature is on, so the default build never needs the `capnp` binary:

```rust
fn main() {
    if std::env::var_os("CARGO_FEATURE_IPC").is_none() {
        return; // lean default build
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor/bitcoin-core/src/ipc/capnp"); // the subtree
    println!("cargo:rerun-if-changed=vendor/bitcoin-core/src/ipc/capnp");
    capnpc::CompilerCommand::new()
        .src_prefix(&dir)
        .import_path(&dir) // resolve `import "proxy.capnp"` etc.
        .file(dir.join("init.capnp"))
        .file(dir.join("chain.capnp")) // + transitive imports resolved from the subtree
        .run()
        .expect("capnp codegen failed - is the `capnp` binary in PATH?");
}
```

Note: `capnpc` shells out to the external `capnp` C++ binary (it does not bundle it). Anyone building
`--features ipc` must have `capnp` (1.x) installed. Generated files land in `OUT_DIR` and are pulled in via
`include!(concat!(env!("OUT_DIR"), "/chain_capnp.rs"))` etc. in `src/ipc/mod.rs`, as sibling modules
matching the reference layout.

## 4. IpcEmitter design (`src/ipc/`)

Reuse the crate's existing `BlockEvent<Block>` verbatim as the output type (its `block_height()`,
`block_hash()`, `connected_to()` all work unchanged), and the `bdk_core` `CheckPoint`/`BlockId` types.

```rust
pub struct IpcEmitter {
    rt: tokio::runtime::Runtime,        // current-thread
    local: tokio::task::LocalSet,       // drives the spawned RpcSystem (see bridge note)
    rpc: Rc<RpcInterface>,              // ChainClient + ThreadClient + Disconnector (!Send caps)
    last_cp: CheckPoint<BlockHash>,     // == Emitter::last_cp
    start_height: u32,                  // == Emitter::start_height
    last_height: Option<u32>,           // cursor; analog of Emitter::last_block
}

impl IpcEmitter {
    pub fn new(socket_path: impl AsRef<Path>, last_cp: CheckPoint<BlockHash>, start_height: u32)
        -> Result<Self, Error>;         // connect + Init.construct(threadMap) + makeChain
    pub fn next_block(&mut self) -> Result<Option<BlockEvent<Block>>, Error>;
}
```

Sync-over-async bridge (load-bearing detail): the capnp caps are `!Send` and the `RpcSystem` future must
be continuously driven. Use `new_current_thread()` + a `LocalSet`; `spawn_local` the `RpcSystem` during
connect; every method runs `self.local.block_on(&self.rt, async { ... })` (must be `LocalSet::block_on`,
NOT bare `Runtime::block_on`, or Chain calls hang). Every Chain request must set its `Proxy.Context.thread`
to the handshake `ThreadClient` (`req.get().get_context()?.set_thread(thread)`), or calls hang. The emitter
is single-thread-bound (do not `Send` it; calling from inside another tokio runtime panics - it owns its
runtime).

`next_block()` reproduces `poll`/`poll_once` (`crates/bitcoind_rpc/src/lib.rs`, the `PollResponse` state
machine) but walks by height pinned to the node tip hash (there is no `nextblockhash` over IPC):

1. Read node tip: `getHeight` -> `tip_height`, `getBlockHash(tip_height)` -> `tip_hash` (re-read for
   stability, like `mempool_at`, to avoid a mid-reorg snapshot).
2. `next_height = last_height.map(|h| h+1).unwrap_or(0).max(start_height)`.
3. If `next_height > tip_height` -> return `Ok(None)` (at tip).
4. Connection/reorg check before emitting: if we have emitted before, verify `last_cp.hash()` is still an
   ancestor of `tip_hash` via `findAncestorByHash`. If not -> `findCommonAncestor`, rewind `last_cp` down
   `last_cp.iter()` to the agreement height, set `last_height`, lower `start_height` if below it (mirrors
   `AgreementFound`), then `continue`. On the very first call (`last_height == None`), scan `last_cp.iter()`
   for the first checkpoint that is an ancestor of `tip_hash` to establish the agreement point; if none,
   force genesis (mirrors `AgreementPointNotFound`).
5. Fetch + emit: `findAncestorByHeight(tip_hash, next_height, wantData=true)` -> `.data` bytes ->
   `Block::consensus_decode`. Then `last_cp = last_cp.push(next_height, hash)`, `last_height =
   Some(next_height)`, return `BlockEvent { block, checkpoint }`.

RPC -> IPC method mapping:

| RPC Emitter                         | IPC Chain                                                       |
|-------------------------------------|----------------------------------------------------------------|
| `get_block_count`                   | `getHeight @1`                                                  |
| `get_block_hash(h)`                 | `getBlockHash @2`                                              |
| forward walk via `nextblockhash`    | `findAncestorByHeight @9 (tip_hash, h, wantData)` - by height   |
| `confirmations < 0` (not best chain)| `findAncestorByHash @10 (tip_hash, our_hash).result == false`   |
| agreement search over `last_cp`     | `findAncestorByHash` per cp, or `findCommonAncestor @11`        |
| `get_block(hash)` payload           | `findAncestorByHeight @9 (..., wantData=true).data` -> decode   |
| pruning guard                       | `hasBlocks @14`                                                 |

`Error` enum (`src/ipc/mod.rs`): `Io`, `Capnp`, `NotInSchema`, `HeightConversion(i32)`,
`BlockNotFound { height }`, `BlockDecode`, `CheckpointPush`, `ReorgTooDeep` - with `From` impls plus
`Display`/`std::error::Error`.

## 5. Example (`examples/ipc_emitter.rs`)

Mirror `examples/filter_iter.rs`, which is already a signet example - reuse its constants verbatim:

- `EXTERNAL` / `INTERNAL` = the same `tr([83737d5e/86'/1'/0']tpub.../0/*)` and `/1/*` descriptors.
- `NETWORK = Network::Signet`, `SPK_COUNT = 25`.
- Birthday: `START_HEIGHT = 205_000`, `START_HASH =
  "0000002bd0f82f8c0c0f1e19128f84c938763641dba85c44bdb6aed1678d16cb"`.

Build `LocalChain::from_genesis(genesis_block(NETWORK).block_hash())` + `IndexedTxGraph` with a
`KeychainTxOutIndex` (two descriptors), insert the birthday via `chain.insert_block(START_HEIGHT,
START_HASH.parse()?)?` (avoids replaying ~205k blocks), read the socket path from an env var
(`CORE_IPC_SOCKET`, e.g. `<datadir>/signet/node.sock` - no RPC URL/cookie needed), construct
`IpcEmitter::new(socket, chain.tip(), START_HEIGHT)`, then loop:

```rust
while let Some(ev) = emitter.next_block()? {
    let h = ev.block_height();
    chain.apply_update(ev.checkpoint)?;
    let cs = graph.apply_block_relevant(&ev.block, h);
    if !cs.is_empty() {
        println!("Matched block {h}");
    }
}
```

Print final tip + canonical-view balance/UTXOs (copy the `canonical_view` block from `filter_iter.rs`).
Start the file with `#![allow(clippy::print_stdout, clippy::print_stderr)]` (workspace denies these).
Keep comments ASCII-only.

## Implementation strategy: incremental, self-compiling commits

Land this as a sequence of small commits where every commit compiles on its own - both the lean default
build (`cargo build -p bdk_bitcoind_rpc`) and the feature build (`cargo check -p bdk_bitcoind_rpc
--features ipc`; needs the `capnp` binary + the subtree, but no running node). Use the `todo!()` macro for
not-yet-implemented bodies so intermediate commits type-check and compile while deferring logic. Planned
commits:

1. docs: add the IPC PoC plan. Commit this plan document into the repo (e.g.
   `crates/bitcoind_rpc/docs/ipc_poc_plan.md`) as the very first commit, so the agreed design is tracked
   alongside the work. No code, compiles trivially.
2. vendor: add Bitcoin Core PR #29409 as a subtree. `git subtree add --prefix
   crates/bitcoind_rpc/vendor/bitcoin-core https://github.com/bitcoin/bitcoin <pr29409-ref> --squash`.
   No Rust change, the crate still compiles. Record the pinned ref in the subtree commit message.
3. build plumbing. `Cargo.toml` (`ipc` feature, optional deps, `capnpc` build-dep, `[[example]]`), gated
   `build.rs` (reads schemas from the subtree), and `#[cfg(feature = "ipc")] pub mod ipc;` wiring a
   skeleton `src/ipc/mod.rs` that `include!`s the generated capnp modules and declares stub
   `pub struct IpcEmitter` / `pub enum Error`. Compiles both ways; proves codegen against the subtree works.
4. Error enum + `src/ipc/rpc.rs`. The connect/bootstrap (`UnixStream` -> `RpcSystem` -> `Init.construct` ->
   `makeChain`) and the async Chain wrappers (`get_tip_height`, `get_block_hash`, `get_block_by_height`,
   `is_ancestor`, `common_ancestor`, optional `has_blocks`). Bodies may start as `todo!()`; the commit
   still compiles.
5. `src/ipc/emitter.rs`. `IpcEmitter` fields + `new()` (runtime + `LocalSet` + connect) and the
   `next_block()` poll/reorg state machine calling the `rpc.rs` wrappers; export from `mod.rs`.
6. `examples/ipc_emitter.rs` + `examples/README.md`. The runnable PoC + `[[example]]` entry, plus the
   README with all run steps (linked from the example's doc comment); compiles under `--features ipc`.
7. docs. Module-doc block flagging the crate/feature as experimental and requiring a PR #29409 node, plus a
   short vendor/build note.

Keep each commit message scoped to its step; do not mix the subtree, transport, and emitter logic in one
commit.

## Example README (`crates/bitcoind_rpc/examples/README.md`)

All the steps required to run the example live in this README (committed with the example in commit 6, and
linked from the top-of-file doc comment in `examples/ipc_emitter.rs`). It is also the manual verification
procedure. Contents:

Requirements (the subtree pins the source but does not remove any of these):
- `capnp` 1.x installed (`apt install capnproto` / `brew install capnp`) - needed both to build Core's IPC
  and for our crate's `build.rs` codegen under `--features ipc`.
- A PR #29409 `bitcoin-node` built with IPC, from the in-repo subtree so it matches our schemas.

Steps (signet):
1. Build the node from the subtree:
   ```
   cd crates/bitcoind_rpc/vendor/bitcoin-core
   cmake -B multiprocbuild/ -DENABLE_IPC=ON && cmake --build multiprocbuild/ -j"$(nproc)"
   ```
2. Run the node on signet with an IPC socket and let it sync (IBD over P2P from the default signet peers -
   this takes a while the first time):
   `./multiprocbuild/bin/bitcoin-node -signet -datadir="$PWD/dd" -ipcbind=unix`
   -> socket at `$PWD/dd/signet/node.sock`. Wait until it is past the example's `START_HEIGHT` birthday.
3. Fund a descriptor address from a signet faucet (e.g. https://signetfaucet.com) so there is something to
   match. If needed, adjust `START_HEIGHT`/`START_HASH` to a signet block at/just below your funding
   height so the emitter does not replay from genesis.
4. Run the example: `export CORE_IPC_SOCKET="$PWD/dd/signet/node.sock"` then
   `cargo run -p bdk_bitcoind_rpc --example ipc_emitter --features ipc`. Confirm blocks emit from the
   birthday to the tip and funded heights print `Matched block H`, ending with the faucet UTXO in the
   balance/UTXO print-out.
5. Parity check (recommended): diff the `(height, hash)` + matched-height sequence against the existing RPC
   `Emitter` on the same signet node; they must be identical.
6. Reorg handling (optional): signet reorgs are rare and not forceable, so the reorg walk-back cannot be
   triggered on demand here. To exercise it deterministically, optionally run the same example binary
   against a throwaway regtest node (`-regtest`, mine blocks, `bitcoin-cli -regtest invalidateblock
   <hash>`, mine a longer branch) - the emitter is network-agnostic, so only `NETWORK` + the birthday
   consts change.

## Risks / explicitly deferred

- Schema drift vs unmerged PR #29409 - mitigated by the subtree: schemas and node build from one pinned
  commit. When PR #29409 advances, `git subtree pull` to update (and regenerate codegen).
- Signet demo caveats - first run does a full signet IBD (slow, needs peers); the birthday checkpoint keeps
  emission cheap but the node must still sync the chain. Reorgs cannot be forced on signet, so the reorg
  walk-back is verified out-of-band on a throwaway regtest node (README step 6), not by the signet run.
- Subtree footprint - vendoring full Core adds a large tree/history to the bdk repo and slows CI clones.
  Accepted for reproducibility; the section-2 fallback (minimal hand-trimmed `schema/`, wire-safe only if
  file `@0x` ids + type names are preserved and verified via `capnp compile -ocapnp`) is the escape hatch.
- External `capnp` binary build dep - the `build.rs` codegen path needs the `capnp` compiler installed
  (only for `--features ipc`; default build unaffected). Alternative (what PR #2 does): commit the
  generated `*_capnp.rs` files and drop `build.rs` entirely - removes the build-time binary requirement at
  the cost of checked-in generated code that must be regenerated on schema bumps. The plan leans `build.rs`
  for freshness, but committed-code is a legitimate PoC shortcut.
- Async-in-sync - must use `LocalSet::block_on`; caps are `!Send`; `IpcEmitter` owns its runtime and must
  be called from non-async context. Hold the `Disconnector` for best-effort cleanup on `Drop`.
- Deferred (out of scope): mempool emission, compact block filters, the notification/push model
  (`handleNotifications`), no-std (ipc implies std), and CI integration tests (manual verification only).
