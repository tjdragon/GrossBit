use crate::error::Result;
use crate::model::{Balance, GrossUtxo, ScriptKind};
use crate::rpc::BitcoinRpcClient;
use crate::utxo::reservation::UtxoReservationManager;
use bitcoin::{Address, Block, Network, OutPoint, Transaction, Txid};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxDirection {
    Received,
    Sent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxHistoryEntry {
    pub txid: Txid,
    pub block_height: Option<u64>,
    pub timestamp: u64,
    pub address: String,
    pub amount_sat: u64,
    pub direction: TxDirection,
}

/// In-memory address indexer for tracking UTXOs, balances, and transaction history.
#[derive(Debug, Clone)]
pub struct AddressIndexer {
    network: Network,
    watched_addresses: Arc<RwLock<HashSet<String>>>,
    // Map address -> Outpoint -> GrossUtxo
    address_utxos: Arc<RwLock<HashMap<String, HashMap<OutPoint, GrossUtxo>>>>,
    // Map address -> list of history entries
    address_history: Arc<RwLock<HashMap<String, Vec<TxHistoryEntry>>>>,
    // Map Outpoint -> (address, amount_sat) so we can quickly detect spends
    known_outpoints: Arc<RwLock<HashMap<OutPoint, (String, u64)>>>,
    last_indexed_height: Arc<RwLock<u64>>,
    reservation_manager: UtxoReservationManager,
}

impl AddressIndexer {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            watched_addresses: Arc::new(RwLock::new(HashSet::new())),
            address_utxos: Arc::new(RwLock::new(HashMap::new())),
            address_history: Arc::new(RwLock::new(HashMap::new())),
            known_outpoints: Arc::new(RwLock::new(HashMap::new())),
            last_indexed_height: Arc::new(RwLock::new(0)),
            reservation_manager: UtxoReservationManager::new(),
        }
    }

    /// Register an address to be watched by the indexer.
    pub fn watch_address(&self, address: &Address) {
        let addr_str = address.to_string();
        self.watched_addresses.write().insert(addr_str.clone());
        self.address_utxos.write().entry(addr_str.clone()).or_default();
        self.address_history.write().entry(addr_str).or_default();
    }

    /// Check if an address is watched.
    pub fn is_watching(&self, address_str: &str) -> bool {
        self.watched_addresses.read().contains(address_str)
    }

    /// Retrieve all unspent UTXOs for a watched address (excluding currently reserved ones).
    pub fn get_available_utxos(&self, address: &Address) -> Vec<GrossUtxo> {
        self.reservation_manager.clean_expired();
        let addr_str = address.to_string();
        let utxos_guard = self.address_utxos.read();
        match utxos_guard.get(&addr_str) {
            Some(map) => map
                .values()
                .filter(|u| !self.reservation_manager.is_reserved(&u.outpoint))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Retrieve all UTXOs for an address (including reserved).
    pub fn get_all_utxos(&self, address: &Address) -> Vec<GrossUtxo> {
        let addr_str = address.to_string();
        let utxos_guard = self.address_utxos.read();
        match utxos_guard.get(&addr_str) {
            Some(map) => map.values().cloned().collect(),
            None => Vec::new(),
        }
    }

    /// Get balance for a specific address.
    pub fn get_balance(&self, address: &Address) -> Balance {
        self.reservation_manager.clean_expired();
        let addr_str = address.to_string();
        let utxos_guard = self.address_utxos.read();

        let mut confirmed = 0u64;
        let mut unconfirmed = 0u64;
        let mut reserved = 0u64;

        if let Some(map) = utxos_guard.get(&addr_str) {
            for (op, u) in map.iter() {
                let sats = u.satoshis();
                if self.reservation_manager.is_reserved(op) {
                    reserved += sats;
                }
                if u.confirmations > 0 {
                    confirmed += sats;
                } else {
                    unconfirmed += sats;
                }
            }
        }

        Balance {
            confirmed_sat: confirmed,
            unconfirmed_sat: unconfirmed,
            reserved_sat: reserved,
        }
    }

    /// Get transaction history for an address.
    pub fn get_history(&self, address: &Address) -> Vec<TxHistoryEntry> {
        let addr_str = address.to_string();
        self.address_history
            .read()
            .get(&addr_str)
            .cloned()
            .unwrap_or_default()
    }

    /// Process a transaction and update UTXO state and history.
    pub fn index_transaction(
        &self,
        tx: &Transaction,
        block_height: Option<u64>,
        timestamp: u64,
    ) {
        let txid = tx.compute_txid();
        let watched = self.watched_addresses.read();

        // 1. Check inputs to see if they spend any of our known UTXOs
        let mut known_op_guard = self.known_outpoints.write();
        let mut utxos_guard = self.address_utxos.write();
        let mut history_guard = self.address_history.write();

        for tx_in in &tx.input {
            if let Some((addr_str, amount_sat)) = known_op_guard.remove(&tx_in.previous_output) {
                // Remove from UTXO set
                if let Some(user_utxos) = utxos_guard.get_mut(&addr_str) {
                    user_utxos.remove(&tx_in.previous_output);
                }
                // Record Sent entry in history
                if let Some(hist) = history_guard.get_mut(&addr_str) {
                    hist.push(TxHistoryEntry {
                        txid,
                        block_height,
                        timestamp,
                        address: addr_str.clone(),
                        amount_sat,
                        direction: TxDirection::Sent,
                    });
                }
            }
        }

        // 2. Check outputs for transfers to watched addresses
        for (vout, tx_out) in tx.output.iter().enumerate() {
            if let Ok(addr) = Address::from_script(&tx_out.script_pubkey, self.network) {
                let addr_str = addr.to_string();
                if watched.contains(&addr_str) {
                    let outpoint = OutPoint::new(txid, vout as u32);
                    let utxo = GrossUtxo {
                        outpoint,
                        amount: tx_out.value,
                        script_pubkey: tx_out.script_pubkey.clone(),
                        address: Some(addr_str.clone()),
                        script_kind: ScriptKind::from_script(&tx_out.script_pubkey),
                        confirmations: if block_height.is_some() { 1 } else { 0 },
                        is_coinbase: tx.is_coinbase(),
                    };

                    known_op_guard.insert(outpoint, (addr_str.clone(), tx_out.value.to_sat()));
                    utxos_guard
                        .entry(addr_str.clone())
                        .or_default()
                        .insert(outpoint, utxo);

                    if let Some(hist) = history_guard.get_mut(&addr_str) {
                        hist.push(TxHistoryEntry {
                            txid,
                            block_height,
                            timestamp,
                            address: addr_str.clone(),
                            amount_sat: tx_out.value.to_sat(),
                            direction: TxDirection::Received,
                        });
                    }
                }
            }
        }
    }

    /// Index an entire block.
    pub fn index_block(&self, block: &Block, height: u64) {
        let timestamp = block.header.time as u64;
        for tx in &block.txdata {
            self.index_transaction(tx, Some(height), timestamp);
        }
        *self.last_indexed_height.write() = height;
    }

    /// Sync blocks from a Bitcoin node RPC client across a given height range.
    pub async fn sync_blocks(
        &self,
        rpc: &BitcoinRpcClient,
        from_height: u64,
        to_height: u64,
    ) -> Result<()> {
        for height in from_height..=to_height {
            let hash = rpc.get_block_hash(height).await?;
            let block = rpc.get_block(&hash).await?;
            self.index_block(&block, height);
        }
        Ok(())
    }

    pub fn last_indexed_height(&self) -> u64 {
        *self.last_indexed_height.read()
    }

    pub fn reservation_manager(&self) -> &UtxoReservationManager {
        &self.reservation_manager
    }
}
