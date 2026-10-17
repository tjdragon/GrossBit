use crate::error::{GrossBitError, Result};
use crate::model::{GrossUtxo, ScriptKind};
use crate::tx::builder::BuiltTransaction;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::key::{Secp256k1, TapTweak};
use bitcoin::secp256k1::schnorr::Signature as SchnorrSignature;
use bitcoin::secp256k1::{Keypair, Message, SecretKey};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::{EcdsaSighashType, Transaction, TxOut, Witness};

pub struct TxSigner;

impl TxSigner {
    /// Sign a P2WPKH input in a transaction using an ECDSA secret key.
    pub fn sign_p2wpkh(
        tx: &mut Transaction,
        input_index: usize,
        utxo: &GrossUtxo,
        secret_key: &SecretKey,
    ) -> Result<()> {
        let secp = Secp256k1::new();
        let public_key = secret_key.public_key(&secp);

        let mut sighash_cache = SighashCache::new(&*tx);
        let sighash = sighash_cache
            .p2wpkh_signature_hash(
                input_index,
                &utxo.script_pubkey,
                utxo.amount,
                EcdsaSighashType::All,
            )
            .map_err(|e| GrossBitError::SigningError(format!("P2WPKH sighash error: {}", e)))?;

        let msg = Message::from_digest(sighash.to_byte_array());
        let sig = secp.sign_ecdsa(&msg, secret_key);

        let mut sig_bytes = sig.serialize_der().to_vec();
        sig_bytes.push(EcdsaSighashType::All as u8);

        // Construct witness: [signature, pubkey]
        let mut witness = Witness::new();
        witness.push(sig_bytes);
        witness.push(public_key.serialize());

        tx.input[input_index].witness = witness;

        Ok(())
    }

    /// Sign a P2TR (Taproot key-path) input in a transaction using a Schnorr keypair.
    pub fn sign_p2tr_keypath(
        tx: &mut Transaction,
        input_index: usize,
        all_utxos: &[GrossUtxo],
        keypair: &Keypair,
    ) -> Result<()> {
        let secp = Secp256k1::new();

        // Prevouts required for taproot signature hashing
        let prevouts: Vec<TxOut> = all_utxos
            .iter()
            .map(|u| TxOut {
                value: u.amount,
                script_pubkey: u.script_pubkey.clone(),
            })
            .collect();

        // Taproot key tweak for standard key-path spending with no merkle tree
        let tweaked_keypair = keypair.tap_tweak(&secp, None);

        let mut sighash_cache = SighashCache::new(&*tx);
        let sighash = sighash_cache
            .taproot_key_spend_signature_hash(
                input_index,
                &Prevouts::All(&prevouts),
                TapSighashType::Default,
            )
            .map_err(|e| GrossBitError::SigningError(format!("Taproot sighash error: {}", e)))?;

        let msg = Message::from_digest(sighash.to_byte_array());
        let sig: SchnorrSignature = secp.sign_schnorr_no_aux_rand(&msg, &tweaked_keypair.to_keypair());

        // Construct witness: [schnorr_signature]
        let mut witness = Witness::new();
        witness.push(sig.as_ref());

        tx.input[input_index].witness = witness;

        Ok(())
    }

    /// Sign all inputs of a BuiltTransaction matching the provided secret key / keypair.
    pub fn sign_transaction(
        mut built: BuiltTransaction,
        secret_key: &SecretKey,
    ) -> Result<String> {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, secret_key);

        for (idx, utxo) in built.spent_utxos.iter().enumerate() {
            match utxo.script_kind {
                ScriptKind::P2wpkh => {
                    Self::sign_p2wpkh(&mut built.transaction, idx, utxo, secret_key)?;
                }
                ScriptKind::P2tr => {
                    Self::sign_p2tr_keypath(
                        &mut built.transaction,
                        idx,
                        &built.spent_utxos,
                        &keypair,
                    )?;
                }
                _ => {
                    return Err(GrossBitError::SigningError(format!(
                        "Unsupported script type {:?} for auto-signing (only P2WPKH and P2TR supported for spending)",
                        utxo.script_kind
                    )));
                }
            }
        }

        Ok(serialize_hex(&built.transaction))
    }
}
