use crate::error::{GrossBitError, Result};
use crate::indexer::memory::{TxDirection, TxHistoryEntry};
use crate::model::{Balance, GrossUtxo, ScriptKind};
use bitcoin::{Amount, OutPoint, ScriptBuf, Txid};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct PostgresUtxoStore {
    pool: PgPool,
}

impl PostgresUtxoStore {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(|e| GrossBitError::UtxoError(format!("Failed to connect to Postgres: {}", e)))?;

        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<()> {
        let schema_sql = include_str!("../../migrations/01_initial_schema.sql");
        sqlx::raw_sql(schema_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| GrossBitError::UtxoError(format!("Schema migration error: {}", e)))?;
        Ok(())
    }

    /// Insert or update a UTXO in the database.
    pub async fn upsert_utxo(&self, utxo: &GrossUtxo) -> Result<()> {
        let addr = utxo.address.clone().unwrap_or_default();
        let txid_str = utxo.outpoint.txid.to_string();
        let vout = utxo.outpoint.vout as i32;
        let amount = utxo.amount.to_sat() as i64;
        let script_hex = hex_encode(utxo.script_pubkey.as_bytes());
        let script_kind = utxo.script_kind.as_str();
        let confirmations = utxo.confirmations as i32;

        sqlx::query(
            r#"
            INSERT INTO utxos (
                outpoint_txid, outpoint_vout, address, amount_sat,
                script_pubkey_hex, script_kind, confirmations, is_coinbase, status
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'AVAILABLE')
            ON CONFLICT (outpoint_txid, outpoint_vout) DO UPDATE SET
                confirmations = EXCLUDED.confirmations,
                updated_at = NOW()
            WHERE utxos.status != 'SPENT';
            "#,
        )
        .bind(txid_str)
        .bind(vout)
        .bind(addr)
        .bind(amount)
        .bind(script_hex)
        .bind(script_kind)
        .bind(confirmations)
        .bind(utxo.is_coinbase)
        .execute(&self.pool)
        .await
        .map_err(|e| GrossBitError::UtxoError(format!("Failed to upsert UTXO: {}", e)))?;

        Ok(())
    }

    /// Get all available (unreserved or expired reservation) UTXOs for an address.
    pub async fn get_available_utxos(&self, address: &str) -> Result<Vec<GrossUtxo>> {
        let rows = sqlx::query(
            r#"
            SELECT outpoint_txid, outpoint_vout, address, amount_sat,
                   script_pubkey_hex, script_kind, confirmations, is_coinbase
            FROM utxos
            WHERE address = $1
              AND (status = 'AVAILABLE' OR (status = 'RESERVED' AND reservation_expires_at < NOW()))
            ORDER BY confirmations DESC, amount_sat DESC;
            "#,
        )
        .bind(address)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| GrossBitError::UtxoError(format!("Failed to fetch available UTXOs: {}", e)))?;

        let mut utxos = Vec::new();
        for row in rows {
            let txid_str: String = row.get("outpoint_txid");
            let vout: i32 = row.get("outpoint_vout");
            let amount_sat: i64 = row.get("amount_sat");
            let script_hex: String = row.get("script_pubkey_hex");
            let script_kind_str: String = row.get("script_kind");
            let confirmations: i32 = row.get("confirmations");
            let is_coinbase: bool = row.get("is_coinbase");
            let addr: String = row.get("address");

            let txid = Txid::from_str(&txid_str)
                .map_err(|e| GrossBitError::UtxoError(format!("Invalid txid in DB: {}", e)))?;
            let script_bytes = hex_decode(&script_hex)
                .map_err(|e| GrossBitError::UtxoError(format!("Invalid script hex in DB: {}", e)))?;

            utxos.push(GrossUtxo {
                outpoint: OutPoint::new(txid, vout as u32),
                amount: Amount::from_sat(amount_sat as u64),
                script_pubkey: ScriptBuf::from_bytes(script_bytes),
                address: Some(addr),
                script_kind: ScriptKind::from_str_name(&script_kind_str),
                confirmations: confirmations.max(0) as u32,
                is_coinbase,
            });
        }

