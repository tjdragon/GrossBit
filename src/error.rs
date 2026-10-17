use thiserror::Error;

#[derive(Error, Debug)]
pub enum GrossBitError {
    #[error("UTXO error: {0}")]
    UtxoError(String),

    #[error("Insufficient funds: needed {needed_sat} sats, but only {available_sat} sats available")]
    InsufficientFunds {
        needed_sat: u64,
        available_sat: u64,
    },

    #[error("UTXO reservation error: {0}")]
    ReservationError(String),

    #[error("Transaction build error: {0}")]
    TxBuildError(String),

    #[error("Transaction parse error: {0}")]
    TxParseError(String),

    #[error("Script error: {0}")]
    ScriptError(String),

    #[error("Signing error: {0}")]
    SigningError(String),

    #[error("RPC error: {0}")]
    RpcError(String),

    #[error("Indexer error: {0}")]
    IndexerError(String),

    #[error("Bitcoin core error: {0}")]
    BitcoinError(#[from] bitcoin::address::ParseError),

    #[error("Consensus decode error: {0}")]
    ConsensusDecode(#[from] bitcoin::consensus::encode::Error),

    #[error("Secp256k1 error: {0}")]
    Secp256k1(#[from] bitcoin::secp256k1::Error),

    #[error("IO/Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, GrossBitError>;
