#![no_std]

//! Reputation contract (Task 11).
//!
//! Tracks repayment outcomes per originator so lenders can screen borrowers
//! before making an offer. The repayment contract (the configured recorder)
//! calls `record_outcome` after every fully-repaid invoice (outcome 0 =
//! success) and every default (outcome 1 = default). Anyone can read the
//! resulting score — no auth required to query.
//!
//! Scoring formula (documented, deliberately simple — ADR-0004):
//! `score = successful_repayments * 1 - defaults * 2`, floored at 0.
//!
//! Score decay (issue #139): outcomes older than `DECAY_HALF_LIFE_SECS`
//! contribute less via exponential decay. The cumulative weighted values
//! (`weighted_repayments`, `weighted_defaults`) are recomputed on each
//! `record_outcome` or `resolve_dispute` call: pending decay is applied
//! first (scaling existing values by `2^(-elapsed / half_life)`), then
//! the new outcome is added at full weight. `get_score` reads the cached
//! recomputed value in O(1). A `ReputationChanged` event (`rep_chg`) is
//! emitted whenever the recomputation changes the stored score.
//!
//! Disputes (issue #134): a default that is later overturned by a dispute
//! resolving in the originator's favour is neutralized via the admin-only
//! `resolve_dispute` — the `-2` penalty stops counting against them. The
//! rule is documented in ADR-0004 §7.

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, BytesN, Env, Map, String, Vec,
};

use invofi_common::{assert_not_paused, AdminConfig, ContractError};

/// Threshold-gated admin check (ADR-0010). See `invofi_common::assert_threshold`.
fn assert_admin(env: &Env, signers: &Vec<Address>) {
    let cfg = invofi_common::load_admin_config(env);
    invofi_common::assert_threshold(env, &cfg, signers);
}

fn pre_upgrade(_env: &Env) {}
fn post_upgrade(_env: &Env) {}

/// Outcome discriminant for a successful full repayment.
pub const OUTCOME_REPAID: u32 = 0;
/// Outcome discriminant for a default.
pub const OUTCOME_DEFAULTED: u32 = 1;

/// Exponential-decay half-life in seconds. Outcomes older than this
/// contribute less than half their original weight; after 2× half-life
/// they contribute less than a quarter, and so on.
pub const DECAY_HALF_LIFE_SECS: u64 = 7_776_000; // 90 days

/// Per-originator reputation state, including both raw outcome counts
/// (the source of truth) and cumulative weighted values used for the
/// decayed score computation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationRecord {
    /// Total number of successful repayments recorded (monotonic).
    pub repayments: u32,
    /// Total number of defaults recorded (monotonic).
    pub defaults: u32,
    /// Cumulative weighted repayments (1× per repayment, decayed over
    /// time). Used to compute the decayed score without iterating over
    /// individual outcomes.
    pub weighted_repayments: i128,
    /// Cumulative weighted defaults (2× per default, decayed over time).
    /// Stored as negative contribution to the score.
    pub weighted_defaults: i128,
    /// Ledger timestamp (seconds since Unix epoch) when the cumulative
    /// weighted values were last recomputed. `get_score` reads this
    /// cached value in O(1); the next `record_outcome` or
    /// `resolve_dispute` applies pending decay before updating.
    pub last_recompute: u64,
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

