#!/usr/bin/env bash
#
# Helper for the `ipc_emitter` example (experimental, multiprocess IPC).
#
# Runs the full setup from this crate's examples/README.md end to end: clone Bitcoin
# Core, check out PR #29409, build the node with IPC enabled, start it on signet with
# an IPC socket, and run the `ipc_emitter` example against it. Run it from the bdk repo.
#
# Assumes a Bitcoin Core checkout at ~/bitcoin and Core's default ~/.bitcoin datadir, so
# a signet node already synced there is reused. This is a proof-of-concept convenience
# script, not production tooling: read it before running. It clones and builds Bitcoin
# Core (long, disk-heavy) and starts a node that keeps running in the background.

set -euo pipefail

# The capnp compiler is needed both to build Core's IPC and for this crate's build.rs.
command -v capnp >/dev/null || {
  echo "capnp compiler not found; install 1.x ('apt install capnproto libcapnp-dev' or 'brew install capnp')." >&2
  exit 1
}

# 1. Clone Bitcoin Core, check out PR #29409, and build bitcoin-node with IPC enabled.
#    The build dir carries the PR number so it does not clash with a release `build/`.
[ -d "$HOME/bitcoin/.git" ] || git clone https://github.com/bitcoin/bitcoin "$HOME/bitcoin"
# Fetch the PR only if we do not already have it locally.
git -C "$HOME/bitcoin" show-ref --verify --quiet refs/heads/pr29409 ||
  git -C "$HOME/bitcoin" fetch origin pull/29409/head:pr29409
git -C "$HOME/bitcoin" checkout pr29409
cmake -B "$HOME/bitcoin/multiprocbuild-pr29409" -S "$HOME/bitcoin" -DENABLE_IPC=ON
cmake --build "$HOME/bitcoin/multiprocbuild-pr29409" -j"$(nproc)" --target bitcoin-node

# build.rs reads the Cap'n Proto schemas from this checkout.
export BITCOIN_CORE_SRC="$HOME/bitcoin"

# 2. Start the node on signet with an IPC socket, unless one is already running.
if [ ! -S "$HOME/.bitcoin/signet/node.sock" ]; then
  # -debug=ipc routes IPC (libmultiprocess) messages into the node's debug.log.
  nohup "$HOME/bitcoin/multiprocbuild-pr29409/bin/bitcoin-node" -signet -ipcbind=unix -debug=ipc >"$HOME/bitcoin/ipc-node.out" 2>&1 &
  echo "started bitcoin-node (pid $!); IPC logs in ~/.bitcoin/signet/debug.log; waiting for the IPC socket ..."
  until [ -S "$HOME/.bitcoin/signet/node.sock" ]; do sleep 1; done
fi

# 3. Run the example. The node may still be syncing, so it emits up to the current tip;
#    re-run this once it has synced past the example's birthday height.
export CORE_IPC_SOCKET="$HOME/.bitcoin/signet/node.sock"
cargo run -p bdk_bitcoind_rpc --example ipc_emitter --features ipc
