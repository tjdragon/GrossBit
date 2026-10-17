use bitcoin::hashes::Hash;
use bitcoin::key::Secp256k1;
use bitcoin::secp256k1::{Keypair, SecretKey};
use bitcoin::{Address, Amount, CompressedPublicKey, Network, OutPoint, ScriptBuf, Txid};
use grossbit::model::{FeeRate, GrossUtxo, ScriptKind};
use grossbit::tx::{TxBuilder, TxParser, TxSigner};

#[test]
fn test_build_sign_and_parse_p2tr() {
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(&[7u8; 32]).unwrap();
    let keypair = Keypair::from_secret_key(&secp, &secret_key);
    let (internal_key, _) = keypair.x_only_public_key();

    // Taproot script_pubkey
    let p2tr_script = ScriptBuf::new_p2tr(&secp, internal_key, None);
    let p2tr_address = Address::from_script(&p2tr_script, Network::Testnet4).unwrap();

    let dummy_txid = Txid::from_byte_array([9u8; 32]);
    let utxo = GrossUtxo {
        outpoint: OutPoint::new(dummy_txid, 0),
        amount: Amount::from_sat(100_000),
        script_pubkey: p2tr_script,
        address: Some(p2tr_address.to_string()),
        script_kind: ScriptKind::P2tr,
        confirmations: 3,
        is_coinbase: false,
    };

    // Recipient address (can be anything: P2WPKH, P2TR, etc.)
    let recipient_secret = SecretKey::from_slice(&[8u8; 32]).unwrap();
    let recipient_pubkey = recipient_secret.public_key(&secp);
    let compressed_recip = CompressedPublicKey(recipient_pubkey);
    let recipient_addr = Address::p2wpkh(&compressed_recip, Network::Testnet4);

    // Build transaction
    let built = TxBuilder::new(Network::Testnet4)
        .add_input(utxo.clone())
        .add_recipient(recipient_addr.clone(), Amount::from_sat(40_000))
        .change_address(p2tr_address.clone())
        .fee_rate(FeeRate::from_sat_per_vb(2.0))
        .enable_rbf(true)
        .build()
        .expect("Build should succeed");

    assert_eq!(built.transaction.input.len(), 1);
    assert_eq!(built.transaction.output.len(), 2); // Recipient + Change
    assert!(built.fee_sat > 0);
    assert!(built.change_sat > 0);

    // Sign transaction with Schnorr
    let signed_hex = TxSigner::sign_transaction(built, &secret_key)
        .expect("P2TR signing should succeed");

    assert!(!signed_hex.is_empty());

    // Parse transaction back from raw hex string
    let parsed = TxParser::parse_hex(&signed_hex, Network::Testnet4)
        .expect("Parsing should succeed");

    assert_eq!(parsed.inputs.len(), 1);
    assert_eq!(parsed.inputs[0].witness_items_hex.len(), 1); // 64-byte Schnorr signature
    assert_eq!(parsed.inputs[0].script_kind, Some(ScriptKind::P2tr));

    assert_eq!(parsed.outputs.len(), 2);
    assert_eq!(parsed.outputs[0].script_kind, ScriptKind::P2wpkh);
    assert_eq!(parsed.outputs[0].amount_sat, 40_000);
    assert_eq!(parsed.outputs[1].script_kind, ScriptKind::P2tr); // Default change is P2TR!
}

#[test]
fn test_build_sign_and_parse_p2wpkh() {
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(&[3u8; 32]).unwrap();
    let public_key = secret_key.public_key(&secp);
    let compressed_pubkey = CompressedPublicKey(public_key);
    let wpkh_addr = Address::p2wpkh(&compressed_pubkey, Network::Testnet4);

    let dummy_txid = Txid::from_byte_array([5u8; 32]);
    let utxo = GrossUtxo {
        outpoint: OutPoint::new(dummy_txid, 0),
        amount: Amount::from_sat(80_000),
        script_pubkey: wpkh_addr.script_pubkey(),
        address: Some(wpkh_addr.to_string()),
        script_kind: ScriptKind::P2wpkh,
        confirmations: 12,
        is_coinbase: false,
    };

    // Recipient Taproot address
    let recipient_secret = SecretKey::from_slice(&[4u8; 32]).unwrap();
    let keypair = Keypair::from_secret_key(&secp, &recipient_secret);
    let (internal_key, _) = keypair.x_only_public_key();
    let p2tr_recipient = Address::p2tr(&secp, internal_key, None, Network::Testnet4);

    let built = TxBuilder::new(Network::Testnet4)
        .add_input(utxo.clone())
        .add_recipient(p2tr_recipient.clone(), Amount::from_sat(35_000))
        .change_address(wpkh_addr.clone())
        .fee_rate(FeeRate::from_sat_per_vb(1.5))
        .build()
        .expect("Build should succeed");

    let signed_hex = TxSigner::sign_transaction(built, &secret_key)
        .expect("P2WPKH signing should succeed");

    let parsed = TxParser::parse_hex(&signed_hex, Network::Testnet4)
        .expect("Parsing should succeed");

    assert_eq!(parsed.inputs.len(), 1);
    assert_eq!(parsed.inputs[0].witness_items_hex.len(), 2); // DER Signature + Compressed Pubkey
    assert_eq!(parsed.inputs[0].script_kind, Some(ScriptKind::P2wpkh));

    assert_eq!(parsed.outputs.len(), 2);
    assert_eq!(parsed.outputs[0].script_kind, ScriptKind::P2tr);
    assert_eq!(parsed.outputs[0].amount_sat, 35_000);
}
