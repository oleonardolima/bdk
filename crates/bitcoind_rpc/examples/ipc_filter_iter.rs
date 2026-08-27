#![allow(clippy::print_stdout, clippy::print_stderr)]
use std::time::Instant;

use bdk_bitcoind_rpc::ipc::{Event, IpcFilterIter};
use bdk_chain::bitcoin::{constants::genesis_block, secp256k1::Secp256k1, Network};
use bdk_chain::indexer::keychain_txout::KeychainTxOutIndex;
use bdk_chain::local_chain::LocalChain;
use bdk_chain::miniscript::Descriptor;
use bdk_chain::{ConfirmationBlockTime, IndexedTxGraph, SpkIterator};
use bdk_testenv::anyhow::{self, Context};

// This example shows how BDK chain and tx-graph structures are updated by BIP158 filter
// syncing over Bitcoin Core's multiprocess IPC (Cap'n Proto) interface. It mirrors the
// JSON-RPC `filter_iter` example, with one twist: filter matching happens node-side, so
// filters are never downloaded and only matching blocks cross the socket.
//
// It requires a node built from Bitcoin Core PR #29409, run with `-ipcbind=unix` and
// `-blockfilterindex=1`. See the README in this directory for the full setup. Point
// CORE_IPC_SOCKET at the node's `node.sock`.
//
// Usage: `CORE_IPC_SOCKET=/path/to/signet/node.sock cargo run -p bdk_bitcoind_rpc \
//         --example ipc_filter_iter --features ipc`

const EXTERNAL: &str = "tr([83737d5e/86'/1'/0']tpubDDR5GgtoxS8fJyjjvdahN4VzV5DV6jtbcyvVXhEKq2XtpxjxBXmxH3r8QrNbQqHg4bJM1EGkxi7Pjfkgnui9jQWqS7kxHvX6rhUeriLDKxz/0/*)";
const INTERNAL: &str = "tr([83737d5e/86'/1'/0']tpubDDR5GgtoxS8fJyjjvdahN4VzV5DV6jtbcyvVXhEKq2XtpxjxBXmxH3r8QrNbQqHg4bJM1EGkxi7Pjfkgnui9jQWqS7kxHvX6rhUeriLDKxz/1/*)";
const SPK_COUNT: u32 = 25;
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

    // Assume a minimum birthday height so we do not scan signet from genesis.
    let _ = chain.insert_block(START_HEIGHT, START_HASH.parse()?)?;

    // Collect the watched script pubkeys. The set is fixed for the scanner's lifetime, so
    // include a lookahead range from each keychain.
    let mut spks = vec![];
    for (_, desc) in graph.index.keychains() {
        spks.extend(SpkIterator::new_with_range(desc, 0..SPK_COUNT).map(|(_, s)| s));
    }

    // Connect to bitcoin-node over IPC. No RPC url/cookie is needed, only the unix socket
    // path. Fails with `NoBlockFilterIndex` unless the node runs `-blockfilterindex=1`.
    let socket = std::env::var("CORE_IPC_SOCKET")
        .context("must set CORE_IPC_SOCKET to the bitcoin-node IPC socket path")?;
    let iter = IpcFilterIter::new(&socket, chain.tip(), spks)?;

    let start = Instant::now();
    for res in iter {
        let Event { cp, block } = res?;
        let height = cp.height();
        let _ = chain.apply_update(cp)?;
        if let Some(block) = block {
            let _ = graph.apply_block_relevant(&block, height);
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

    for canon_tx in canonical_view.txs() {
        if !canon_tx.pos.is_confirmed() {
            eprintln!("ERROR: canonical tx should be confirmed {}", canon_tx.txid);
        }
    }

    Ok(())
}
