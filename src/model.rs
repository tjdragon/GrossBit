use bitcoin::{Address, Amount, Network, OutPoint, ScriptBuf, Txid};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Comprehensive script classification to support receiving and parsing all Bitcoin script types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScriptKind {
    /// Pay-to-Taproot (BIP 341, SegWit v1) - DEFAULT for modern testnet4 tx building
    P2tr,
    /// Pay-to-Witness-Public-Key-Hash (BIP 141, SegWit v0)
    P2wpkh,
    /// Pay-to-Witness-Script-Hash (BIP 141, SegWit v0)
    P2wsh,
    /// Pay-to-Script-Hash (BIP 16, Legacy/Nested SegWit)
    P2sh,
    /// Pay-to-Public-Key-Hash (Legacy Base58)
    P2pkh,
    /// OP_RETURN provably unspendable data carrier
    OpReturn,
    /// Bare Multisig
    Multisig,
    /// Other / Custom non-standard script
    Unknown,
}

impl ScriptKind {
    pub fn from_script(script: &ScriptBuf) -> Self {
        if script.is_p2tr() {
            ScriptKind::P2tr
        } else if script.is_p2wpkh() {
            ScriptKind::P2wpkh
        } else if script.is_p2wsh() {
            ScriptKind::P2wsh
        } else if script.is_p2sh() {
            ScriptKind::P2sh
        } else if script.is_p2pkh() {
            ScriptKind::P2pkh
        } else if script.is_op_return() {
            ScriptKind::OpReturn
        } else if script.is_multisig() {
            ScriptKind::Multisig
        } else {
            ScriptKind::Unknown
        }
    }

    /// Estimated input virtual size (vbytes) when spent.
    pub fn estimated_input_vsize(&self) -> usize {
        match self {
            ScriptKind::P2tr => 58,    // BIP 341 keypath spend (~57.5 vB)
            ScriptKind::P2wpkh => 68,  // BIP 141 SegWit v0 (~67.5-68 vB)
            ScriptKind::P2sh => 91,    // P2SH-P2WPKH (~91 vB)
            ScriptKind::P2wsh => 105,  // P2WSH typical spend
            ScriptKind::P2pkh => 148,  // Legacy input (~148 vB)
            _ => 150,
        }
    }

    /// Estimated output virtual size (vbytes).
    pub fn estimated_output_vsize(&self) -> usize {
        match self {
            ScriptKind::P2tr => 43,
            ScriptKind::P2wpkh => 31,
            ScriptKind::P2wsh => 43,
            ScriptKind::P2sh => 32,
            ScriptKind::P2pkh => 34,
            ScriptKind::OpReturn => 10,
            _ => 34,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ScriptKind::P2tr => "p2tr",
            ScriptKind::P2wpkh => "p2wpkh",
            ScriptKind::P2wsh => "p2wsh",
            ScriptKind::P2sh => "p2sh",
            ScriptKind::P2pkh => "p2pkh",
            ScriptKind::OpReturn => "op_return",
            ScriptKind::Multisig => "multisig",
            ScriptKind::Unknown => "unknown",
        }
    }

    pub fn from_str_name(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "p2tr" => ScriptKind::P2tr,
            "p2wpkh" => ScriptKind::P2wpkh,
            "p2wsh" => ScriptKind::P2wsh,
            "p2sh" => ScriptKind::P2sh,
            "p2pkh" => ScriptKind::P2pkh,
            "op_return" => ScriptKind::OpReturn,
            "multisig" => ScriptKind::Multisig,
            _ => ScriptKind::Unknown,
        }
    }
}

/// A UTXO tracked by GrossBit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrossUtxo {
    pub outpoint: OutPoint,
    pub amount: Amount,
    pub script_pubkey: ScriptBuf,
    pub address: Option<String>,
    pub script_kind: ScriptKind,
    pub confirmations: u32,
    pub is_coinbase: bool,
}

impl GrossUtxo {
    pub fn new(
        outpoint: OutPoint,
        amount: Amount,
        script_pubkey: ScriptBuf,
        network: Network,
        confirmations: u32,
    ) -> Self {
        let script_kind = ScriptKind::from_script(&script_pubkey);
        let address = Address::from_script(&script_pubkey, network)
            .ok()
            .map(|a| a.to_string());
        Self {
            outpoint,
            amount,
            script_pubkey,
            address,
            script_kind,
            confirmations,
            is_coinbase: false,
        }
    }

    pub fn satoshis(&self) -> u64 {
        self.amount.to_sat()
    }
}

/// Reservation details for a locked UTXO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoReservation {
    pub reservation_id: String,
    pub outpoint: OutPoint,
    pub amount_sat: u64,
    pub reserved_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub context: Option<String>,
}

impl UtxoReservation {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// Target fee rate in satoshis per virtual byte (sat/vB).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct FeeRate {
    sat_per_vb: f64,
}

impl FeeRate {
    pub fn from_sat_per_vb(sat_per_vb: f64) -> Self {
        assert!(sat_per_vb > 0.0, "Fee rate must be greater than zero");
        Self { sat_per_vb }
    }

    pub fn sat_per_vb(&self) -> f64 {
        self.sat_per_vb
    }

    /// Calculate total fee in satoshis for a given vsize.
    pub fn calculate_fee(&self, vsize: usize) -> u64 {
        (self.sat_per_vb * (vsize as f64)).ceil() as u64
    }
}

/// Aggregated account or address balance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub confirmed_sat: u64,
    pub unconfirmed_sat: u64,
    pub reserved_sat: u64,
}

impl Balance {
    pub fn total_sat(&self) -> u64 {
        self.confirmed_sat.saturating_add(self.unconfirmed_sat)
    }

    pub fn spendable_sat(&self) -> u64 {
        self.total_sat().saturating_sub(self.reserved_sat)
    }
}

/// Parsed transaction input with decoded script and witness fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTxIn {
    pub txid: Txid,
    pub vout: u32,
    pub sequence: u32,
    pub script_sig_hex: String,
    pub witness_items_hex: Vec<String>,
    pub script_kind: Option<ScriptKind>,
}

/// Parsed transaction output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTxOut {
    pub vout: u32,
    pub amount_sat: u64,
    pub script_pubkey_hex: String,
    pub address: Option<String>,
    pub script_kind: ScriptKind,
}

/// Parsed Bitcoin transaction representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTx {
    pub txid: Txid,
    pub version: i32,
    pub lock_time: u32,
    pub size: usize,
    pub vsize: usize,
    pub weight: u64,
    pub inputs: Vec<ParsedTxIn>,
    pub outputs: Vec<ParsedTxOut>,
}
