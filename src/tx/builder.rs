use crate::error::{GrossBitError, Result};
use crate::model::{FeeRate, GrossUtxo, ScriptKind};
use crate::utxo::selection::CoinSelector;
use bitcoin::absolute::LockTime;
use bitcoin::transaction::Version;
use bitcoin::{
    Address, Amount, Network, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
};

/// A recipient output for a transaction.
#[derive(Debug, Clone)]
pub struct Recipient {
    pub address: Address,
    pub amount: Amount,
}

/// Transaction builder supporting Native SegWit (P2WPKH) and Taproot (P2TR).
#[derive(Debug, Clone)]
pub struct TxBuilder {
    network: Network,
    inputs: Vec<GrossUtxo>,
    recipients: Vec<Recipient>,
    change_address: Option<Address>,
    fee_rate: Option<FeeRate>,
    enable_rbf: bool,
    lock_time: LockTime,
}

impl TxBuilder {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            inputs: Vec::new(),
            recipients: Vec::new(),
            change_address: None,
            fee_rate: None,
            enable_rbf: true,
            lock_time: LockTime::ZERO,
        }
    }

    /// The network this transaction is being constructed for.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Add an input UTXO.
    pub fn add_input(mut self, utxo: GrossUtxo) -> Self {
        self.inputs.push(utxo);
        self
    }

    /// Add multiple input UTXOs.
    pub fn add_inputs(mut self, utxos: Vec<GrossUtxo>) -> Self {
        self.inputs.extend(utxos);
        self
    }

    /// Add a recipient output.
    pub fn add_recipient(mut self, address: Address, amount: Amount) -> Self {
        self.recipients.push(Recipient { address, amount });
        self
    }

    /// Set change address for excess funds.
    pub fn change_address(mut self, address: Address) -> Self {
        self.change_address = Some(address);
        self
    }

    /// Set fee rate in sat/vB.
    pub fn fee_rate(mut self, fee_rate: FeeRate) -> Self {
        self.fee_rate = Some(fee_rate);
        self
    }

    /// Enable or disable Replace-By-Fee (BIP 125).
    pub fn enable_rbf(mut self, enable: bool) -> Self {
        self.enable_rbf = enable;
        self
    }

    /// Build the raw unsigned Bitcoin transaction.
    pub fn build(self) -> Result<BuiltTransaction> {
        if self.inputs.is_empty() {
            return Err(GrossBitError::TxBuildError("No inputs provided".into()));
        }
        if self.recipients.is_empty() {
            return Err(GrossBitError::TxBuildError("No recipients provided".into()));
        }

        let fee_rate = self
            .fee_rate
            .ok_or_else(|| GrossBitError::TxBuildError("Fee rate must be specified".into()))?;

        let total_input_sat: u64 = self.inputs.iter().map(|u| u.satoshis()).sum();
        let total_recipient_sat: u64 = self.recipients.iter().map(|r| r.amount.to_sat()).sum();

        if total_input_sat < total_recipient_sat {
            return Err(GrossBitError::InsufficientFunds {
                needed_sat: total_recipient_sat,
                available_sat: total_input_sat,
            });
        }

        let input_kinds: Vec<ScriptKind> = self.inputs.iter().map(|u| u.script_kind).collect();
        let recipient_kinds: Vec<ScriptKind> = self
            .recipients
            .iter()
            .map(|r| ScriptKind::from_script(&r.address.script_pubkey()))
            .collect();

        let change_kind = self
            .change_address
            .as_ref()
            .map(|addr| ScriptKind::from_script(&addr.script_pubkey()))
            .unwrap_or(ScriptKind::P2tr);

        // Estimate vsize with change
        let vsize_with_change = CoinSelector::estimate_vsize(
            &input_kinds,
            &recipient_kinds,
            true,
            change_kind,
        );
        let fee_with_change = fee_rate.calculate_fee(vsize_with_change);

        let needed_with_change = total_recipient_sat + fee_with_change;

        let (final_change_sat, final_fee_sat, include_change) = if total_input_sat >= needed_with_change {
            let change = total_input_sat - needed_with_change;
            // Dust limit threshold check (~546 sats)
            if change >= 546 && self.change_address.is_some() {
                (change, fee_with_change, true)
            } else {
                // If change is dust or no change address provided, absorb into miner fee
                (0, total_input_sat - total_recipient_sat, false)
            }
        } else {
            // Check without change
            let vsize_no_change = CoinSelector::estimate_vsize(
                &input_kinds,
                &recipient_kinds,
                false,
                change_kind,
            );
            let fee_no_change = fee_rate.calculate_fee(vsize_no_change);
            if total_input_sat < total_recipient_sat + fee_no_change {
                return Err(GrossBitError::InsufficientFunds {
                    needed_sat: total_recipient_sat + fee_no_change,
                    available_sat: total_input_sat,
                });
            }
            (0, total_input_sat - total_recipient_sat, false)
        };

        let sequence = if self.enable_rbf {
            Sequence::ENABLE_RBF_NO_LOCKTIME
        } else {
            Sequence::MAX
        };

        // Construct inputs
        let tx_inputs: Vec<TxIn> = self
            .inputs
            .iter()
            .map(|u| TxIn {
                previous_output: u.outpoint,
                script_sig: ScriptBuf::new(),
                sequence,
                witness: Witness::new(),
            })
            .collect();

        // Construct outputs
        let mut tx_outputs: Vec<TxOut> = self
            .recipients
            .iter()
            .map(|r| TxOut {
                value: r.amount,
                script_pubkey: r.address.script_pubkey(),
            })
            .collect();

        if include_change {
            let change_addr = self.change_address.as_ref().unwrap();
            tx_outputs.push(TxOut {
                value: Amount::from_sat(final_change_sat),
                script_pubkey: change_addr.script_pubkey(),
            });
        }

        let tx = Transaction {
            version: Version::TWO,
            lock_time: self.lock_time,
            input: tx_inputs,
            output: tx_outputs,
        };

        Ok(BuiltTransaction {
            transaction: tx,
            spent_utxos: self.inputs,
            fee_sat: final_fee_sat,
            change_sat: final_change_sat,
            estimated_vsize: vsize_with_change,
        })
    }
}

/// The result of building an unsigned transaction.
#[derive(Debug, Clone)]
pub struct BuiltTransaction {
    pub transaction: Transaction,
    pub spent_utxos: Vec<GrossUtxo>,
    pub fee_sat: u64,
    pub change_sat: u64,
    pub estimated_vsize: usize,
}

impl BuiltTransaction {
    /// Serialize transaction to raw hex (unsigned or partially signed).
    pub fn to_hex(&self) -> String {
        use bitcoin::consensus::encode::serialize_hex;
        serialize_hex(&self.transaction)
    }
}
