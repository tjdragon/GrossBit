use crate::error::{GrossBitError, Result};
use crate::model::UtxoReservation;
use bitcoin::OutPoint;
use chrono::{Duration, Utc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// In-memory lease-based UTXO reservation manager to prevent double-spending in concurrent environments.
#[derive(Debug, Clone)]
pub struct UtxoReservationManager {
    // Map from OutPoint to UtxoReservation
    reservations_by_outpoint: Arc<RwLock<HashMap<OutPoint, UtxoReservation>>>,
    // Map from ReservationId to list of OutPoints
    reservations_by_id: Arc<RwLock<HashMap<String, Vec<OutPoint>>>>,
}

impl Default for UtxoReservationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl UtxoReservationManager {
    pub fn new() -> Self {
        Self {
            reservations_by_outpoint: Arc::new(RwLock::new(HashMap::new())),
            reservations_by_id: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Clean up expired reservations.
    pub fn clean_expired(&self) {
        let now = Utc::now();
        let mut by_outpoint = self.reservations_by_outpoint.write();
        let mut by_id = self.reservations_by_id.write();

        // Find expired reservation IDs
        let mut expired_ids = Vec::new();
        for (id, outpoints) in by_id.iter() {
            let is_any_expired = outpoints.iter().any(|op| {
                by_outpoint
                    .get(op)
                    .map(|res| res.is_expired(now))
                    .unwrap_or(true)
            });
            if is_any_expired {
                expired_ids.push(id.clone());
            }
        }

        for id in expired_ids {
            if let Some(outpoints) = by_id.remove(&id) {
                for op in outpoints {
                    by_outpoint.remove(&op);
                }
            }
        }
    }

    /// Check if a specific outpoint is currently reserved and not expired.
    pub fn is_reserved(&self, outpoint: &OutPoint) -> bool {
        let now = Utc::now();
        let by_outpoint = self.reservations_by_outpoint.read();
        match by_outpoint.get(outpoint) {
            Some(res) => !res.is_expired(now),
            None => false,
        }
    }

    /// Atomically reserve a set of UTXOs with a lease duration (TTL) and optional context.
    /// Returns the reservation_id if successful, or an error if any UTXO is already locked.
    pub fn reserve(
        &self,
        outpoints: &[OutPoint],
        amounts_sat: &[u64],
        ttl: Duration,
        context: Option<String>,
    ) -> Result<String> {
        if outpoints.is_empty() {
            return Err(GrossBitError::ReservationError(
                "Cannot reserve empty outpoint set".to_string(),
            ));
        }

        self.clean_expired();

        let now = Utc::now();
        let expires_at = now + ttl;
        let reservation_id = format!("res_{}", rand::random::<u64>());

        let mut by_outpoint = self.reservations_by_outpoint.write();
        let mut by_id = self.reservations_by_id.write();

        // 1. Verify none are already reserved
        for op in outpoints {
            if let Some(existing) = by_outpoint.get(op) {
                if !existing.is_expired(now) {
                    return Err(GrossBitError::ReservationError(format!(
                        "Outpoint {} already reserved under ID {} until {}",
                        op, existing.reservation_id, existing.expires_at
                    )));
                }
            }
        }

        // 2. Lock all outpoints
        for (idx, &op) in outpoints.iter().enumerate() {
            let amount_sat = amounts_sat.get(idx).copied().unwrap_or(0);
            let reservation = UtxoReservation {
                reservation_id: reservation_id.clone(),
                outpoint: op,
                amount_sat,
                reserved_at: now,
                expires_at,
                context: context.clone(),
            };
            by_outpoint.insert(op, reservation);
        }

        by_id.insert(reservation_id.clone(), outpoints.to_vec());

        Ok(reservation_id)
    }

    /// Release an active reservation by ID, returning the UTXOs back to the available pool.
    pub fn release(&self, reservation_id: &str) -> bool {
        let mut by_outpoint = self.reservations_by_outpoint.write();
        let mut by_id = self.reservations_by_id.write();

        if let Some(outpoints) = by_id.remove(reservation_id) {
            for op in outpoints {
                by_outpoint.remove(&op);
            }
            true
        } else {
            false
        }
    }

    /// Commit an active reservation (e.g. after tx broadcast).
    /// Removes the reservation without releasing UTXOs back to available pool.
    pub fn commit(&self, reservation_id: &str) -> bool {
        self.release(reservation_id)
    }

    /// List all currently active reservations.
    pub fn list_active(&self) -> Vec<UtxoReservation> {
        let now = Utc::now();
        let by_outpoint = self.reservations_by_outpoint.read();
        by_outpoint
            .values()
            .filter(|res| !res.is_expired(now))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Txid;
    use std::str::FromStr;

    #[test]
    fn test_reservation_and_anti_double_spend() {
        let manager = UtxoReservationManager::new();
        let txid = Txid::from_str("0000000000000000000000000000000000000000000000000000000000000001").unwrap();
        let op1 = OutPoint::new(txid, 0);
        let op2 = OutPoint::new(txid, 1);

        // Reserve op1
        let res_id1 = manager
            .reserve(&[op1], &[100_000], Duration::seconds(60), Some("Tx 1".into()))
            .expect("should reserve");

        assert!(manager.is_reserved(&op1));
        assert!(!manager.is_reserved(&op2));

        // Attempting to reserve op1 again should fail (anti-double spend)
        let conflict = manager.reserve(&[op1, op2], &[100_000, 50_000], Duration::seconds(60), Some("Tx 2".into()));
        assert!(conflict.is_err());

        // Releasing res_id1 unlocks op1
        assert!(manager.release(&res_id1));
        assert!(!manager.is_reserved(&op1));

        // Now reserving both should succeed
        let _res_id2 = manager
            .reserve(&[op1, op2], &[100_000, 50_000], Duration::seconds(60), Some("Tx 2".into()))
            .expect("should now reserve both");
        assert!(manager.is_reserved(&op1));
        assert!(manager.is_reserved(&op2));
    }
}
