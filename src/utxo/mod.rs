pub mod reservation;
pub mod selection;

use crate::error::Result;
use crate::model::{Balance, FeeRate, GrossUtxo, ScriptKind};
use bitcoin::OutPoint;
use chrono::Duration;
use parking_lot::RwLock;
use reservation::UtxoReservationManager;
use selection::{CoinSelectionStrategy, CoinSelector, SelectedCoins};
use std::collections::HashMap;
use std::sync::Arc;

/// A managed pool of UTXOs with anti-double-spend reservation capabilities.
#[derive(Debug, Clone)]
pub struct UtxoPool {
    utxos: Arc<RwLock<HashMap<OutPoint, GrossUtxo>>>,
    reservation_manager: UtxoReservationManager,
}

impl Default for UtxoPool {
    fn default() -> Self {
        Self::new()
    }
}

impl UtxoPool {
    pub fn new() -> Self {
        Self {
            utxos: Arc::new(RwLock::new(HashMap::new())),
            reservation_manager: UtxoReservationManager::new(),
        }
    }

    /// Add a UTXO into the pool.
    pub fn insert(&self, utxo: GrossUtxo) {
        self.utxos.write().insert(utxo.outpoint, utxo);
    }

    /// Remove a UTXO from the pool (e.g. once confirmed spent).
    pub fn remove(&self, outpoint: &OutPoint) -> Option<GrossUtxo> {
        self.utxos.write().remove(outpoint)
    }

    /// Retrieve all unreserved, available UTXOs.
    pub fn available_utxos(&self) -> Vec<GrossUtxo> {
        self.reservation_manager.clean_expired();
        let map = self.utxos.read();
        map.values()
            .filter(|u| !self.reservation_manager.is_reserved(&u.outpoint))
            .cloned()
            .collect()
    }

    /// Get current balance breakdown (confirmed, unconfirmed, reserved).
    pub fn balance(&self) -> Balance {
        self.reservation_manager.clean_expired();
        let map = self.utxos.read();
        let mut confirmed_sat = 0u64;
        let mut unconfirmed_sat = 0u64;
        let mut reserved_sat = 0u64;

        for (op, utxo) in map.iter() {
            let sats = utxo.satoshis();
            if self.reservation_manager.is_reserved(op) {
                reserved_sat += sats;
            }
            if utxo.confirmations > 0 {
                confirmed_sat += sats;
            } else {
                unconfirmed_sat += sats;
            }
        }

        Balance {
            confirmed_sat,
            unconfirmed_sat,
            reserved_sat,
        }
    }

    /// Atomically select UTXOs and reserve them under a lease to prevent double-spending.
    /// Returns the selected coins result and the reservation ID.
    pub fn select_and_reserve(
        &self,
        target_sat: u64,
        fee_rate: FeeRate,
        recipient_kind: ScriptKind,
        change_kind: ScriptKind,
        strategy: CoinSelectionStrategy,
        ttl: Duration,
        context: Option<String>,
    ) -> Result<(SelectedCoins, String)> {
        let available = self.available_utxos();
        let selected = CoinSelector::select(
            available,
            target_sat,
            fee_rate,
            recipient_kind,
            change_kind,
            strategy,
        )?;

        let outpoints: Vec<OutPoint> = selected.selected_utxos.iter().map(|u| u.outpoint).collect();
        let amounts: Vec<u64> = selected.selected_utxos.iter().map(|u| u.satoshis()).collect();

        let res_id = self.reservation_manager.reserve(
            &outpoints,
            &amounts,
            ttl,
            context,
        )?;

        Ok((selected, res_id))
    }

    /// Release an active reservation (e.g. if transaction creation was canceled).
    pub fn release_reservation(&self, reservation_id: &str) -> bool {
        self.reservation_manager.release(reservation_id)
    }

    /// Commit an active reservation (remove spent UTXOs from pool permanently).
    pub fn commit_reservation(&self, reservation_id: &str) -> bool {
        let active = self.reservation_manager.list_active();
        let to_remove: Vec<OutPoint> = active
            .into_iter()
            .filter(|r| r.reservation_id == reservation_id)
            .map(|r| r.outpoint)
            .collect();

        for op in &to_remove {
            self.remove(op);
        }

        self.reservation_manager.release(reservation_id)
    }

    pub fn reservation_manager(&self) -> &UtxoReservationManager {
        &self.reservation_manager
    }
}
