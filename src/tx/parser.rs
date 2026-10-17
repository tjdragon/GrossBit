use crate::error::{GrossBitError, Result};
use crate::model::{ParsedTx, ParsedTxIn, ParsedTxOut, ScriptKind};
use bitcoin::consensus::encode::deserialize_hex;
use bitcoin::{Address, Network, Transaction};

pub struct TxParser;

impl TxParser {
    /// Parse a transaction from raw hex string.
    pub fn parse_hex(hex_str: &str, network: Network) -> Result<ParsedTx> {
        let clean_hex = hex_str.trim().trim_start_matches("0x");
        let tx: Transaction = deserialize_hex(clean_hex)
            .map_err(|e| GrossBitError::TxParseError(format!("Failed to deserialize tx: {}", e)))?;
        Self::parse_tx(&tx, network)
    }

    /// Parse an in-memory `bitcoin::Transaction` into a rich `ParsedTx` model.
    pub fn parse_tx(tx: &Transaction, network: Network) -> Result<ParsedTx> {
        let txid = tx.compute_txid();
        let total_size = tx.total_size();
        let vsize = tx.vsize();
        let weight = tx.weight().to_wu();

        let inputs = tx
            .input
            .iter()
            .map(|tx_in| {
                let witness_items_hex: Vec<String> = tx_in
                    .witness
                    .iter()
                    .map(|item| hex::encode(item))
                    .collect();

                // Infer script kind from witness if available
                let script_kind = if tx_in.witness.is_empty() {
                    None
                } else if tx_in.witness.len() == 2 && tx_in.witness.last().map(|k| k.len() == 33).unwrap_or(false) {
                    Some(ScriptKind::P2wpkh)
                } else if tx_in.witness.len() == 1 && tx_in.witness[0].len() == 64 {
                    Some(ScriptKind::P2tr)
                } else {
                    Some(ScriptKind::Unknown)
                };

                ParsedTxIn {
                    txid: tx_in.previous_output.txid,
                    vout: tx_in.previous_output.vout,
                    sequence: tx_in.sequence.to_consensus_u32(),
                    script_sig_hex: hex::encode(tx_in.script_sig.as_bytes()),
                    witness_items_hex,
                    script_kind,
                }
            })
            .collect();

        let outputs = tx
            .output
            .iter()
            .enumerate()
            .map(|(vout, tx_out)| {
                let script_kind = ScriptKind::from_script(&tx_out.script_pubkey);
                let address = Address::from_script(&tx_out.script_pubkey, network)
                    .ok()
                    .map(|a| a.to_string());

                ParsedTxOut {
                    vout: vout as u32,
                    amount_sat: tx_out.value.to_sat(),
                    script_pubkey_hex: hex::encode(tx_out.script_pubkey.as_bytes()),
                    address,
                    script_kind,
                }
            })
            .collect();

        Ok(ParsedTx {
            txid,
            version: tx.version.0,
            lock_time: tx.lock_time.to_consensus_u32(),
            size: total_size,
            vsize,
            weight,
            inputs,
            outputs,
        })
    }
}

// Minimal hex module helper using standard library or bitcoin format to avoid external hex crate conflict
mod hex {
    pub fn encode(data: &[u8]) -> String {
        let mut s = String::with_capacity(data.len() * 2);
        for byte in data {
            use std::fmt::Write;
            write!(&mut s, "{:02x}", byte).unwrap();
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash;
    use bitcoin::absolute::LockTime;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, ScriptBuf, Sequence, TxIn, TxOut};

    #[test]
    fn test_parse_tx() {
        let dummy_tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: bitcoin::OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::all_zeros()),
            }],
        };

        let parsed = TxParser::parse_tx(&dummy_tx, Network::Testnet).unwrap();
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.outputs[0].script_kind, ScriptKind::P2wpkh);
        assert_eq!(parsed.outputs[0].amount_sat, 50_000);
    }
}
