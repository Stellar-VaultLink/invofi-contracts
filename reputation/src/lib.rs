#![no_std]

//! Reputation contract — cross-contract reputation scoring with
//! reputation-weighted governance support.
//!
//! Tracks repayment outcomes, dispute outcomes, and invoice volume per
//! originator. The repayment contract (the configured recorder) calls
//! `record_outcome` after every fully-repaid invoice (outcome 0 = success)
//! and every default (outcome 1 = default). The registry contract
//! (the configured dispute/volume recorder) calls `record_dispute_outcome`
//! and `record_invoice_volume`. Anyone can read the resulting scores —
//! no auth required to query.
//!
//! Scoring formula (ADR-0004, extended):
//! - Simple score: `repayments - 2 * defaults`, floored at 0
//! - Unified score: `repayment_score * 0.6 + dispute_score * 0.2 + volume_score * 0.2`
//! - Effective score: unified_score with time-decay applied
//!
//! Tiers: Bronze (0–99), Silver (100–499), Gold (500–999), Platinum (1000+)
//! Governance weight: `effective_score / 100` (vote weight for proposals)

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Map};

use invofi_common::{
    assert_not_paused, ContractError, ReputationTier, GOLD_THRESHOLD, PLATINUM_THRESHOLD,
    SILVER_THRESHOLD,
};

/// Outcome discriminant for a successful full repayment.
pub const OUTCOME_REPAID: u32 = 0;
/// Outcome discriminant for a default.
pub const OUTCOME_DEFAULTED: u32 = 1;

/// Per-originator outcome counts (repayment outcomes).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationRecord {
    pub repayments: u32,
    pub defaults: u32,
}

/// Per-originator dispute outcome counts.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeRecord {
    pub disputes_won: u32,
    pub disputes_lost: u32,
}

/// Per-originator invoice volume and activity tracking.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeRecord {
    pub invoice_count: u32,
    pub total_volume: i128,
    /// Unix timestamp of the most recent recorded action.
    pub last_action_timestamp: u64,
}

// ─── Storage Helpers ─────────────────────────────────────────────────────────

