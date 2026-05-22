#![allow(clippy::print_stdout, clippy::print_stderr)]
// use std::time::Instant;

use std::time::Instant;

use bdk_chain::bitcoin::hashes::hex::FromHex;
use bdk_chain::bitcoin::{constants::genesis_block, secp256k1::Secp256k1, Network};
use bdk_chain::indexer::keychain_txout::KeychainTxOutIndex;
use bdk_chain::local_chain::LocalChain;
use bdk_chain::miniscript::Descriptor;
use bdk_chain::{ConfirmationBlockTime, IndexedTxGraph};
use bdk_testenv::anyhow;
use bitcoinkernel::{ChainstateManagerBuilder, ContextBuilder, KernelError, Log, Logger};
use env_logger::Builder;
use log::LevelFilter;

use crate::kernel_iter::KernelIter;

mod kernel_iter;

// This example shows how BDK chain and tx-graph structures are updated by reading blocks
// directly from a Bitcoin Core data directory, using `bitcoinkernel`.

// Usage: `cargo run -p example_bitcoinkernel`
// The bitcoind data directory can be overridden with the `DATA_DIR` environment variable.

/// Default bitcoind data directory, used when the `DATA_DIR` environment variable is unset.
const DEFAULT_DATA_DIR: &str = "/root/.bitcoin/signet";

/// Custom signet challenge (the serialized script blocks must satisfy), as a hex string.
/// See the `signetchallenge` setting in vinteum-bdl's `infra-signet-server/config/bitcoin.conf`.
const SIGNET_CHALLENGE: &str = "0014bdec02fe5ec499cc2cb52dc160230643a84dd118";

const NETWORK: Network = Network::Signet;

const EXTERNAL: &str = "wpkh(tprv8ZgxMBicQKsPdhhv1KhNWaxPcRpsNcpHK63mQ4wkXf2DNk3yHQ8eY7MZF6985J2FezXvY4ZpwjQqccgqH6RczR3axnwUBF351NrfvdJc2Pg/84h/1h/0h/0/*)";
const INTERNAL: &str = "wpkh(tprv8ZgxMBicQKsPdhhv1KhNWaxPcRpsNcpHK63mQ4wkXf2DNk3yHQ8eY7MZF6985J2FezXvY4ZpwjQqccgqH6RczR3axnwUBF351NrfvdJc2Pg/84h/1h/0h/1/*)";

// Wallet birthday — unused on this small signet, kept for reference.
// const START_HEIGHT: u32 = 205_000;
// const START_HASH: &str = "0000002bd0f82f8c0c0f1e19128f84c938763641dba85c44bdb6aed1678d16cb";

struct MainLog {}

impl Log for MainLog {
    fn log(&self, message: &str) {
        log::info!(
            target: "libbitcoinkernel",
            "{}", message.strip_suffix("\r\n").or_else(|| message.strip_suffix('\n')).unwrap_or(message));
    }
}

fn setup_logging() -> Result<Logger, KernelError> {
    let mut builder = Builder::from_default_env();
    builder.filter(None, LevelFilter::Info).init();
    Logger::new(MainLog {})
}

// TODO: (@oleonardolima) convert this example to a `bdk_wallet` one!
fn main() -> anyhow::Result<()> {
    let secp = Secp256k1::new();
    let _logger = setup_logging()?;

    // setup rust-bitcoinkernel
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_string());
    let blocks_dir = format!("{data_dir}/blocks");

    let challenge = Vec::<u8>::from_hex(SIGNET_CHALLENGE)?;
    let context = ContextBuilder::new().signet(&challenge).build()?;

    let chainstate_manager =
        ChainstateManagerBuilder::new(&context, &data_dir, &blocks_dir)?.build()?;

    chainstate_manager.import_blocks()?;

    log::info!("successfully setup `bitcoinkernel::ChainstateManager`!");

    // setup chain
    let (mut chain, _) = LocalChain::from_genesis(genesis_block(NETWORK).block_hash());

    // setup descriptors
    let (descriptor, _) = Descriptor::parse_descriptor(&secp, EXTERNAL)?;
    let (change_descriptor, _) = Descriptor::parse_descriptor(&secp, INTERNAL)?;

    // setup tx_graph
    let mut tx_graph = IndexedTxGraph::<ConfirmationBlockTime, KeychainTxOutIndex<&str>>::new({
        let mut index = KeychainTxOutIndex::default();
        index.insert_descriptor("external", descriptor.clone())?;
        index.insert_descriptor("internal", change_descriptor.clone())?;
        index
    });

    log::info!("successfully setup descriptors, chain and txgraph!");

    // assume a minimum birthday height
    // let _ = chain.insert_block(START_HEIGHT, START_HASH.parse()?)?;

    // scan all blocks of the kernel's active chain that are missing from the local chain.
    //
    // `KernelIter` emits blocks above the point of agreement between `chain.tip()` and the
    // kernel's active chain, then for each emitted block:
    //
    // - extend `chain` with the block's checkpoint (linked, so it always connects).
    // - index transactions relevant to the `graph`'s keychains.
    //

    let active_chain_tip = chainstate_manager.active_chain().height();
    log::info!("the best chain tip is {active_chain_tip}!");

    let start = Instant::now();
    for event in KernelIter::new(&chainstate_manager, chain.tip()) {
        let event = event?;
        let height = event.checkpoint.height();
        let hash = event.checkpoint.hash();

        log::info!("applying block height={height} ; hash={hash} ...");

        let _ = chain.apply_update(event.checkpoint)?;
        let _ = tx_graph.apply_block_relevant(&event.block, height);

        log::info!("successfully applied block height={height} ; hash={hash}!");
    }

    log::info!(
        "finished to apply blocks, took: {}s ; current localchain tip={}",
        start.elapsed().as_secs(),
        chain.tip().height()
    );

    let canonical_view =
        tx_graph.canonical_view(&chain, chain.tip().block_id(), Default::default());

    let unspent: Vec<_> = canonical_view
        .filter_unspent_outpoints(tx_graph.index.outpoints().clone())
        .collect();
    if !unspent.is_empty() {
        println!("\nUnspent");
        for (index, utxo) in unspent {
            // (k, index) | value | outpoint |
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
