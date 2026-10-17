pub mod error;
pub mod indexer;
pub mod model;
pub mod provider;
pub mod rpc;
pub mod storage;
pub mod tx;
pub mod utxo;

// Re-export core dependencies for downstream convenience
pub use bitcoin;
pub use error::{GrossBitError, Result};
pub use indexer::{AddressIndexer, TxDirection, TxHistoryEntry};
pub use model::{
    Balance, FeeRate, GrossUtxo, ParsedTx, ParsedTxIn, ParsedTxOut, ScriptKind, UtxoReservation,
};
pub use provider::{EsploraAddressStats, EsploraChainStats, EsploraClient, EsploraUtxo, DEFAULT_TESTNET4_ESPLORA_URL};
pub use rpc::{BitcoinRpcClient, RpcConfig};
pub use storage::PostgresUtxoStore;
pub use tx::{BuiltTransaction, Recipient, TxBuilder, TxParser, TxSigner};
pub use utxo::reservation::UtxoReservationManager;
pub use utxo::selection::{CoinSelectionStrategy, CoinSelector, SelectedCoins};
pub use utxo::UtxoPool;

use bitcoin::secp256k1::SecretKey;
use bitcoin::{Address, Amount, Network, Txid};
use chrono::Duration;

/// High-level client coordinating UTXO selection, reservation (Postgres or Memory),
/// transaction building, signing, indexing, and external providers (Esplora / Electrum)
/// on Bitcoin Testnet4.
#[derive(Debug, Clone)]
pub struct GrossBit {
    pub network: Network,
    pub pool: UtxoPool,
    pub indexer: AddressIndexer,
    pub rpc: Option<BitcoinRpcClient>,
    pub esplora: Option<EsploraClient>,
    pub postgres: Option<PostgresUtxoStore>,
}

impl GrossBit {
    /// Initialize GrossBit pre-configured for Bitcoin Testnet4.
    pub fn new_testnet4() -> Self {
        Self {
            network: Network::Testnet4,
            pool: UtxoPool::new(),
            indexer: AddressIndexer::new(Network::Testnet4),
            rpc: None,
            esplora: Some(EsploraClient::new_testnet4()),
            postgres: None,
        }
    }

    /// Initialize GrossBit with a specific network (e.g. Testnet, Testnet4, Signet, Regtest).
    pub fn new(network: Network) -> Self {
        Self {
            network,
            pool: UtxoPool::new(),
            indexer: AddressIndexer::new(network),
            rpc: None,
            esplora: None,
            postgres: None,
        }
    }

    /// Attach an Esplora external provider client.
    pub fn with_esplora(mut self, esplora: EsploraClient) -> Self {
        self.esplora = Some(esplora);
        self
    }

    /// Attach a PostgreSQL storage engine for persistent UTXO tracking and reservations.
    pub fn with_postgres(mut self, postgres: PostgresUtxoStore) -> Self {
        self.postgres = Some(postgres);
        self
    }

    /// Attach a Bitcoin node RPC client.
    pub fn with_rpc(mut self, rpc: BitcoinRpcClient) -> Self {
        self.rpc = Some(rpc);
        self
    }

    /// Watch an address in the indexer.
    pub fn watch_address(&self, address: &Address) {
        self.indexer.watch_address(address);
    }

    /// Sync address UTXOs from Testnet4 Esplora provider into PostgreSQL (if configured) and in-memory pool.
    pub async fn sync_address(&self, address: &Address) -> Result<usize> {
        let esplora = self
            .esplora
            .as_ref()
            .ok_or_else(|| GrossBitError::RpcError("Esplora client not configured".into()))?;

        let utxos = esplora.get_address_utxos(address).await?;
        let count = utxos.len();

        if let Some(pg) = &self.postgres {
            for u in &utxos {
                pg.upsert_utxo(u).await?;
            }
        }

        for u in utxos {
            self.pool.insert(u);
        }

        Ok(count)
    }