fn load_records(env: &Env) -> Map<Address, ReputationRecord> {
    env.storage()
        .persistent()
        .get(&symbol_short!("reputn"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_records(env: &Env, map: &Map<Address, ReputationRecord>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("reputn"), map);
}

fn load_dispute_records(env: &Env) -> Map<Address, DisputeRecord> {
    env.storage()
        .persistent()
        .get(&symbol_short!("disp_r"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_dispute_records(env: &Env, map: &Map<Address, DisputeRecord>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("disp_r"), map);
}

fn load_volume_records(env: &Env) -> Map<Address, VolumeRecord> {
    env.storage()
        .persistent()
        .get(&symbol_short!("vol_rec"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_volume_records(env: &Env, map: &Map<Address, VolumeRecord>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("vol_rec"), map);
}

/// Compute time-decay factor: `0.95 ^ days`, returned as a numerator over 100.
/// For 0 days returns 100 (no decay). Uses integer arithmetic to avoid
/// floating-point — each day multiplies by 95 and divides by 100.
fn time_decay_numerator(days: u64) -> i128 {
    if days == 0 {
        return 100;
    }
    let mut factor: i128 = 100;
    let mut remaining = days;
    while remaining > 0 {
        factor = factor * 95 / 100;
        remaining -= 1;
    }
    factor
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct ReputationContract;

#[contractimpl]
impl ReputationContract {
    // ── Initialization / admin ──────────────────────────────────────────────

    /// One-time setup. Sets the admin address.
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run (issue #75).
    pub fn __constructor(env: Env, admin: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("Already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Configure the address allowed to record outcomes — this is the
    /// repayment contract. Admin only. Recording is disabled until a
    /// recorder is configured (fail-closed).
    pub fn set_recorder(env: Env, admin: Address, recorder: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("recorder"), &recorder);
    }

    pub fn get_recorder(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("recorder"))
    }

    /// Configure the address allowed to record dispute outcomes — this is the
    /// registry contract. Admin only.
    pub fn set_dispute_recorder(env: Env, admin: Address, recorder: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("disp_rec"), &recorder);
    }

    pub fn get_dispute_recorder(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("disp_rec"))
    }

    /// Configure the address allowed to record invoice volume — this is the
    /// registry contract. Admin only.
    pub fn set_volume_recorder(env: Env, admin: Address, recorder: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("vol_rec_a"), &recorder);
    }

    pub fn get_volume_recorder(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("vol_rec_a"))
    }

    // ── Pause / unpause (circuit breaker) ───────────────────────────────────

    pub fn pause(env: Env, admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &true);
    }

    pub fn unpause(env: Env, admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &false);
    }

    pub fn contract_is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("paused"))
            .unwrap_or(false)
    }

    // ── Outcome recording (repayment outcomes) ──────────────────────────────

    /// Record an outcome for an originator. Only the configured recorder (the
    /// repayment contract) may call this, authorized via implicit
    /// contract-invoker auth.
    ///
    /// `outcome`: 0 = successful full repayment, 1 = default. Anything else
    /// panics. Counts are mononic — calling the same outcome twice for the
    /// same invoice would double count, so repayment must call this exactly
    /// once per terminal outcome (once on full repay, once on reclaim).
    pub fn record_outcome(env: Env, originator: Address, outcome: u32) {
        assert_not_paused(&env);
        let recorder: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("recorder"))
            .unwrap_or_else(|| panic!("No recorder configured"));
        recorder.require_auth();
        if outcome != OUTCOME_REPAID && outcome != OUTCOME_DEFAULTED {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let mut records = load_records(&env);
        let mut record = records.get(originator.clone()).unwrap_or(ReputationRecord {
            repayments: 0,
            defaults: 0,
        });
        if outcome == OUTCOME_REPAID {
            record.repayments += 1;
        } else {
            record.defaults += 1;
        }
        records.set(originator.clone(), record);
        save_records(&env, &records);

        env.events()
            .publish((symbol_short!("reputn"), originator.clone()), outcome);
    }

    // ── Dispute outcome recording ───────────────────────────────────────────

    /// Record a dispute outcome for an originator. Only the configured dispute
    /// recorder (the registry contract) may call this.
    ///
    /// `won`: true if the dispute was resolved in the originator's favor,
    /// false otherwise. Updates dispute counts and the last action timestamp.
    pub fn record_dispute_outcome(env: Env, originator: Address, won: bool) {
        assert_not_paused(&env);
        let recorder: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("disp_rec"))
            .unwrap_or_else(|| panic!("No dispute recorder configured"));
        recorder.require_auth();

        let mut disputes = load_dispute_records(&env);
        let mut record = disputes
            .get(originator.clone())
            .unwrap_or(DisputeRecord {
                disputes_won: 0,
                disputes_lost: 0,
            });
        if won {
            record.disputes_won += 1;
        } else {
            record.disputes_lost += 1;
        }
        disputes.set(originator.clone(), record);
        save_dispute_records(&env, &disputes);

        // Update last action timestamp for time-decay calculation.
        let mut volumes = load_volume_records(&env);
        let mut vol = volumes.get(originator.clone()).unwrap_or(VolumeRecord {
            invoice_count: 0,
            total_volume: 0,
            last_action_timestamp: 0,
        });
        vol.last_action_timestamp = env.ledger().timestamp();
        volumes.set(originator.clone(), vol);
        save_volume_records(&env, &volumes);

        let event_data = if won { 1_u32 } else { 0_u32 };
        env.events().publish(
            (symbol_short!("disp_r"), originator.clone()),
            event_data,
        );
    }

    // ── Volume recording ────────────────────────────────────────────────────

    /// Record invoice volume for an originator. Only the configured volume
    /// recorder (the registry contract) may call this.
    ///
    /// Increments the originator's invoice count, adds `amount` to total
    /// volume, and updates the last action timestamp.
    pub fn record_invoice_volume(env: Env, originator: Address, amount: i128) {
        assert_not_paused(&env);
        let recorder: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("vol_rec_a"))
            .unwrap_or_else(|| panic!("No volume recorder configured"));
        recorder.require_auth();

        let mut volumes = load_volume_records(&env);
        let mut record = volumes.get(originator.clone()).unwrap_or(VolumeRecord {
            invoice_count: 0,
            total_volume: 0,
            last_action_timestamp: 0,
        });
        record.invoice_count += 1;
        record.total_volume += amount;
        record.last_action_timestamp = env.ledger().timestamp();
        volumes.set(originator.clone(), record);
        save_volume_records(&env, &volumes);

        env.events().publish(
            (symbol_short!("vol_rec"), originator.clone()),
            amount,
        );
    }

    // ── Query helpers (public, read-only) ───────────────────────────────────

    /// The originator's simple reputation score: `repayments - 2 * defaults`,
    /// floored at 0. No auth required to query.
    pub fn get_score(env: Env, originator: Address) -> i128 {
        let record = load_records(&env)
            .get(originator)
            .unwrap_or(ReputationRecord {
                repayments: 0,
                defaults: 0,
            });
        let score = record.repayments as i128 - 2 * record.defaults as i128;
        score.max(0)
    }

    /// Raw outcome counts for transparency and auditability.
    pub fn get_record(env: Env, originator: Address) -> ReputationRecord {
        load_records(&env)
            .get(originator)
            .unwrap_or(ReputationRecord {
                repayments: 0,
                defaults: 0,
            })
    }

    /// Read an originator's dispute record.
    pub fn get_dispute_record(env: Env, originator: Address) -> DisputeRecord {
        load_dispute_records(&env)
            .get(originator)
            .unwrap_or(DisputeRecord {
                disputes_won: 0,
                disputes_lost: 0,
            })
    }

    /// Read an originator's volume record.
    pub fn get_volume_record(env: Env, originator: Address) -> VolumeRecord {
        load_volume_records(&env)
            .get(originator)
            .unwrap_or(VolumeRecord {
                invoice_count: 0,
                total_volume: 0,
                last_action_timestamp: 0,
            })
    }

    /// Compute the dispute sub-score: `disputes_won - disputes_lost`, floored
    /// at 0. Each won dispute contributes 1 point, each lost dispute
    /// subtracts 1 point.
    pub fn get_dispute_score(env: Env, originator: Address) -> i128 {
        let record = load_dispute_records(&env)
            .get(originator)
            .unwrap_or(DisputeRecord {
                disputes_won: 0,
                disputes_lost: 0,
            });
        let score = record.disputes_won as i128 - record.disputes_lost as i128;
        score.max(0)
    }

    /// Compute the volume sub-score: `invoice_count`. Each registered
    /// invoice contributes 1 point.
    pub fn get_volume_score(env: Env, originator: Address) -> i128 {
        let record = load_volume_records(&env)
            .get(originator)
            .unwrap_or(VolumeRecord {
                invoice_count: 0,
                total_volume: 0,
                last_action_timestamp: 0,
            });
        record.invoice_count as i128
    }

    /// Unified reputation score aggregating repayment history (60%),
    /// dispute outcomes (20%), and invoice volume (20%).
    ///
    /// Formula: `(repayment_score * 60 + dispute_score * 20 + volume_score * 20) / 100`
    pub fn get_unified_score(env: Env, originator: Address) -> i128 {
        let repayment_score = Self::get_score(env.clone(), originator.clone());
        let dispute_score = Self::get_dispute_score(env.clone(), originator.clone());
        let volume_score = Self::get_volume_score(env.clone(), originator.clone());

        (repayment_score * 60 + dispute_score * 20 + volume_score * 20) / 100
    }

    /// Effective reputation score with time-decay applied.
    ///
    /// Applies `0.95 ^ days_since_last_action` decay to the unified score.
    /// If no actions have been recorded (last_action_timestamp == 0), returns
    /// the unified score without decay.
    pub fn get_effective_score(env: Env, originator: Address) -> i128 {
        let unified = Self::get_unified_score(env.clone(), originator.clone());
        let vol = load_volume_records(&env)
            .get(originator)
            .unwrap_or(VolumeRecord {
                invoice_count: 0,
                total_volume: 0,
                last_action_timestamp: 0,
            });

        if vol.last_action_timestamp == 0 {
            return unified;
        }

        let now = env.ledger().timestamp();
        if now <= vol.last_action_timestamp {
            return unified;
        }

        let elapsed_secs = now - vol.last_action_timestamp;
        let days = elapsed_secs / 86_400; // seconds per day

        if days == 0 {
            return unified;
        }

        let decay_num = time_decay_numerator(days);
        // unified * decay_num / 100
        unified * decay_num / 100
    }

    /// Determine an originator's reputation tier based on their effective
    /// score. Tiers are: Bronze (0–99), Silver (100–499), Gold (500–999),
    /// Platinum (1000+).
    pub fn get_tier(env: Env, originator: Address) -> ReputationTier {
        let score = Self::get_effective_score(env, originator);
        if score >= PLATINUM_THRESHOLD {
            ReputationTier::Platinum
        } else if score >= GOLD_THRESHOLD {
            ReputationTier::Gold
        } else if score >= SILVER_THRESHOLD {
            ReputationTier::Silver
        } else {
            ReputationTier::Bronze
        }
    }

    /// Governance voting weight: `effective_score / 100`. A weight of 0 means
    /// the originator cannot meaningfully influence governance. Minimum
    /// effective score of 100 (Silver tier) is required for proposal creation.
    pub fn get_governance_weight(env: Env, originator: Address) -> u32 {
        let score = Self::get_effective_score(env, originator);
        (score / 100).max(0) as u32
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod test;