/// Apply pending exponential decay to a record's cumulative weighted
/// values, based on elapsed time since `last_recompute`. Scales both
/// `weighted_repayments` and `weighted_defaults` by
/// `2^(-elapsed / DECAY_HALF_LIFE_SECS)`, then updates `last_recompute`
/// to `now`.
fn apply_pending_decay(record: &mut ReputationRecord, now: u64) {
    if now > record.last_recompute && record.last_recompute > 0 {
        let elapsed = now - record.last_recompute;
        let half_life = DECAY_HALF_LIFE_SECS as f64;
        let scale = libm::pow(0.5, elapsed as f64 / half_life);
        record.weighted_repayments =
            (record.weighted_repayments as f64 * scale) as i128;
        record.weighted_defaults =
            (record.weighted_defaults as f64 * scale) as i128;
    }
    record.last_recompute = now;
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
        invofi_common::init_admin_config(&env, &admin);
        invofi_common::initialize_contract_version(&env, env!("CARGO_PKG_VERSION"));
    }

    /// Returns the primary admin address (the first configured signer). See
    /// `RegistryContract::get_admin` for the same caveat under true M-of-N.
    pub fn get_admin(env: Env) -> Address {
        invofi_common::load_admin_config(&env)
            .signers
            .get(0)
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// The full M-of-N admin governance config. See ADR-0010.
    pub fn get_admin_config(env: Env) -> AdminConfig {
        invofi_common::load_admin_config(&env)
    }

    /// The current signer set.
    pub fn get_signers(env: Env) -> Vec<Address> {
        invofi_common::load_admin_config(&env).signers
    }

    /// The current approval threshold.
    pub fn get_threshold(env: Env) -> u32 {
        invofi_common::load_admin_config(&env).threshold
    }

    /// Reconfigure the admin signer set and threshold. See
    /// `RegistryContract::set_signers`.
    pub fn set_signers(
        env: Env,
        signers: Vec<Address>,
        new_signers: Vec<Address>,
        new_threshold: u32,
    ) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        invofi_common::validate_signers(&env, &new_signers, new_threshold);
        invofi_common::save_admin_config(
            &env,
            &AdminConfig {
                signers: new_signers,
                threshold: new_threshold,
            },
        );
    }

    /// Transfers admin rights, collapsing the config back to a single new
    /// admin. See `RegistryContract::transfer_admin`.
    pub fn transfer_admin(env: Env, signers: Vec<Address>, new_admin: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        let mut new_signers = Vec::new(&env);
        new_signers.push_back(new_admin);
        invofi_common::save_admin_config(
            &env,
            &AdminConfig {
                signers: new_signers,
                threshold: 1,
            },
        );
    }

    /// Configure the address allowed to record outcomes — this is the
    /// repayment contract. Admin only. Recording is disabled until a
    /// recorder is configured (fail-closed).
    pub fn set_recorder(env: Env, signers: Vec<Address>, recorder: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("recorder"), &recorder);
    }

    pub fn get_recorder(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("recorder"))
    }

    // ── Pause / unpause (Task 4A circuit breaker) ───────────────────────────

    pub fn pause(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &true);
    }

    pub fn unpause(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
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

    // ── Outcome recording ───────────────────────────────────────────────────

    /// Record an outcome for an originator. Only the configured recorder (the
    /// repayment contract) may call this, authorized via implicit
    /// contract-invoker auth.
    ///
    /// `outcome`: 0 = successful full repayment, 1 = default. Anything else
    /// panics. Counts are monotonic — calling the same outcome twice for the
    /// same invoice would double count, so repayment must call this exactly
    /// once per terminal outcome (once on full repay, once on reclaim).
    ///
    /// Before recording, pending exponential decay (issue #139) is applied
    /// to the cumulative weighted values. The new outcome is added at full
    /// weight. A `ReputationChanged` event (`rep_chg`) is emitted when the
    /// recomputation changes the stored score.
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

        let now = env.ledger().timestamp();
        let mut records = load_records(&env);
        let mut record = records.get(originator.clone()).unwrap_or(ReputationRecord {
            repayments: 0,
            defaults: 0,
            weighted_repayments: 0,
            weighted_defaults: 0,
            last_recompute: 0,
        });

        let old_score = (record.weighted_repayments - record.weighted_defaults).max(0);

        // Apply pending decay before adding the new outcome.
        apply_pending_decay(&mut record, now);

        if outcome == OUTCOME_REPAID {
            record.repayments += 1;
            record.weighted_repayments += 1;
        } else {
            record.defaults += 1;
            record.weighted_defaults += 2;
        }

        let new_score = (record.weighted_repayments - record.weighted_defaults).max(0);

        records.set(originator.clone(), record);
        save_records(&env, &records);

        env.events()
            .publish((symbol_short!("reputn"), originator.clone()), outcome);

        if new_score != old_score {
            env.events()
                .publish((symbol_short!("rep_chg"), originator.clone()), new_score);
        }
    }

    /// Adjust an originator's recorded outcome after a dispute resolution.
    /// Admin only — mirrors the admin-only `resolve_dispute` in the
    /// registry. After the admin resolves a disputed invoice, they call
    /// this so the resolution is reflected in the originator's score.
    ///
    /// Documented rule (ADR-0004 §7): when `originator_favourable` is
    /// true, one previously-recorded default is neutralized — `defaults`
    /// decrements by one (floored at 0) — so the `-2` penalty stops
    /// counting against the originator. When false, the recorded outcome
    /// stands unchanged: the penalty, if already applied by
    /// `record_outcome`, remains.
    ///
    /// Before adjusting, pending exponential decay (issue #139) is applied
    /// to the cumulative weighted values. The disputed default's weight is
    /// removed from `weighted_defaults` (but its contribution to the raw
    /// count is simply decremented). A `ReputationChanged` event (`rep_chg`)
    /// is emitted when the recomputation changes the stored score.
    ///
    /// `get_score` stays public and read-only. Returns the originator's
    /// corrected score.
    pub fn resolve_dispute(
        env: Env,
        signers: Vec<Address>,
        originator: Address,
        originator_favourable: bool,
    ) -> i128 {
        assert_not_paused(&env);
        assert_admin(&env, &signers);

        let now = env.ledger().timestamp();
        let mut records = load_records(&env);
        let mut record = records.get(originator.clone()).unwrap_or(ReputationRecord {
            repayments: 0,
            defaults: 0,
            weighted_repayments: 0,
            weighted_defaults: 0,
            last_recompute: 0,
        });

        let old_score = (record.weighted_repayments - record.weighted_defaults).max(0);

        // Apply pending decay before adjusting.
        apply_pending_decay(&mut record, now);

        if originator_favourable && record.defaults > 0 {
            record.defaults -= 1;
            record.weighted_defaults = (record.weighted_defaults - 2).max(0);
        }

        let new_score = (record.weighted_repayments - record.weighted_defaults).max(0);

        records.set(originator.clone(), record);
        save_records(&env, &records);

        if new_score != old_score {
            env.events()
                .publish((symbol_short!("rep_chg"), originator.clone()), new_score);
        }

        new_score
    }

    // ── Query helpers (public, read-only) ───────────────────────────────────

    /// The originator's reputation score. Returns the cached decayed value
    /// (issue #139) — recomputed on each `record_outcome` or
    /// `resolve_dispute` call. No auth required to query. O(1) read.
    pub fn get_score(env: Env, originator: Address) -> i128 {
        let record = load_records(&env)
            .get(originator)
            .unwrap_or(ReputationRecord {
                repayments: 0,
                defaults: 0,
                weighted_repayments: 0,
                weighted_defaults: 0,
                last_recompute: 0,
            });
        (record.weighted_repayments - record.weighted_defaults).max(0)
    }

    /// Raw outcome counts for transparency and auditability. The counts
    /// are monotonic (never decremented except by dispute resolution).
    pub fn get_record(env: Env, originator: Address) -> ReputationRecord {
        load_records(&env)
            .get(originator)
            .unwrap_or(ReputationRecord {
                repayments: 0,
                defaults: 0,
                weighted_repayments: 0,
                weighted_defaults: 0,
                last_recompute: 0,
            })
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        invofi_common::contract_version(&env)
    }

    pub fn upgrade(
        env: Env,
        signers: Vec<Address>,
        current_wasm_hash: BytesN<32>,
        new_wasm_hash: BytesN<32>,
        new_version: String,
    ) {
        assert_admin(&env, &signers);
        invofi_common::begin_upgrade(&env, &current_wasm_hash, &new_wasm_hash, &new_version);
        pre_upgrade(&env);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    pub fn post_upgrade(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        post_upgrade(&env);
        invofi_common::complete_upgrade(&env);
    }

    pub fn rollback(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        let (wasm_hash, version) = invofi_common::rollback_target(&env);
        invofi_common::commit_rollback(&env, &version);
        env.deployer().update_current_contract_wasm(wasm_hash);
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