    /// Fetch spendable balance for an address (from Postgres if available, else in-memory pool).
    pub async fn get_balance(&self, address: &Address) -> Result<Balance> {
        if let Some(pg) = &self.postgres {
            pg.get_balance(&address.to_string()).await
        } else {
            Ok(self.indexer.get_balance(address))
        }
    }

    /// Build a transaction with UTXO selection and anti-double-spend reservation.
    /// Change output defaults to P2TR (Taproot).
    pub async fn create_payment(
        &self,
        recipient_address: Address,
        amount_sat: u64,
        change_address: Address,
        fee_rate: FeeRate,
        strategy: CoinSelectionStrategy,
        reservation_ttl: Duration,
    ) -> Result<(BuiltTransaction, String)> {
        let recipient_kind = ScriptKind::from_script(&recipient_address.script_pubkey());
        let change_kind = ScriptKind::from_script(&change_address.script_pubkey());

        if let Some(pg) = &self.postgres {
            // PostgreSQL-backed flow
            let available_utxos = pg.get_available_utxos(&change_address.to_string()).await?;
            let selected = CoinSelector::select(
                available_utxos,
                amount_sat,
                fee_rate,
                recipient_kind,
                change_kind,
                strategy,
            )?;

            let outpoints: Vec<bitcoin::OutPoint> =
                selected.selected_utxos.iter().map(|u| u.outpoint).collect();

            let reservation_id = pg
                .reserve_utxos(
                    &outpoints,
                    reservation_ttl.num_seconds(),
                    Some(&format!("Payment to {}", recipient_address)),
                )
                .await?;

            let builder = TxBuilder::new(self.network)
                .add_inputs(selected.selected_utxos)
                .add_recipient(recipient_address, Amount::from_sat(amount_sat))
                .change_address(change_address)
                .fee_rate(fee_rate)
                .enable_rbf(true);

            let built_tx = match builder.build() {
                Ok(tx) => tx,
                Err(e) => {
                    let _ = pg.release_reservation(&reservation_id).await;
                    return Err(e);
                }
            };

            Ok((built_tx, reservation_id))
        } else {
            // In-memory pool flow
            let (selected, reservation_id) = self.pool.select_and_reserve(
                amount_sat,
                fee_rate,
                recipient_kind,
                change_kind,
                strategy,
                reservation_ttl,
                Some(format!("Payment to {}", recipient_address)),
            )?;

            let builder = TxBuilder::new(self.network)
                .add_inputs(selected.selected_utxos)
                .add_recipient(recipient_address, Amount::from_sat(amount_sat))
                .change_address(change_address)
                .fee_rate(fee_rate)
                .enable_rbf(true);

            let built_tx = match builder.build() {
                Ok(tx) => tx,
                Err(e) => {
                    self.pool.release_reservation(&reservation_id);
                    return Err(e);
                }
            };

            Ok((built_tx, reservation_id))
        }
    }

    /// Sign a transaction (P2TR or P2WPKH) and broadcast via external Esplora provider on Testnet4.
    pub async fn sign_and_broadcast_esplora(
        &self,
        built_tx: BuiltTransaction,
        reservation_id: &str,
        secret_key: &SecretKey,
    ) -> Result<Txid> {
        let esplora = self
            .esplora
            .as_ref()
            .ok_or_else(|| GrossBitError::RpcError("Esplora client not configured".into()))?;

        // 1. Sign transaction (P2WPKH or P2TR)
        let signed_hex = TxSigner::sign_transaction(built_tx, secret_key)?;

        // 2. Broadcast to Testnet4 via Esplora
        let txid = esplora.broadcast_tx(&signed_hex).await?;

        // 3. Commit reservation
        if let Some(pg) = &self.postgres {
            let _ = pg.commit_reservation(reservation_id).await;
        } else {
            self.pool.commit_reservation(reservation_id);
        }

        Ok(txid)
    }

    /// Parse any raw transaction hex into a rich parsed model.
    pub fn parse_raw_hex(&self, hex_str: &str) -> Result<ParsedTx> {
        TxParser::parse_hex(hex_str, self.network)
    }
}
