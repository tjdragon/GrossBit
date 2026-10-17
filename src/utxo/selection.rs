use crate::error::{GrossBitError, Result};
use crate::model::{FeeRate, GrossUtxo, ScriptKind};

/// Supported coin selection algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoinSelectionStrategy {
    /// Try exact match with Branch and Bound; if none found, fallback to Knapsack / Lowest Larger.
    Optimal,
    /// Oldest First (FIFO) based on confirmations.
    Fifo,
    /// Largest first (consolidates large values first, minimizing input count).
    LargestFirst,
}

/// Result of a coin selection process.
#[derive(Debug, Clone)]
pub struct SelectedCoins {
    pub selected_utxos: Vec<GrossUtxo>,
    pub total_input_sat: u64,
    pub target_sat: u64,
    pub fee_sat: u64,
    pub change_sat: u64,
    pub estimated_vsize: usize,
}

pub struct CoinSelector;

impl CoinSelector {
    /// Estimate transaction virtual size (vbytes) based on input and output counts and script kinds.
    pub fn estimate_vsize(
        inputs: &[ScriptKind],
        output_script_kinds: &[ScriptKind],
        has_change: bool,
        change_kind: ScriptKind,
    ) -> usize {
        // Base transaction overhead: 10.5 rounded to 11 vbytes (version + segwit marker/flag + locktime + varints)
        let base_overhead = 11;

        let input_vsize: usize = inputs.iter().map(|k| k.estimated_input_vsize()).sum();
        let mut output_vsize: usize = output_script_kinds
            .iter()
            .map(|k| k.estimated_output_vsize())
            .sum();

        if has_change {
            output_vsize += change_kind.estimated_output_vsize();
        }

        base_overhead + input_vsize + output_vsize
    }

    /// Select UTXOs to cover `target_sat` plus dynamic fees.
    pub fn select(
        mut available: Vec<GrossUtxo>,
        target_sat: u64,
        fee_rate: FeeRate,
        recipient_kind: ScriptKind,
        change_kind: ScriptKind,
        strategy: CoinSelectionStrategy,
    ) -> Result<SelectedCoins> {
        if available.is_empty() {
            return Err(GrossBitError::InsufficientFunds {
                needed_sat: target_sat,
                available_sat: 0,
            });
        }

        let total_available: u64 = available.iter().map(|u| u.satoshis()).sum();
        if total_available < target_sat {
            return Err(GrossBitError::InsufficientFunds {
                needed_sat: target_sat,
                available_sat: total_available,
            });
        }

        match strategy {
            CoinSelectionStrategy::Optimal => {
                // Try exact match (Branch and Bound without change) first
                if let Some(selected) = Self::try_branch_and_bound(
                    &available,
                    target_sat,
                    fee_rate,
                    recipient_kind,
                ) {
                    return Ok(selected);
                }
                // Fallback to lowest larger / knapsack
                Self::select_knapsack(
                    &mut available,
                    target_sat,
                    fee_rate,
                    recipient_kind,
                    change_kind,
                )
            }
            CoinSelectionStrategy::Fifo => {
                // Sort by confirmations descending (oldest first)
                available.sort_by(|a, b| b.confirmations.cmp(&a.confirmations));
                Self::select_linear(
                    &available,
                    target_sat,
                    fee_rate,
                    recipient_kind,
                    change_kind,
                )
            }
            CoinSelectionStrategy::LargestFirst => {
                // Sort by amount descending
                available.sort_by(|a, b| b.satoshis().cmp(&a.satoshis()));
                Self::select_linear(
                    &available,
                    target_sat,
                    fee_rate,
                    recipient_kind,
                    change_kind,
                )
            }
        }
    }

