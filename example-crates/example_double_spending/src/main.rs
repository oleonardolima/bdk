use std::str::FromStr;

use bdk_esplora::EsploraExt;
use bdk_wallet::{
    bitcoin::{Address, Amount, FeeRate, TxIn},
    rusqlite::Connection,
    KeychainKind, SignOptions, TxOrdering, Wallet,
};

fn main() {
    const ESPLORA_API: &str = "https://mempool.space/testnet4/api";
    const PARALLEL_REQUESTS: usize = 5;

    // Initialize the DB connections for both wallets A and B.
    let wallet_a = "WALLET_A";
    let wallet_b = "WALLET_B";

    let mut conn_a = Connection::open(format!("./{}.sqlite", wallet_a).as_str()).unwrap();
    let mut conn_b = Connection::open(format!("./{}.sqlite", wallet_b).as_str()).unwrap();

    // Initialize & Load wallets A and B.
    let mut wallet_a = Wallet::load()
        .descriptor(KeychainKind::External, Some("tr()#h305zpuu"))
        .extract_keys()
        .load_wallet(&mut conn_a)
        .unwrap()
        .unwrap();

    let mut wallet_b = Wallet::load()
        .descriptor(KeychainKind::External, Some("tr()#dyn6d6zd"))
        .extract_keys()
        .load_wallet(&mut conn_b)
        .unwrap();

    let esplora_client = bdk_esplora::esplora_client::Builder::new(ESPLORA_API).build_blocking();

    // Sync Wallet A

    let sync_req = wallet_a.start_sync_with_revealed_spks().build();
    let sync_res = esplora_client.sync(sync_req, PARALLEL_REQUESTS).unwrap();
    let _ = wallet_a.apply_update(sync_res).unwrap();

    // Persist Wallet B
    wallet_a.persist(&mut conn_b).unwrap();

    // Sync Wallet B
    let sync_req = wallet_b.start_sync_with_revealed_spks().build();
    let sync_res = esplora_client.sync(sync_req, PARALLEL_REQUESTS).unwrap();
    let _ = wallet_b.apply_update(sync_res).unwrap();

    // Persist Wallet B
    wallet_b.persist(&mut conn_b).unwrap();

    for tx in wallet_a.transactions() {
        println!("wallet: {:?} tx: {:?}", wallet_a, tx);
    }

    for tx in wallet_b.transactions() {
        println!("wallet: {:?} tx: {:?}", wallet_b, tx);
    }

    // Build Initial TxA: WalletA -> WalletB

    let wa_change_addr = wallet_a.peek_address(KeychainKind::Internal, 0).address;
    let wb_recv_addr = wallet_b.peek_address(KeychainKind::External, 0).address;

    let mut tx1_builder = wallet_a.build_tx();
    tx1_builder
        .ordering(TxOrdering::Untouched)
        .add_recipient(wb_recv_addr.script_pubkey(), Amount::from_sat(1000))
        .fee_rate(FeeRate::from_sat_per_vb(1).unwrap())
        .drain_to(wa_change_addr.script_pubkey());

    let mut tx1_psbt = tx1_builder.finish().unwrap();

    let _sign_outcome = wallet_a
        .sign(&mut tx1_psbt, SignOptions::default())
        .unwrap();

    let tx1 = tx1_psbt.extract_tx().unwrap();
    println!("tx1: {:?}", tx1.compute_txid());

    esplora_client.broadcast(&tx1).unwrap();

    // Sync Wallet B
    let sync_req = wallet_b.start_sync_with_revealed_spks().build();
    let sync_res = esplora_client.sync(sync_req, PARALLEL_REQUESTS).unwrap();
    let _ = wallet_b.apply_update(sync_res).unwrap();

    // Persist Wallet B
    wallet_b.persist(&mut conn_b).unwrap();

    for tx in wallet_b.transactions() {
        println!("wallet: {:?} tx: {:?}", wallet_b, tx.tx_node.txid);
    }

    // RBF Tx1 (Double Spend)
    let wa_recv_addr = wallet_a.peek_address(KeychainKind::External, 0).address;

    let mut tx2_builder = wallet_a.build_fee_bump(tx1.compute_txid()).unwrap();

    tx2_builder
        .fee_rate(FeeRate::from_sat_per_vb_unchecked(15))
        .set_recipients(vec![(wa_recv_addr.script_pubkey(), Amount::from_sat(1000))])
        .drain_to(wa_change_addr.script_pubkey())
        .drain_wallet();

    let mut tx2_psbt = tx2_builder.finish().unwrap();

    let _sign_outcome = wallet_a
        .sign(&mut tx2_psbt, SignOptions::default())
        .unwrap();

    let tx2 = tx2_psbt.extract_tx().unwrap();
    println!("tx2: {:?}", tx2.compute_txid());

    esplora_client.broadcast(&tx2).unwrap();

    // Check that both Tx1 and Tx2 (RBF) have different TxIds.
    assert_ne!(tx1.compute_txid(), tx2.compute_txid());

    // Check that Wallet B transactions contains Tx2, but not Tx1.
    assert!(wallet_b
        .transactions()
        .find(|tx| tx.tx_node.txid == tx2.compute_txid())
        .is_some());
    assert!(wallet_b
        .transactions()
        .find(|tx| tx.tx_node.txid == tx1.compute_txid())
        .is_none());

    // Sync Wallet B
    let sync_req = wallet_b.start_sync_with_revealed_spks().build();
    let sync_res = esplora_client.sync(sync_req, PARALLEL_REQUESTS).unwrap();
    let _ = wallet_b.apply_update(sync_res).unwrap();

    wallet_a.persist(&mut conn_a).unwrap();
    wallet_b.persist(&mut conn_b).unwrap();

    for tx in wallet_b.transactions() {
        println!("wallet: {:?} tx: {:?}", wallet_b, tx);
    }

    for tx in wallet_b.transactions() {
        println!("wallet: {:?} tx: {:?}", wallet_b, tx);
    }
}
