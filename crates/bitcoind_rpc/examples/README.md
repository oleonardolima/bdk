# Example bitcoind RPC sync

### Simple Signet Test with FilterIter

1. Start local signet bitcoind. (~8 GB space required)
   ```
    mkdir -p /tmp/signet/bitcoind
    bitcoind -signet -server -fallbackfee=0.0002 -blockfilterindex -datadir=/tmp/signet/bitcoind -daemon
    tail -f /tmp/signet/bitcoind/signet/debug.log
   ```
   Watch debug.log and wait for bitcoind to finish syncing.

2. Set bitcoind env variables.
   ```
   export RPC_URL=127.0.0.1:38332
   export RPC_COOKIE=/tmp/signet/bitcoind/signet/.cookie
   ```
3. Run `filter_iter` example.
   ```
   cargo run -p bdk_bitcoind_rpc --example filter_iter
   ```

### `ipc_emitter` and `ipc_filter_iter` (experimental, multiprocess IPC)

`ipc_emitter` syncs the same BDK `LocalChain` + `IndexedTxGraph` by emitting blocks from Bitcoin
Core over its multiprocess IPC (Cap'n Proto) interface instead of JSON-RPC. It mirrors `filter_iter`
and reuses its signet descriptors and birthday.

`ipc_filter_iter` does the same sync via BIP158 filters over IPC, with the filter matching running
node-side (`Chain::blockFilterMatchesAny`): filters are never downloaded and only matching blocks
cross the socket. It additionally requires the node to run with `-blockfilterindex=1`.

This is a proof of concept. The `Chain` IPC interface is not in released Bitcoin Core: it requires a
node built from Bitcoin Core PR #29409. Bitcoin Core is not vendored here; you clone and build it
yourself, and point this crate's `build.rs` at that checkout. The examples are gated behind the
`ipc` cargo feature.

Requirements:

- The `capnp` compiler, version 1.x (`apt install capnproto libcapnp-dev` or `brew install capnp`).
  It is needed both to build Bitcoin Core's IPC and for this crate's `build.rs` codegen under
  `--features ipc`.
- A Bitcoin Core checkout built from PR #29409, exposed to `build.rs` via the `BITCOIN_CORE_SRC`
  environment variable (so the generated bindings match the node's schemas).

Steps (signet):

1. Clone Bitcoin Core to `~/bitcoin`, check out PR #29409, and build the node with IPC enabled:
   ```
   git clone https://github.com/bitcoin/bitcoin ~/bitcoin
   git -C ~/bitcoin fetch origin pull/29409/head:pr29409
   git -C ~/bitcoin checkout pr29409
   cmake -B ~/bitcoin/build-pr29409 -S ~/bitcoin -DENABLE_IPC=ON
   cmake --build ~/bitcoin/build-pr29409 -j"$(nproc)"
   ```

2. Run the node on signet with an IPC socket and the BIP158 filter index, using Core's standard
   `~/.bitcoin` datadir, and let it sync (initial block download over P2P takes a while the first
   time):
   ```
   ~/bitcoin/build-pr29409/bin/bitcoin-node -signet -ipcbind=unix -blockfilterindex=1
   ```
   This creates the socket at `~/.bitcoin/signet/node.sock`. Wait until the node is past the
   examples' `START_HEIGHT` birthday. `-blockfilterindex=1` is only needed by `ipc_filter_iter`;
   while the index is still building, that example fails with a transient "filter unavailable"
   error. To run the node in the background instead, add `-daemonwait`: it daemonizes (logging to
   `debug.log`) and returns once initialization is done, i.e. once the socket exists.

3. Run the examples (from the bdk repo). `BITCOIN_CORE_SRC` must be set so `build.rs` can find the
   schemas; `CORE_IPC_SOCKET` points at the running node's socket:
   ```
   export BITCOIN_CORE_SRC=~/bitcoin
   export CORE_IPC_SOCKET=~/.bitcoin/signet/node.sock
   cargo run -p bdk_bitcoind_rpc --example ipc_emitter --features ipc
   cargo run -p bdk_bitcoind_rpc --example ipc_filter_iter --features ipc
   ```
   `ipc_emitter` emits every block from the birthday to the tip; `ipc_filter_iter` scans the same
   range but only downloads matching blocks. Heights with transactions relevant to the example
   descriptors print `Matched block H`, and any UTXOs appear in the final output of both.

Verification notes:

- Parity: run the JSON-RPC `Emitter` (or the `filter_iter` example) against the same node and check
  that the sequence of emitted `(height, hash)` and matched heights is identical.
- Reorgs: signet reorgs cannot be forced, so the reorg walk-back is not exercised by the signet run.
  To test it deterministically, run the same example against a throwaway regtest node instead
  (`-regtest`, mine blocks, `bitcoin-cli -regtest invalidateblock <hash>`, then mine a longer
  branch). Both examples are network-agnostic, so only `NETWORK` and the birthday constants change.
