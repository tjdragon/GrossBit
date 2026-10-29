# Bitcoin Deposit Address Derivation: Architectural Analysis & Decision Guide

This document analyzes address derivation and signing architectures for **GrossBit**, comparing **BIP-341 Taproot Merkle Root Tag Tweaking** against **BIP-32 Hardened** and **Unhardened Key Derivation**.

---

## Table of Contents
1. [Core Scenario & Goals](#core-scenario--goals)
2. [Model 1: Taproot Script-Tree Tag Tweaking (BIP-341 / BIP-371)](#model-1-taproot-script-tree-tag-tweaking-bip-341--bip-371)
3. [Model 2: Hardened BIP-32 Key Derivation](#model-2-hardened-bip-32-key-derivation)
4. [Model 3: Unhardened BIP-32 Key Derivation (xpub)](#model-3-unhardened-bip-32-key-derivation-xpub)
5. [Comprehensive Comparison Matrix](#comprehensive-comparison-matrix)
6. [Tag Durability, Loss, and Recovery Analysis](#tag-durability-loss-and-recovery-analysis)
7. [Deterministic Tag Mitigation Strategies](#deterministic-tag-mitigation-strategies)
8. [GrossBit Integration Blueprint](#grossbit-integration-blueprint)

---

## 1. Core Scenario & Goals

* **Single Master/Account Key**: A user or service has a single secp256k1 keypair ($d, P$), representing an internal Taproot key.
* **Multiple Deposit Addresses**: Dynamically generate unique deposit addresses (`bc1p...` / `tb1p...`) on demand for users, orders, or customers.
* **Spend via Key-Path**: Spend funds cheaply and privately using standard 64-byte Schnorr signatures without publishing script trees to the blockchain.
* **PSBT Workflow**: Sign transactions via PSBTs specifying input metadata (`witnessUtxo`, `tapInternalKey`, and `tapMerkleRoot`).

---

## 2. Model 1: Taproot Script-Tree Tag Tweaking (BIP-341 / BIP-371)

### Mathematical Mechanics

In BIP-341, any Taproot output key $Q$ is computed from an internal x-only public key $P$ and an optional Merkle root $m$:

$$t = \text{tagged\_hash}_{\text{TapTweak}}(P \parallel m)$$
$$Q = P + t \cdot G$$

Instead of committing to an actual AST of Bitcoin scripts, we place a custom **32-byte tag** directly into the Merkle root field $m$:

* **Address Generation**:
  $$m = \text{tag}_{32}$$
  $$Q = P + \text{tagged\_hash}_{\text{TapTweak}}(P \parallel \text{tag}_{32}) \cdot G$$
  The deposit address is $Q$ encoded as Bech32m (`bc1p...`).
* **Key-Path Spending**:
  BIP-341 permits key-path spending even when a Merkle root is committed. The tweaked private key is:
  $$d' = (d + t) \pmod n \quad \text{(with coordinate parity adjustments per BIP-340/341)}$$
  The witness on-chain consists **solely of the 64-byte Schnorr signature**. The 32-byte tag is **never revealed on-chain**.

### BIP-371 PSBT Flow
A coordinator prepares an unsigned PSBT input populated with:
* `PSBT_IN_WITNESS_UTXO`: Amount and scriptPubKey.
* `PSBT_IN_TAP_INTERNAL_KEY`: The 32-byte internal pubkey $P$.
* `PSBT_IN_TAP_MERKLE_ROOT`: The 32-byte tag.

The signer (cold storage, HSM, or software) reads $P$ and $m$, computes tweak $t$, derives $d'$, signs the sighash, and returns `PSBT_IN_TAP_KEY_SIG`.

---

## 3. Model 2: Hardened BIP-32 Key Derivation

### Mathematical Mechanics

Using standard BIP-32 / BIP-86 hardened derivation paths (e.g. `m/86'/0'/0'/0'/i`):

$$k_{\text{child}} = \left( k_{\text{par}} + \text{HMAC-SHA512}(\text{ChainCode}_{\text{par}}, \text{0x00} \parallel k_{\text{par}} \parallel i) \right) \pmod n$$

* **Private Key Obligation**: Hardened derivation **strictly requires the parent private key** to compute child keys.
* **Address Generation**: Public keys can **never** be derived from an `xpub` for hardened indices ($i \ge 2^{31}$).
* **Security Implication**: The server or microservice generating deposit addresses **must hold the master or account private key hot**.

---

## 4. Model 3: Unhardened BIP-32 Key Derivation (xpub)

### Mathematical Mechanics

Using unhardened derivation (e.g. `m/86'/0'/0'/0/i`):
* The server stores the account extended public key (`xpub` / `tpub`), which includes $P_{\text{account}}$ and the 32-byte `ChainCode`.
* Child public keys are derived purely publicly without private keys:
  $$P_i = P_{\text{par}} + \text{HMAC-SHA512}(\text{ChainCode}_{\text{par}}, P_{\text{par}} \parallel i) \cdot G$$

### Inherent Risks:
* **Catastrophic Key Leakage**: If an attacker gets the `xpub` AND any single child private key leaks, the attacker can mathematically derive the **master/account private key**, compromising all past and future addresses.
* **Gap Limit**: Scanning nodes and standard wallets break if there are gaps of more than 20 unused addresses.

---

## 5. Comprehensive Comparison Matrix

| Evaluation Dimension | Model 1: BIP-341 Tag Tweak | Model 2: BIP-32 Hardened | Model 3: BIP-32 Unhardened |
| :--- | :--- | :--- | :--- |
| **Server Security** | **Maximum**: Server only holds a 32-byte public key $P$. | **Poor**: Server must hold hot private key. | **Moderate**: Server holds `xpub`; vulnerable to child-key leak. |
| **Exposure Risk on Server Breach** | Attacker gets public key & tags. Zero funds at risk. | Complete loss of all funds. | Master key recovery if 1 child privkey ever leaks. |
| **On-Chain Privacy** | Maximum. Standard P2TR key-path spend. Tag is hidden. | Maximum. Standard P2TR key-path spend. | Maximum. Standard P2TR key-path spend. |
| **Witness Size & Fees** | 64-byte Schnorr signature. Minimum fees. | 64-byte Schnorr signature. Minimum fees. | 64-byte Schnorr signature. Minimum fees. |
| **Gap Limit Limitations** | **None**: PSBT coordinator passes exact tag. | Standard gap limits apply (default 20). | Standard gap limits apply (default 20). |
| **Metadata Commitment** | Can commit arbitrary 32-byte payload (hash of UUID, invoice). | Sequential integer index $i$ only. | Sequential integer index $i$ only. |
| **Standard Wallet Recovery** | **None**: Off-the-shelf wallets do not know custom tags. | **Full**: Any BIP-39/BIP-86 wallet can recover seed. | **Full**: Any BIP-39/BIP-86 wallet can recover seed. |
| **Recovery if DB is Lost** | **Fatal if random** ($2^{256}$ space). Solvable if deterministic. | **Easy**: Loop index $i = 0..10^6$ against indexer. | **Easy**: Loop index $i = 0..10^6$ against indexer. |
| **GrossBit Native Effort** | Minimal: add `Option<TapNodeHash>` to `TxSigner` + PSBT helper. | Already possible via external BIP-32 derivation. | Already possible via external BIP-32 derivation. |

---

## 6. Tag Durability, Loss, and Recovery Analysis

### The Critical Difference Between BIP-32 Index and Taproot Tag

1. **Brute-Force Feasibility**:
   * In BIP-32, derivation index $i$ is a 32-bit unsigned integer ($0 \le i < 2^{31}$). If your database is wiped out, you can scan $i = 0 \dots 1,000,000$ in minutes using an indexer/node to recover all funds.
   * With a **random 32-byte tag**, the search space is $2^{256}$. If the tag is lost, it is **impossible to scan or brute-force**. Even the owner of the private key cannot spend the funds—they are burned permanently.

2. **Secrecy vs. Integrity / Availability**:
   * **Tag Loss = Fatal**: Prevents calculating $d' = d + t$.
   * **Tag Leak = Non-Fatal**: An attacker knowing the tag cannot spend without internal private key $d$. It only causes a privacy leak (the attacker can link the deposit address to $P$).

3. **Off-The-Shelf Wallet Incompatibility**:
   * No standard wallet (Sparrow, Electrum, Trezor, Ledger Live) scans for arbitrary Merkle roots.
   * Recovery always requires your custom signing logic or a PSBT constructor that sets `PSBT_IN_TAP_MERKLE_ROOT`.

---

## 7. Deterministic Tag Mitigation Strategies

To eliminate the risk of unrecoverable fund loss while retaining the security of public address generation, **never use purely random tags**. Instead, use deterministic derivations:

### Strategy A: Sequential Index Hashing
Treat the tag as a commitment to an integer index:
```rust
// tag = SHA256("grossbit_deposit" || index_u64)
let tag = bitcoin::hashes::sha256::Hash::hash(
    &[b"grossbit_deposit", &customer_index.to_be_bytes()[..]].concat()
);
```
* **Disaster Recovery**: If the database is completely destroyed, scan `customer_index = 0..1_000_000` just like BIP-32!
* **Security**: The address-generation service still only needs public key $P$.

### Strategy B: Seeded HMAC with Business Identifier
Bind the tag to an existing business entity (customer UUID, invoice ID, or order number):
```rust
// tag = HMAC_SHA256(server_secret_seed, customer_uuid)
```
* **Disaster Recovery**: Stateless. Tags do not need individual database backups; backing up `server_secret_seed` and your customer table lets you regenerate any tag on the fly when preparing a sweeping PSBT.

---

## 8. GrossBit Integration Blueprint

GrossBit re-exports `bitcoin = "0.32"`, giving direct access to `bitcoin::psbt::Psbt`, `bitcoin::key::TapTweak`, and `TapNodeHash`.

### 1. Generating Deposit Addresses
```rust
use bitcoin::hashes::Hash;
use bitcoin::key::{Secp256k1, TapTweak};
use bitcoin::{Address, Network, TapNodeHash, XOnlyPublicKey};

pub fn generate_tagged_taproot_address(
    internal_key: XOnlyPublicKey,
    tag: [u8; 32],
    network: Network,
) -> Address {
    let secp = Secp256k1::verification_only();
    let merkle_root = TapNodeHash::from_byte_array(tag);
    
    // Q = P + TapTweak(P || merkle_root) * G
    let (output_key, _parity) = internal_key.tap_tweak(&secp, Some(merkle_root));
    Address::p2tr_tweaked(output_key, network)
}
```

### 2. Updating `TxSigner` to Support Tagged Merkle Roots
Currently, [`src/tx/signer.rs`](file:///home/tj/Code/GrossBit/src/tx/signer.rs#L70) has:
```rust
let tweaked_keypair = keypair.tap_tweak(&secp, None);
```
Update `TxSigner::sign_p2tr_keypath` to take an optional merkle root:
```rust
pub fn sign_p2tr_keypath(
    tx: &mut Transaction,
    input_index: usize,
    all_utxos: &[GrossUtxo],
    keypair: &Keypair,
    merkle_root: Option<TapNodeHash>, // <--- Pass custom tag/root here
) -> Result<()> {
    let secp = Secp256k1::new();
    let prevouts: Vec<TxOut> = all_utxos
        .iter()
        .map(|u| TxOut { value: u.amount, script_pubkey: u.script_pubkey.clone() })
        .collect();

    let tweaked_keypair = keypair.tap_tweak(&secp, merkle_root);

    let mut sighash_cache = SighashCache::new(&*tx);
    let sighash = sighash_cache
        .taproot_key_spend_signature_hash(
            input_index,
            &Prevouts::All(&prevouts),
            TapSighashType::Default,
        )
        .map_err(|e| GrossBitError::SigningError(format!("Taproot sighash error: {}", e)))?;

    let msg = Message::from_digest(sighash.to_byte_array());
    let sig: SchnorrSignature = secp.sign_schnorr_no_aux_rand(&msg, &tweaked_keypair.to_keypair());

    let mut witness = Witness::new();
    witness.push(sig.as_ref());
    tx.input[input_index].witness = witness;

    Ok(())
}
```

### 3. Assembling a BIP-371 PSBT
```rust
use bitcoin::psbt::{Input, Psbt};
use bitcoin::{TapNodeHash, Transaction, TxOut, XOnlyPublicKey};

pub fn build_taproot_psbt(
    unsigned_tx: Transaction,
    spent_utxos: Vec<TxOut>,
    internal_key: XOnlyPublicKey,
    tags: Vec<Option<[u8; 32]>>,
) -> Psbt {
    let mut psbt = Psbt::from_unsigned_tx(unsigned_tx).expect("valid tx");
    psbt.inputs = spent_utxos
        .into_iter()
        .zip(tags)
        .map(|(utxo, tag)| {
            let mut input = Input::default();
            input.witness_utxo = Some(utxo);
            input.tap_internal_key = Some(internal_key);
            if let Some(t) = tag {
                input.tap_merkle_root = Some(TapNodeHash::from_byte_array(t));
            }
            input
        })
        .collect();
    psbt
}
```

---

## Summary & Recommendation

1. If your highest priority is **preventing hot private keys on the deposit server** and **avoiding BIP-32 chaincode compromise risks**, the **BIP-341 Taproot Merkle Root Tag Tweaking** model is technically superior.
2. If you choose Model 1, **always use deterministic tag derivation** (Strategy A or B) so that a database failure never results in unspendable UTXOs.
3. If your priority is **standard seed recovery using commercial hardware wallets or software like Sparrow/Electrum without custom code**, stick to standard BIP-32/BIP-86 derivation.