    /// Branch and Bound exact match (no change output needed, saving output vbytes & fees).
    fn try_branch_and_bound(
        utxos: &[GrossUtxo],
        target_sat: u64,
        fee_rate: FeeRate,
        recipient_kind: ScriptKind,
    ) -> Option<SelectedCoins> {
        // Look for single UTXO exact match
        for u in utxos {
            let vsize = Self::estimate_vsize(&[u.script_kind], &[recipient_kind], false, recipient_kind);
            let fee = fee_rate.calculate_fee(vsize);
            if u.satoshis() == target_sat + fee {
                return Some(SelectedCoins {
                    selected_utxos: vec![u.clone()],
                    total_input_sat: u.satoshis(),
                    target_sat,
                    fee_sat: fee,
                    change_sat: 0,
                    estimated_vsize: vsize,
                });
            }
        }
        None
    }

    /// Knapsack / Lowest Larger selection
    fn select_knapsack(
        utxos: &mut [GrossUtxo],
        target_sat: u64,
        fee_rate: FeeRate,
        recipient_kind: ScriptKind,
        change_kind: ScriptKind,
    ) -> Result<SelectedCoins> {
        // Sort ascending by satoshis
        utxos.sort_by_key(|u| u.satoshis());

        // Check if there is a single UTXO that is >= target + fee (Lowest Larger)
        for u in utxos.iter() {
            let vsize_with_change = Self::estimate_vsize(
                &[u.script_kind],
                &[recipient_kind],
                true,
                change_kind,
            );
            let fee = fee_rate.calculate_fee(vsize_with_change);
            let needed = target_sat + fee;

            if u.satoshis() >= needed {
                let change = u.satoshis() - needed;
                // If change is below dust limit (e.g. 546 sats for p2wpkh/p2tr), donate to fee
                let (actual_change, actual_fee) = if change < 546 {
                    (0, fee + change)
                } else {
                    (change, fee)
                };

                return Ok(SelectedCoins {
                    selected_utxos: vec![u.clone()],
                    total_input_sat: u.satoshis(),
                    target_sat,
                    fee_sat: actual_fee,
                    change_sat: actual_change,
                    estimated_vsize: vsize_with_change,
                });
            }
        }

        // Otherwise accumulate from largest to smallest
        utxos.sort_by(|a, b| b.satoshis().cmp(&a.satoshis()));
        Self::select_linear(
            utxos,
            target_sat,
            fee_rate,
            recipient_kind,
            change_kind,
        )
    }

    /// Linear accumulation
    fn select_linear(
        utxos: &[GrossUtxo],
        target_sat: u64,
        fee_rate: FeeRate,
        recipient_kind: ScriptKind,
        change_kind: ScriptKind,
    ) -> Result<SelectedCoins> {
        let mut selected = Vec::new();
        let mut accumulated_sat = 0u64;

        for u in utxos {
            selected.push(u.clone());
            accumulated_sat += u.satoshis();

            let input_kinds: Vec<ScriptKind> = selected.iter().map(|x| x.script_kind).collect();
            let vsize = Self::estimate_vsize(&input_kinds, &[recipient_kind], true, change_kind);
            let fee = fee_rate.calculate_fee(vsize);
            let needed = target_sat + fee;

            if accumulated_sat >= needed {
                let change = accumulated_sat - needed;
                let (actual_change, actual_fee) = if change < 546 {
                    (0, fee + change)
                } else {
                    (change, fee)
                };

                return Ok(SelectedCoins {
                    selected_utxos: selected,
                    total_input_sat: accumulated_sat,
                    target_sat,
                    fee_sat: actual_fee,
                    change_sat: actual_change,
                    estimated_vsize: vsize,
                });
            }
        }

        let total_avail = accumulated_sat;
        let last_vsize = Self::estimate_vsize(
            &selected.iter().map(|x| x.script_kind).collect::<Vec<_>>(),
            &[recipient_kind],
            true,
            change_kind,
        );
        let needed_fee = fee_rate.calculate_fee(last_vsize);

        Err(GrossBitError::InsufficientFunds {
            needed_sat: target_sat + needed_fee,
            available_sat: total_avail,
        })
    }
}
