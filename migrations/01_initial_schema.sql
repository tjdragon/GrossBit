-- Schema for GrossBit Bitcoin UTXO & Transaction Store

CREATE TABLE IF NOT EXISTS utxos (
    outpoint_txid VARCHAR(64) NOT NULL,
    outpoint_vout INTEGER NOT NULL,
    address VARCHAR(120) NOT NULL,
    amount_sat BIGINT NOT NULL,
    script_pubkey_hex TEXT NOT NULL,
    script_kind VARCHAR(30) NOT NULL,
    confirmations INTEGER NOT NULL DEFAULT 0,
    is_coinbase BOOLEAN NOT NULL DEFAULT FALSE,
    status VARCHAR(20) NOT NULL DEFAULT 'AVAILABLE', -- 'AVAILABLE', 'RESERVED', 'SPENT'
    reservation_id VARCHAR(64),
    reserved_at TIMESTAMPTZ,
    reservation_expires_at TIMESTAMPTZ,
    reservation_context TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (outpoint_txid, outpoint_vout)
);

CREATE INDEX IF NOT EXISTS idx_utxos_address_status ON utxos(address, status);
CREATE INDEX IF NOT EXISTS idx_utxos_reservation ON utxos(reservation_id, reservation_expires_at);

CREATE TABLE IF NOT EXISTS tx_history (
    id BIGSERIAL PRIMARY KEY,
    txid VARCHAR(64) NOT NULL,
    address VARCHAR(120) NOT NULL,
    amount_sat BIGINT NOT NULL,
    direction VARCHAR(10) NOT NULL, -- 'RECEIVED', 'SENT'
    block_height BIGINT,
    block_timestamp BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_tx_history_address ON tx_history(address, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_tx_history_txid ON tx_history(txid);
