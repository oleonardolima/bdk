#![allow(clippy::print_stdout, clippy::print_stderr)]
use std::time::Instant;

use bdk_bitcoind_rpc::ipc::IpcEmitter;
use bdk_chain::bitcoin::{constants::genesis_block, secp256k1::Secp256k1, Network};
use bdk_chain::indexer::keychain_txout::KeychainTxOutIndex;
use bdk_chain::local_chain::LocalChain;
use bdk_chain::miniscript::Descriptor;
use bdk_chain::{ConfirmationBlockTime, IndexedTxGraph, Merge};
use bdk_testenv::anyhow::{self, Context};

// This example shows how BDK chain and tx-graph structures are updated by emitting blocks from
// Bitcoin Core over its multiprocess IPC (Cap'n Proto) interface, instead of JSON-RPC. It mirrors
// the `filter_iter` example and reuses its signet descriptors and birthday.
//
// It requires a node built from Bitcoin Core PR #29409, run with `-ipcbind=unix`. See the README
// in this directory for the full setup. Point CORE_IPC_SOCKET at the node's `node.sock`.
//
// Usage: `CORE_IPC_SOCKET=/path/to/signet/node.sock cargo run -p bdk_bitcoind_rpc \
//         --example ipc_emitter --features ipc`

const EXTERNAL: &str = "tr([83737d5e/86'/1'/0']tpubDDR5GgtoxS8fJyjjvdahN4VzV5DV6jtbcyvVXhEKq2XtpxjxBXmxH3r8QrNbQqHg4bJM1EGkxi7Pjfkgnui9jQWqS7kxHvX6rhUeriLDKxz/0/*)";
const INTERNAL: &str = "tr([83737d5e/86'/1'/0']tpubDDR5GgtoxS8fJyjjvdahN4VzV5DV6jtbcyvVXhEKq2XtpxjxBXmxH3r8QrNbQqHg4bJM1EGkxi7Pjfkgnui9jQWqS7kxHvX6rhUeriLDKxz/1/*)";
const NETWORK: Network = Network::Signet;

const START_HEIGHT: u32 = 205_000;
const START_HASH: &str = "0000002bd0f82f8c0c0f1e19128f84c938763641dba85c44bdb6aed1678d16cb";

fn main() -> anyhow::Result<()> {
    // Set up the receiving chain and graph structures.
    let secp = Secp256k1::new();
    let (descriptor, _) = Descriptor::parse_descriptor(&secp, EXTERNAL)?;
    let (change_descriptor, _) = Descriptor::parse_descriptor(&secp, INTERNAL)?;
    let (mut chain, _) = LocalChain::from_genesis(genesis_block(NETWORK).block_hash());

    let mut graph = IndexedTxGraph::<ConfirmationBlockTime, KeychainTxOutIndex<&str>>::new({
        let mut index = KeychainTxOutIndex::default();
        index.insert_descriptor("external", descriptor.clone())?;
        index.insert_descriptor("internal", change_descriptor.clone())?;
        index
    });

    // Assume a minimum birthday height so we do not replay signet from genesis.
    let _ = chain.insert_block(START_HEIGHT, START_HASH.parse()?)?;

    // Connect to bitcoin-node over IPC. No RPC url/cookie is needed, only the unix socket path.
    let socket = std::env::var("CORE_IPC_SOCKET")
        .context("must set CORE_IPC_SOCKET to the bitcoin-node IPC socket path")?;
    let mut emitter = IpcEmitter::new(&socket, chain.tip(), START_HEIGHT)?;

    let start = Instant::now();
    while let Some(event) = emitter.next_block()? {
        let height = event.block_height();
        let _ = chain.apply_update(event.checkpoint)?;
        let changeset = graph.apply_block_relevant(&event.block, height);
        if !changeset.is_empty() {
            println!("Matched block {height}");
        }
    }

    println!("\ntook: {}s", start.elapsed().as_secs());
    println!("Local tip: {}", chain.tip().height());

    let canonical_view =
        chain.canonical_view(graph.graph(), chain.tip().block_id(), Default::default());

    let unspent: Vec<_> = canonical_view
        .filter_unspent_outpoints(graph.index.outpoints().clone())
        .collect();
    if !unspent.is_empty() {
        println!("\nUnspent");
        for (index, utxo) in unspent {
            println!("{:?} | {} | {}", index, utxo.txout.value, utxo.outpoint);
        }
    }

    Ok(())
}
