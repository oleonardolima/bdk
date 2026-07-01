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

### `ipc_emitter` (experimental, multiprocess IPC)

`ipc_emitter` syncs the same BDK `LocalChain` + `IndexedTxGraph` by emitting blocks from Bitcoin
Core over its multiprocess IPC (Cap'n Proto) interface instead of JSON-RPC. It mirrors `filter_iter`
and reuses its signet descriptors and birthday.

This is a proof of concept. The `Chain` IPC interface is not in released Bitcoin Core: it requires a
node built from Bitcoin Core PR #29409. Bitcoin Core is not vendored here; you clone and build it
yourself, and point this crate's `build.rs` at that checkout. The example is gated behind the `ipc`
cargo feature.

Requirements:

- The `capnp` compiler, version 1.x (`apt install capnproto libcapnp-dev` or `brew install capnp`).
  It is needed both to build Bitcoin Core's IPC and for this crate's `build.rs` codegen under
  `--features ipc`.
- A Bitcoin Core checkout built from PR #29409, exposed to `build.rs` via the `BITCOIN_CORE_SRC`
  environment variable (so the generated bindings match the node's schemas).

Automated path: `examples/ipc_setup.sh` runs steps 1, 2, and 4 below end to end (clone
and build Core, start the node, run the example). It clones/builds under
`~/bitcoin` and uses Core's standard `~/.bitcoin` datadir, so a signet node already synced
there is reused (edit the script for other locations); that node must not already be
running against the datadir when the script starts its own. You still fund an address
yourself (step 3). Read the script before running it. The manual steps follow.

Steps (signet):

1. Clone Bitcoin Core, check out PR #29409, and build the node with IPC enabled:
   ```
   git clone https://github.com/bitcoin/bitcoin
   cd bitcoin
   git fetch origin pull/29409/head:pr29409 && git checkout pr29409
   cmake -B multiprocbuild-pr29409/ -DENABLE_IPC=ON
   cmake --build multiprocbuild-pr29409/ -j"$(nproc)"
   export BITCOIN_CORE_SRC="$PWD"   # build.rs reads the .capnp schemas from here
   ```

2. Run the node on signet with an IPC socket and let it sync (initial block download over P2P takes
   a while the first time):
   ```
   ./multiprocbuild-pr29409/bin/bitcoin-node -signet -datadir="$PWD/dd" -ipcbind=unix
   ```
   This creates the socket at `$PWD/dd/signet/node.sock`. Wait until the node is past the example's
   `START_HEIGHT` birthday.

3. Fund one of the example's descriptor addresses from a signet faucet (for example
   https://signetfaucet.com) so there is something to match. If needed, adjust `START_HEIGHT` /
   `START_HASH` in `ipc_emitter.rs` to a signet block at or just below your funding height.

4. Run the example (from the bdk repo). `BITCOIN_CORE_SRC` must be set so `build.rs` can find the
   schemas; `CORE_IPC_SOCKET` points at the running node's socket:
   ```
   export BITCOIN_CORE_SRC="/path/to/bitcoin"       # the checkout from step 1
   export CORE_IPC_SOCKET="/path/to/bitcoin/dd/signet/node.sock"
   cargo run -p bdk_bitcoind_rpc --example ipc_emitter --features ipc
   ```
   Blocks are emitted from the birthday to the tip; funded heights print `Matched block H` and the
   faucet UTXO appears in the final balance/UTXO output.

Verification notes:

- Parity: run the JSON-RPC `Emitter` (or the `filter_iter` example) against the same node and check
  that the sequence of emitted `(height, hash)` and matched heights is identical.
- Reorgs: signet reorgs cannot be forced, so the reorg walk-back is not exercised by the signet run.
  To test it deterministically, run the same example against a throwaway regtest node instead
  (`-regtest`, mine blocks, `bitcoin-cli -regtest invalidateblock <hash>`, then mine a longer
  branch). The emitter is network-agnostic, so only `NETWORK` and the birthday constants change.
