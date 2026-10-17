use crate::error::{GrossBitError, Result};
use crate::model::{FeeRate, GrossUtxo, ScriptKind};
use crate::storage::PostgresUtxoStore;
use bitcoin::{Address, Amount, Network, OutPoint, Txid};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

pub const DEFAULT_TESTNET4_ESPLORA_URL: &str = "https://mempool.space/testnet4/api";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsploraUtxoStatus {
    pub confirmed: bool,
    pub block_height: Option<u64>,
    pub block_hash: Option<String>,
    pub block_time: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsploraUtxo {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
    pub status: EsploraUtxoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsploraChainStats {
    pub funded_txo_count: u64,
    pub funded_txo_sum: u64,
    pub spent_txo_count: u64,
    pub spent_txo_sum: u64,
    pub tx_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsploraAddressStats {
    pub address: String,
    pub chain_stats: EsploraChainStats,
    pub mempool_stats: EsploraChainStats,
}

/// High-performance asynchronous Esplora client for Bitcoin Testnet4 (and other networks).
#[derive(Debug, Clone)]
pub struct EsploraClient {
    base_url: String,
    network: Network,
    client: reqwest::Client,
}

impl EsploraClient {
    /// Create a new Esplora client targeting Testnet4 using the default public Mempool endpoint.
    pub fn new_testnet4() -> Self {
        Self::new(DEFAULT_TESTNET4_ESPLORA_URL, Network::Testnet4)
    }

    /// Create a new Esplora client with a custom URL and network.
    pub fn new(base_url: impl Into<String>, network: Network) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build reqwest client");

        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            network,
            client,
        }
    }

    /// The network this client targets (e.g. Testnet4).
    pub fn network(&self) -> Network {
        self.network
    }

    /// Get current tip height on Testnet4.
    pub async fn get_tip_height(&self) -> Result<u64> {
        let url = format!("{}/blocks/tip/height", self.base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(GrossBitError::Network(resp.error_for_status().unwrap_err()));
        }
        let height_str = resp.text().await?;
        height_str
            .trim()
            .parse::<u64>()
            .map_err(|e| GrossBitError::RpcError(format!("Failed to parse tip height: {}", e)))
    }

    /// Fetch all active UTXOs for an address directly from Esplora.
    pub async fn get_address_utxos(&self, address: &Address) -> Result<Vec<GrossUtxo>> {
        let url = format!("{}/address/{}/utxo", self.base_url, address);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            let err_txt = resp.text().await.unwrap_or_default();
            return Err(GrossBitError::RpcError(format!(
                "Esplora get_address_utxos error: {}",
                err_txt
            )));
        }

        let esplora_utxos: Vec<EsploraUtxo> = resp.json().await?;
        let tip_height = self.get_tip_height().await.unwrap_or(0);
        let script_pubkey = address.script_pubkey();
        let script_kind = ScriptKind::from_script(&script_pubkey);

        let mut result = Vec::new();
        for u in esplora_utxos {
            let txid = Txid::from_str(&u.txid)
                .map_err(|e| GrossBitError::RpcError(format!("Invalid txid from Esplora: {}", e)))?;
            let confirmations = if let Some(h) = u.status.block_height {
                if tip_height >= h {
                    (tip_height - h + 1) as u32
                } else {
                    1
                }
            } else {
                0
            };

            result.push(GrossUtxo {
                outpoint: OutPoint::new(txid, u.vout),
                amount: Amount::from_sat(u.value),
                script_pubkey: script_pubkey.clone(),
                address: Some(address.to_string()),
                script_kind,
                confirmations,
                is_coinbase: false,
            });
        }

        Ok(result)
    }

    /// Fetch stats and balances for an address.
    pub async fn get_address_stats(&self, address: &Address) -> Result<EsploraAddressStats> {
        let url = format!("{}/address/{}", self.base_url, address);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            let err_txt = resp.text().await.unwrap_or_default();
            return Err(GrossBitError::RpcError(format!(
                "Esplora get_address_stats error: {}",
                err_txt
            )));
        }
        let stats: EsploraAddressStats = resp.json().await?;
        Ok(stats)
    }

    /// Broadcast a signed raw transaction hex string to Testnet4.
    pub async fn broadcast_tx(&self, tx_hex: &str) -> Result<Txid> {
        let clean_hex = tx_hex.trim().trim_start_matches("0x");
        let url = format!("{}/tx", self.base_url);
        let resp = self
            .client
            .post(&url)
            .body(clean_hex.to_string())
            .send()
            .await?;

        if !resp.status().is_success() {
            let err_msg = resp.text().await.unwrap_or_default();
            return Err(GrossBitError::RpcError(format!(
                "Failed to broadcast tx to Testnet4: {}",
                err_msg
            )));
        }

        let txid_str = resp.text().await?;
        txid_str
            .trim()
            .parse::<Txid>()
            .map_err(|e| GrossBitError::RpcError(format!("Invalid txid returned: {}", e)))
    }

    /// Fetch raw transaction hex by TXID.
    pub async fn get_tx_hex(&self, txid: &Txid) -> Result<String> {
        let url = format!("{}/tx/{}/hex", self.base_url, txid);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(GrossBitError::RpcError(format!(
                "Failed to fetch tx hex for {}",
                txid
            )));
        }
        let hex = resp.text().await?;
        Ok(hex.trim().to_string())
    }

    /// Fetch dynamic fee estimates from Testnet4 mempool (returns map of target blocks -> sat/vB).
    pub async fn get_fee_estimates(&self) -> Result<HashMap<String, f64>> {
        let url = format!("{}/fee-estimates", self.base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            // Return conservative testnet fallback
            let mut fallback = HashMap::new();
            fallback.insert("1".to_string(), 3.0);
            fallback.insert("3".to_string(), 2.0);
            fallback.insert("6".to_string(), 1.5);
            return Ok(fallback);
        }
        let estimates: HashMap<String, f64> = resp.json().await.unwrap_or_default();
        Ok(estimates)
    }

    /// Get estimated fee rate for a specific confirmation block target (e.g. 1, 3, or 6).
    pub async fn estimate_fee_rate(&self, target_blocks: u16) -> Result<FeeRate> {
        let estimates = self.get_fee_estimates().await?;
        let key = target_blocks.to_string();
        let sat_per_vb = estimates.get(&key).copied().unwrap_or(2.0).max(1.0);
        Ok(FeeRate::from_sat_per_vb(sat_per_vb))
    }

    /// Sync an address's UTXOs from Testnet4 Esplora directly into the PostgreSQL storage engine.
    pub async fn sync_to_postgres(
        &self,
        address: &Address,
        store: &PostgresUtxoStore,
    ) -> Result<usize> {
        let utxos = self.get_address_utxos(address).await?;
        let count = utxos.len();
        for u in &utxos {
            store.upsert_utxo(u).await?;
        }
        Ok(count)
    }
}
