use crate::error::{GrossBitError, Result};
use crate::model::FeeRate;
use bitcoin::consensus::encode::deserialize_hex;
use bitcoin::{Block, BlockHash, Network, Transaction, Txid};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub timeout: Duration,
    pub network: Network,
}

impl RpcConfig {
    pub fn new(url: impl Into<String>, network: Network) -> Self {
        Self {
            url: url.into(),
            user: None,
            password: None,
            timeout: Duration::from_secs(30),
            network,
        }
    }

    pub fn with_auth(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self.password = Some(password.into());
        self
    }
}

/// Async Bitcoin Core JSON-RPC client.
#[derive(Debug, Clone)]
pub struct BitcoinRpcClient {
    client: reqwest::Client,
    config: RpcConfig,
}

impl BitcoinRpcClient {
    pub fn new(config: RpcConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let (Some(user), Some(pass)) = (&config.user, &config.password) {
            let encoded = base64_encode(format!("{}:{}", user, pass).as_bytes());
            let auth_header = format!("Basic {}", encoded);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_header).map_err(|e| {
                    GrossBitError::RpcError(format!("Invalid auth header: {}", e))
                })?,
            );
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(config.timeout)
            .build()?;

        Ok(Self { client, config })
    }

    /// Generic JSON-RPC request helper.
    pub async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T> {
        let payload = json!({
            "jsonrpc": "1.0",
            "id": "grossbit",
            "method": method,
            "params": params,
        });

        let resp = self
            .client
            .post(&self.config.url)
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            return Err(GrossBitError::RpcError(format!(
                "HTTP error {}: {}",
                status, error_text
            )));
        }

        let json_resp: Value = resp.json().await?;
        if let Some(err) = json_resp.get("error").filter(|e| !e.is_null()) {
            return Err(GrossBitError::RpcError(err.to_string()));
        }

        let result = json_resp
            .get("result")
            .ok_or_else(|| GrossBitError::RpcError("Missing result in JSON-RPC response".into()))?;

        serde_json::from_value(result.clone()).map_err(|e| {
            GrossBitError::RpcError(format!("Failed to parse RPC response: {}", e))
        })
    }

    /// Get current blockchain block height.
    pub async fn get_block_count(&self) -> Result<u64> {
        self.call("getblockcount", vec![]).await
    }

    /// Get best block hash.
    pub async fn get_best_block_hash(&self) -> Result<BlockHash> {
        let hash_hex: String = self.call("getbestblockhash", vec![]).await?;
        hash_hex
            .parse::<BlockHash>()
            .map_err(|e| GrossBitError::RpcError(format!("Invalid block hash: {}", e)))
    }

    /// Get block hash at specific height.
    pub async fn get_block_hash(&self, height: u64) -> Result<BlockHash> {
        let hash_hex: String = self.call("getblockhash", vec![json!(height)]).await?;
        hash_hex
            .parse::<BlockHash>()
            .map_err(|e| GrossBitError::RpcError(format!("Invalid block hash: {}", e)))
    }

    /// Fetch and deserialize a full block by its hash.
    pub async fn get_block(&self, hash: &BlockHash) -> Result<Block> {
        // Verbosity 0 returns raw serialized block hex
        let block_hex: String = self.call("getblock", vec![json!(hash.to_string()), json!(0)]).await?;
        let block: Block = deserialize_hex(&block_hex)
            .map_err(|e| GrossBitError::RpcError(format!("Failed to decode block: {}", e)))?;
        Ok(block)
    }

    /// Fetch raw transaction by TXID.
    pub async fn get_raw_transaction(&self, txid: &Txid) -> Result<Transaction> {
        let tx_hex: String = self
            .call("getrawtransaction", vec![json!(txid.to_string()), json!(false)])
            .await?;
        let tx: Transaction = deserialize_hex(&tx_hex)
            .map_err(|e| GrossBitError::RpcError(format!("Failed to decode tx: {}", e)))?;
        Ok(tx)
    }

    /// Broadcast a signed raw transaction hex string to the network.
    pub async fn send_raw_transaction(&self, hex: &str) -> Result<Txid> {
        let clean_hex = hex.trim().trim_start_matches("0x");
        let txid_str: String = self.call("sendrawtransaction", vec![json!(clean_hex)]).await?;
        txid_str
            .parse::<Txid>()
            .map_err(|e| GrossBitError::RpcError(format!("Invalid txid returned: {}", e)))
    }

    /// Estimate smart fee rate in sat/vB for target confirmation blocks.
    pub async fn estimate_smart_fee(&self, target_blocks: u16) -> Result<FeeRate> {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct FeeResponse {
            feerate: Option<f64>, // BTC/kvB
            errors: Option<Vec<String>>,
        }

        let resp: FeeResponse = self
            .call("estimatesmartfee", vec![json!(target_blocks)])
            .await?;

        if let Some(btc_per_kvb) = resp.feerate {
            // 1 BTC/kvB = 100,000,000 sats / 1000 vB = 100,000 sat/vB
            let sat_per_vb = (btc_per_kvb * 100_000.0).max(1.0);
            Ok(FeeRate::from_sat_per_vb(sat_per_vb))
        } else {
            // Default fallback if node has insufficient stats (e.g. fresh testnet node)
            Ok(FeeRate::from_sat_per_vb(2.0))
        }
    }
}

// Simple base64 encode helper for basic auth
fn base64_encode(data: &[u8]) -> String {
    const CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as usize;
        let b1 = if i + 1 < data.len() { data[i + 1] as usize } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] as usize } else { 0 };

        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(CHARSET[(triple >> 18) & 0x3F] as char);
        out.push(CHARSET[(triple >> 12) & 0x3F] as char);

        if i + 1 < data.len() {
            out.push(CHARSET[(triple >> 6) & 0x3F] as char);
        } else {
            out.push('=');
        }

        if i + 2 < data.len() {
            out.push(CHARSET[triple & 0x3F] as char);
        } else {
            out.push('=');
        }

        i += 3;
    }
    out
}
