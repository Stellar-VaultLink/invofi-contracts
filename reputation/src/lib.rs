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
//! A richer weighted model (amount-weighted, recency decay) is tracked as a
//! follow-up issue; see ADR-0004.
//!
//! Disputes (issue #134): a default that is later overturned by a dispute
//! resolving in the originator's favour is neutralized via the admin-only
//! `resolve_dispute` — the `-2` penalty stops counting against them. The
//! rule is documented in ADR-0004 §7.

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Map};

use invofi_common::{assert_not_paused, ContractError};

/// Outcome discriminant for a successful full repayment.
pub const OUTCOME_REPAID: u32 = 0;
/// Outcome discriminant for a default.
pub const OUTCOME_DEFAULTED: u32 = 1;

/// Per-originator outcome counts.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationRecord {
    pub repayments: u32,
    pub defaults: u32,
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

    // ── Pause / unpause (Task 4A circuit breaker) ───────────────────────────

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

    // ── Outcome recording ───────────────────────────────────────────────────

    /// Record an outcome for an originator. Only the configured recorder (the
    /// repayment contract) may call this, authorized via implicit
    /// contract-invoker auth.
    ///
    /// `outcome`: 0 = successful full repayment, 1 = default. Anything else
    /// panics. Counts are monotonic — calling the same outcome twice for the
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
    /// Emits `ReputationChanged` (topic `rep_chg`) with the corrected
    /// score when a default was neutralized. `get_score` stays public and
    /// read-only. Returns the originator's corrected score.
    pub fn resolve_dispute(
        env: Env,
        admin: Address,
        originator: Address,
        originator_favourable: bool,
    ) -> i128 {
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

        let mut records = load_records(&env);
        let mut record = records.get(originator.clone()).unwrap_or(ReputationRecord {
            repayments: 0,
            defaults: 0,
        });
        let mut score = (record.repayments as i128 - 2 * record.defaults as i128).max(0);
        if originator_favourable && record.defaults > 0 {
            record.defaults -= 1;
            score = (record.repayments as i128 - 2 * record.defaults as i128).max(0);
            records.set(originator.clone(), record);
            save_records(&env, &records);
            env.events()
                .publish((symbol_short!("rep_chg"), originator.clone()), score);
        }
        score
    }

    // ── Query helpers (public, read-only) ───────────────────────────────────

    /// The originator's reputation score: `repayments - 2 * defaults`,
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

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