        Ok(utxos)
    }

    /// Atomically reserve UTXOs in PostgreSQL with a TTL to prevent double spending.
    pub async fn reserve_utxos(
        &self,
        outpoints: &[OutPoint],
        ttl_seconds: i64,
        context: Option<&str>,
    ) -> Result<String> {
        if outpoints.is_empty() {
            return Err(GrossBitError::ReservationError("Empty outpoint list".into()));
        }

        let reservation_id = format!("res_{}", rand::random::<u64>());
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| GrossBitError::ReservationError(format!("DB tx begin error: {}", e)))?;

        for op in outpoints {
            let txid_str = op.txid.to_string();
            let vout = op.vout as i32;

            let result = sqlx::query(
                r#"
                UPDATE utxos
                SET status = 'RESERVED',
                    reservation_id = $1,
                    reserved_at = NOW(),
                    reservation_expires_at = NOW() + make_interval(secs => $2::int),
                    reservation_context = $3,
                    updated_at = NOW()
                WHERE outpoint_txid = $4
                  AND outpoint_vout = $5
                  AND (status = 'AVAILABLE' OR (status = 'RESERVED' AND reservation_expires_at < NOW()));
                "#,
            )
            .bind(&reservation_id)
            .bind(ttl_seconds as i32)
            .bind(context)
            .bind(&txid_str)
            .bind(vout)
            .execute(&mut *tx)
            .await
            .map_err(|e| GrossBitError::ReservationError(format!("Failed to lock UTXO {}: {}", op, e)))?;

            if result.rows_affected() == 0 {
                // Outpoint is already locked or does not exist
                let _ = tx.rollback().await;
                return Err(GrossBitError::ReservationError(format!(
                    "Outpoint {} is already reserved or does not exist (anti-double spend check failed)",
                    op
                )));
            }
        }

        tx.commit()
            .await
            .map_err(|e| GrossBitError::ReservationError(format!("DB tx commit error: {}", e)))?;

        Ok(reservation_id)
    }

    /// Release a reservation back to AVAILABLE status (e.g. if transaction failed).
    pub async fn release_reservation(&self, reservation_id: &str) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE utxos
            SET status = 'AVAILABLE',
                reservation_id = NULL,
                reserved_at = NULL,
                reservation_expires_at = NULL,
                reservation_context = NULL,
                updated_at = NOW()
            WHERE reservation_id = $1 AND status = 'RESERVED';
            "#,
        )
        .bind(reservation_id)
        .execute(&self.pool)
        .await
        .map_err(|e| GrossBitError::ReservationError(format!("Failed to release reservation: {}", e)))?;

        Ok(result.rows_affected())
    }

    /// Commit a reservation marking the UTXOs permanently SPENT (after broadcast).
    pub async fn commit_reservation(&self, reservation_id: &str) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE utxos
            SET status = 'SPENT',
                updated_at = NOW()
            WHERE reservation_id = $1 AND status = 'RESERVED';
            "#,
        )
        .bind(reservation_id)
        .execute(&self.pool)
        .await
        .map_err(|e| GrossBitError::ReservationError(format!("Failed to commit reservation: {}", e)))?;

        Ok(result.rows_affected())
    }

    /// Fetch balance breakdown for an address from PostgreSQL.
    pub async fn get_balance(&self, address: &str) -> Result<Balance> {
        let row = sqlx::query(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN confirmations > 0 AND (status = 'AVAILABLE' OR (status = 'RESERVED' AND reservation_expires_at < NOW())) THEN amount_sat ELSE 0 END), 0) AS confirmed_sat,
                COALESCE(SUM(CASE WHEN confirmations = 0 AND (status = 'AVAILABLE' OR (status = 'RESERVED' AND reservation_expires_at < NOW())) THEN amount_sat ELSE 0 END), 0) AS unconfirmed_sat,
                COALESCE(SUM(CASE WHEN status = 'RESERVED' AND reservation_expires_at >= NOW() THEN amount_sat ELSE 0 END), 0) AS reserved_sat
            FROM utxos
            WHERE address = $1 AND status != 'SPENT';
            "#,
        )
        .bind(address)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| GrossBitError::UtxoError(format!("Failed to fetch balance: {}", e)))?;

        let confirmed_sat: i64 = row.get("confirmed_sat");
        let unconfirmed_sat: i64 = row.get("unconfirmed_sat");
        let reserved_sat: i64 = row.get("reserved_sat");

        Ok(Balance {
            confirmed_sat: confirmed_sat as u64,
            unconfirmed_sat: unconfirmed_sat as u64,
            reserved_sat: reserved_sat as u64,
        })
    }

    /// Log a transaction history entry in PostgreSQL.
    pub async fn record_tx_history(&self, entry: &TxHistoryEntry) -> Result<()> {
        let direction_str = match entry.direction {
            TxDirection::Received => "RECEIVED",
            TxDirection::Sent => "SENT",
        };

        sqlx::query(
            r#"
            INSERT INTO tx_history (txid, address, amount_sat, direction, block_height, block_timestamp)
            VALUES ($1, $2, $3, $4, $5, $6);
            "#,
        )
        .bind(entry.txid.to_string())
        .bind(&entry.address)
        .bind(entry.amount_sat as i64)
        .bind(direction_str)
        .bind(entry.block_height.map(|h| h as i64))
        .bind(entry.timestamp as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| GrossBitError::IndexerError(format!("Failed to record tx history: {}", e)))?;

        Ok(())
    }

    /// Retrieve transaction history for an address from PostgreSQL.
    pub async fn get_tx_history(&self, address: &str) -> Result<Vec<TxHistoryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT txid, address, amount_sat, direction, block_height, block_timestamp
            FROM tx_history
            WHERE address = $1
            ORDER BY created_at DESC;
            "#,
        )
        .bind(address)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| GrossBitError::IndexerError(format!("Failed to fetch tx history: {}", e)))?;

        let mut history = Vec::new();
        for r in rows {
            let txid_str: String = r.get("txid");
            let addr: String = r.get("address");
            let amount_sat: i64 = r.get("amount_sat");
            let dir_str: String = r.get("direction");
            let block_height: Option<i64> = r.get("block_height");
            let timestamp: i64 = r.get("block_timestamp");

            let direction = match dir_str.as_str() {
                "RECEIVED" => TxDirection::Received,
                _ => TxDirection::Sent,
            };

            history.push(TxHistoryEntry {
                txid: Txid::from_str(&txid_str)
                    .map_err(|e| GrossBitError::IndexerError(format!("Invalid txid in DB: {}", e)))?,
                block_height: block_height.map(|h| h as u64),
                timestamp: timestamp as u64,
                address: addr,
                amount_sat: amount_sat as u64,
                direction,
            });
        }

        Ok(history)
    }
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for byte in data {
        use std::fmt::Write;
        write!(&mut s, "{:02x}", byte).unwrap();
    }
    s
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("Odd length hex string".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
