# GrossBit

GrossBit is an industrial-grade Bitcoin library implemented in **Rust**, specifically tailored for **Bitcoin Testnet4** (and extensible to Mainnet, Signet, and Regtest).

It provides complete transaction lifecycle management:
- **UTXO Selection & Anti-Double-Spend Reservation**: Atomically leases UTXOs with time-to-live (TTL) expiration, preventing concurrent double-spends across workers.
- **Transaction Building**: Native SegWit (**P2WPKH**) and Taproot (**P2TR** as the modern default), dynamic sat/vB fee calculation, dust protection, and RBF (Replace-By-Fee).
- **Universal Receiving & Script Parsing**: Decodes and classifies all standard Bitcoin scripts: **P2TR**, **P2WPKH**, **P2WSH**, **P2SH**, **P2PKH**, **OP_RETURN**, and **Multisig**.
- **Signature Assembly**: Taproot keypath spending (**BIP 340/341 Schnorr**) and SegWit v0 (**BIP 141/143 ECDSA**).
- **External Providers**: Built-in async **Esplora client** targeting public Testnet4 (e.g. `https://mempool.space/testnet4/api`), with support for node RPC.
- **Dockerized PostgreSQL Storage**: Robust persistence for UTXO pools, transactional lease-locking, balances, and transaction history.

---

## Architecture Overview

```
GrossBit
├── provider/       # Async Esplora (Mempool.space) Testnet4 client
├── storage/        # PostgreSQL UTXO store with row-level atomic locking & TTL
├── utxo/           # Coin selection algorithms (Branch & Bound, Knapsack, FIFO) & in-memory pool
├── tx/             # Transaction builder, signer (Schnorr/ECDSA), and universal script parser
├── indexer/        # Address watcher, balance calculator, and tx history logger
└── rpc/            # Bitcoin Core JSON-RPC client fallback
```

---

## Quickstart

### 1. Launch PostgreSQL with Docker

```bash
docker compose up -d
```

This starts PostgreSQL on port `5432` and runs the schema migration in [`migrations/01_initial_schema.sql`](file:///home/tj/Code/GrossBit/migrations/01_initial_schema.sql).

### 2. Add GrossBit to your `Cargo.toml`

```toml
[dependencies]
grossbit = { path = "." }
tokio = { version = "1", features = ["full"] }
```

### 3. Usage Example: Building, Reserving, and Broadcasting on Testnet4

```rust
use grossbit::{
    Address, Amount, CoinSelectionStrategy, FeeRate, GrossBit,
    PostgresUtxoStore, Duration, SecretKey
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Connect to PostgreSQL UTXO storage
    let pg_store = PostgresUtxoStore::connect(
        "postgres://grossbit:grossbit_secret@localhost:5432/grossbit_db"
    ).await?;

    // 2. Initialize GrossBit client configured for Testnet4 with Esplora
    let gb = GrossBit::new_testnet4().with_postgres(pg_store);

    // 3. Sync address UTXOs from Testnet4 mempool into PostgreSQL
    let my_address: Address = "tb1p...your_taproot_address...".parse()?;
    gb.sync_address(&my_address).await?;

    // Check confirmed, unconfirmed, and spendable balances
    let balance = gb.get_balance(&my_address).await?;
    println!("Spendable balance: {} sats", balance.spendable_sat());

    // 4. Build payment with automatic coin selection and anti-double-spend reservation
    let recipient: Address = "tb1q...recipient_address...".parse()?;
    let fee_rate = FeeRate::from_sat_per_vb(2.5); // or gb.esplora.unwrap().estimate_fee_rate(3).await?

    let (built_tx, reservation_id) = gb.create_payment(
        recipient,
        50_000,                  // 50,000 satoshis
        my_address.clone(),       // Change output (defaults to P2TR)
        fee_rate,
        CoinSelectionStrategy::Optimal,
        Duration::minutes(5),     // Lease locked for 5 minutes
    ).await?;

    println!("Transaction built! Vsize: {} vB, Fee: {} sats", built_tx.estimated_vsize, built_tx.fee_sat);
    println!("Unsigned Hex: {}", built_tx.to_hex());

    // 5. Sign with private key and broadcast to Testnet4 via Esplora
    let my_secret_key = SecretKey::from_slice(&[...32_bytes...])?;
    let txid = gb.sign_and_broadcast_esplora(built_tx, &reservation_id, &my_secret_key).await?;

    println!("Broadcast successful! TxID: {}", txid);
    Ok(())
}
```

---

## Universal Script & Transaction Parsing

GrossBit can inspect any raw transaction hex from Testnet4:

```rust
use grossbit::{GrossBit, ScriptKind};

let gb = GrossBit::new_testnet4();
let parsed = gb.parse_raw_hex("02000000000101...")?;

println!("Txid: {}", parsed.txid);
for (i, out) in parsed.outputs.iter().enumerate() {
    println!("Output #{}: {} sats, Script type: {:?}", i, out.amount_sat, out.script_kind);
    // Matches P2tr, P2wpkh, P2wsh, P2sh, P2pkh, OpReturn, Multisig, etc.
}
```

---

## Running Tests

Run the test suite verifying coin selection, anti-double-spend leases, Taproot Schnorr and SegWit ECDSA signing:

```bash
cargo test
```