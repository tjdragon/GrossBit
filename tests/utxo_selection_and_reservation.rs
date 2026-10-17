use bitcoin::hashes::Hash;
use bitcoin::secp256k1::XOnlyPublicKey;
use bitcoin::{Amount, OutPoint, ScriptBuf, Txid};
use chrono::Duration;
use grossbit::model::{FeeRate, GrossUtxo, ScriptKind};
use grossbit::utxo::selection::{CoinSelectionStrategy, CoinSelector};
use grossbit::utxo::UtxoPool;

fn dummy_outpoint(index: u32) -> OutPoint {
    let mut bytes = [0u8; 32];
    bytes[0] = index as u8;
    OutPoint::new(Txid::from_byte_array(bytes), index)
}

#[test]
fn test_coin_selection_branch_and_bound() {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let xpk1 = XOnlyPublicKey::from_slice(&[1u8; 32]).unwrap();
    let xpk2 = XOnlyPublicKey::from_slice(&[2u8; 32]).unwrap();

    let utxo1 = GrossUtxo {
        outpoint: dummy_outpoint(1),
        amount: Amount::from_sat(50_000),
        script_pubkey: ScriptBuf::new_p2tr(&secp, xpk1, None),
        address: None,
        script_kind: ScriptKind::P2tr,
        confirmations: 10,
        is_coinbase: false,
    };

    let utxo2 = GrossUtxo {
        outpoint: dummy_outpoint(2),
        amount: Amount::from_sat(100_000),
        script_pubkey: ScriptBuf::new_p2tr(&secp, xpk2, None),
        address: None,
        script_kind: ScriptKind::P2tr,
        confirmations: 5,
        is_coinbase: false,
    };

    let utxos = vec![utxo1.clone(), utxo2.clone()];
    let fee_rate = FeeRate::from_sat_per_vb(2.0);

    // Select with Optimal strategy
    let selected = CoinSelector::select(
        utxos,
        45_000,
        fee_rate,
        ScriptKind::P2tr,
        ScriptKind::P2tr,
        CoinSelectionStrategy::Optimal,
    )
    .expect("Selection should succeed");

    assert_eq!(selected.selected_utxos.len(), 1);
    assert_eq!(selected.selected_utxos[0].satoshis(), 50_000);
    assert!(selected.change_sat > 0);
    assert!(selected.fee_sat > 0);
}

#[test]
fn test_reservation_anti_double_spend() {
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let xpk1 = XOnlyPublicKey::from_slice(&[1u8; 32]).unwrap();
    let xpk2 = XOnlyPublicKey::from_slice(&[2u8; 32]).unwrap();

    let pool = UtxoPool::new();
    let op1 = dummy_outpoint(1);
    let op2 = dummy_outpoint(2);

    let utxo1 = GrossUtxo {
        outpoint: op1,
        amount: Amount::from_sat(60_000),
        script_pubkey: ScriptBuf::new_p2tr(&secp, xpk1, None),
        address: Some("tb1p...".into()),
        script_kind: ScriptKind::P2tr,
        confirmations: 6,
        is_coinbase: false,
    };

    let utxo2 = GrossUtxo {
        outpoint: op2,
        amount: Amount::from_sat(40_000),
        script_pubkey: ScriptBuf::new_p2tr(&secp, xpk2, None),
        address: Some("tb1p...".into()),
        script_kind: ScriptKind::P2tr,
        confirmations: 2,
        is_coinbase: false,
    };

    pool.insert(utxo1);
    pool.insert(utxo2);

    assert_eq!(pool.available_utxos().len(), 2);
    assert_eq!(pool.balance().spendable_sat(), 100_000);

    // Reserve 50,000 sats -> should pick utxo1 and lock it
    let (sel, res_id) = pool
        .select_and_reserve(
            50_000,
            FeeRate::from_sat_per_vb(2.0),
            ScriptKind::P2tr,
            ScriptKind::P2tr,
            CoinSelectionStrategy::Optimal,
            Duration::seconds(60),
            Some("Tx A".into()),
        )
        .expect("Should reserve");

    assert_eq!(sel.selected_utxos.len(), 1);
    assert_eq!(sel.selected_utxos[0].outpoint, op1);

    // Available UTXOs now only contains utxo2!
    assert_eq!(pool.available_utxos().len(), 1);
    assert_eq!(pool.available_utxos()[0].outpoint, op2);
    assert_eq!(pool.balance().reserved_sat, 60_000);
    assert_eq!(pool.balance().spendable_sat(), 40_000);

    // Trying to spend 50,000 sats again should fail because utxo1 is reserved!
    let second_attempt = pool.select_and_reserve(
        50_000,
        FeeRate::from_sat_per_vb(2.0),
        ScriptKind::P2tr,
        ScriptKind::P2tr,
        CoinSelectionStrategy::Optimal,
        Duration::seconds(60),
        Some("Tx B".into()),
    );
    assert!(second_attempt.is_err(), "Must prevent double-spending reserved UTXO");

    // Releasing the reservation frees utxo1 back to available
    assert!(pool.release_reservation(&res_id));
    assert_eq!(pool.available_utxos().len(), 2);
    assert_eq!(pool.balance().spendable_sat(), 100_000);
}
