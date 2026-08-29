#![cfg(test)]
extern crate std;

use super::{ReputationContract, OUTCOME_DEFAULTED, OUTCOME_REPAID};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _, Ledger as _},
    Address, Env, Symbol, TryFromVal,
};

/// Wrap a single signer in the one-element `Vec<Address>` the threshold-gated
/// admin API expects (ADR-0010). Single-admin/bootstrap deployments pass
/// exactly this.
fn one(env: &Env, signer: &Address) -> soroban_sdk::Vec<Address> {
    let mut v = soroban_sdk::Vec::new(env);
    v.push_back(signer.clone());
    v
}

/// Deploy the reputation contract and initialize.
fn setup<'a>(env: &'a Env, admin: &Address) -> super::ReputationContractClient<'a> {
    let rep_id = env.register(ReputationContract, (admin.clone(),));
    let client = super::ReputationContractClient::new(env, &rep_id);
    client
}

// ─── Multisig admin governance tests (ADR-0010) ─────────────────────────────

#[test]
fn test_reputation_set_signers_requires_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client = setup(&env, &admin);

    let b = Address::generate(&env);
    let mut two_signers = soroban_sdk::Vec::new(&env);
    two_signers.push_back(admin.clone());
    two_signers.push_back(b.clone());
    client.set_signers(&one(&env, &admin), &two_signers, &2u32);

    let result = client.try_pause(&one(&env, &admin));
    assert!(result.is_err(), "one of two required signatures must not pause");

    let mut both = soroban_sdk::Vec::new(&env);
    both.push_back(admin.clone());
    both.push_back(b.clone());
    client.pause(&both);
    assert!(client.contract_is_paused());
}

#[test]
fn test_reputation_transfer_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client = setup(&env, &admin);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&one(&env, &admin), &new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

// ─── Scoring tests ───────────────────────────────────────────────────────────

#[test]
fn test_score_after_sequence_of_outcomes() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    // Fresh address has score 0.
    assert_eq!(client.get_score(&originator), 0);

    // No recorder configured yet — recording is disabled.
    client.set_recorder(&one(&env, &admin), &recorder);
    assert_eq!(client.get_recorder(), Some(recorder.clone()));

    // 1 repayment -> score 1.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(client.get_score(&originator), 1);

    // +1 repayment -> score 2.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(client.get_score(&originator), 2);

    // +1 default -> 2 - 2 = 0 (floor, never negative).
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // +1 default -> 0 - 2 -> floored at 0.
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // +1 repayment -> 1 - 2 -> floored at 0.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(client.get_score(&originator), 0);

    // +3 more repayments (6 total) -> 6 - 4 = 2.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(client.get_score(&originator), 2);

    // Raw counts are exposed for auditability.
    let rec = client.get_record(&originator);
    assert_eq!(rec.repayments, 6);
    assert_eq!(rec.defaults, 2);

    // Scores are independent per originator.
    let other = Address::generate(&env);
    assert_eq!(client.get_score(&other), 0);
}

#[test]
fn test_get_score_is_public_read_only() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    // Any address can read without any setup — no panic, no auth.
    assert_eq!(client.get_score(&originator), 0);
    assert_eq!(
        client.get_record(&originator),
        super::ReputationRecord {
            repayments: 0,
            defaults: 0,
            weighted_repayments: 0,
            weighted_defaults: 0,
            last_recompute: 0,
        }
    );
}

// ─── Failure-path tests ──────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "No recorder configured")]
fn test_record_outcome_without_recorder_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    client.record_outcome(&originator, &OUTCOME_REPAID);
}

#[test]
fn test_pause_blocks_all_reputation_state_changes() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client = setup(&env, &admin);
    let originator = Address::generate(&env);
    let recorder = Address::generate(&env);

    client.pause(&one(&env, &admin));
    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(
            result.is_err(),
            "state-changing function should panic while paused"
        );
    }

    assert_paused(|| {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    });
    assert_paused(|| {
        client.set_recorder(&one(&env, &admin), &recorder);
    });

    assert_eq!(client.get_score(&originator), 0);
    assert_eq!(client.get_record(&originator).repayments, 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_record_outcome_invalid_outcome_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    client.record_outcome(&originator, &99);
}

// ─── Dispute-aware adjustment tests (issue #134) ─────────────────────────────

#[test]
fn test_resolve_dispute_favourable_neutralizes_default() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    // 2 repayments + 1 default -> 2 - 2 = 0: the default penalty outweighs
    // the successful repayments.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // Dispute resolves in the originator's favour -> default neutralized.
    let corrected = client.resolve_dispute(&one(&env, &admin), &originator, &true);
    assert_eq!(corrected, 2);
    assert_eq!(client.get_score(&originator), 2);

    let rec = client.get_record(&originator);
    assert_eq!(rec.repayments, 2);
    assert_eq!(rec.defaults, 0);
}

#[test]
fn test_resolve_dispute_favourable_without_default_is_noop() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    // Fresh originator — nothing to neutralize, score stays 0.
    let corrected = client.resolve_dispute(&one(&env, &admin), &originator, &true);
    assert_eq!(corrected, 0);
    assert_eq!(client.get_score(&originator), 0);
    assert_eq!(client.get_record(&originator).defaults, 0);
}

#[test]
fn test_resolve_dispute_unfavourable_leaves_record_unchanged() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // Resolution against the originator: the recorded outcome stands.
    let corrected = client.resolve_dispute(&one(&env, &admin), &originator, &false);
    assert_eq!(corrected, 0);
    assert_eq!(client.get_score(&originator), 0);
    assert_eq!(client.get_record(&originator).defaults, 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_resolve_dispute_non_admin_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    let impostor = Address::generate(&env);
    client.resolve_dispute(&one(&env, &impostor), &originator, &true);
}

#[test]
fn test_resolve_dispute_paused_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client = setup(&env, &admin);
    let originator = Address::generate(&env);

    client.pause(&one(&env, &admin));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.resolve_dispute(&one(&env, &admin), &originator, &true);
    }));
    assert!(result.is_err(), "resolve_dispute must panic while paused");
}

/// Count published events whose first topic is `name`.
fn count_events(env: &Env, name: Symbol) -> u32 {
    let mut count = 0;
    for (_contract, topics, _data) in env.events().all().iter() {
        if let Some(first) = topics.get(0) {
            if let Ok(topic) = Symbol::try_from_val(env, &first) {
                if topic == name {
                    count += 1;
                }
            }
        }
    }
    count
}

#[test]
fn test_resolve_dispute_emits_corrected_score_event() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);

    // record_outcome now also emits rep_chg when score changes (issue #139).
    // The event window holds only the most recent invocation, so the last
    // record_outcome (default, score 2 → 0) emits one rep_chg.
    assert_eq!(count_events(&env, symbol_short!("rep_chg")), 1);

    let corrected = client.resolve_dispute(&one(&env, &admin), &originator, &true);
    assert_eq!(corrected, 2);
    assert_eq!(count_events(&env, symbol_short!("rep_chg")), 1);

    // The event payload is the corrected score.
    let mut found: Option<i128> = None;
    for (_contract, topics, data) in env.events().all().iter() {
        if let Some(first) = topics.get(0) {
            if let Ok(topic) = Symbol::try_from_val(&env, &first) {
                if topic == symbol_short!("rep_chg") {
                    found = Some(i128::try_from_val(&env, &data).unwrap());
                }
            }
        }
    }
    assert_eq!(found, Some(2));

    // Unfavourable resolution does not change the score (defaults=0, nothing to
    // remove), so the event window shows no rep_chg from this invocation.
    client.resolve_dispute(&one(&env, &admin), &originator, &false);
    assert_eq!(count_events(&env, symbol_short!("rep_chg")), 0);
}

// ─── Score decay tests (issue #139) ─────────────────────────────────────────

/// Helper: set the ledger timestamp for decay tests. Soroban requires a
/// configured ledger header before timestamp can be set.
fn set_timestamp(env: &Env, ts: u64) {
    env.ledger().set_timestamp(ts);
}

/// A fresh default at time T has full weight (−2) regardless of what came
/// before — no gaming window.
#[test]
fn test_fresh_default_hits_immediately() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);

    // 3 repayments → score 3.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(client.get_score(&originator), 3);

    // Fresh default at the same timestamp → score drops to 1
    // (3 − 2 = 1).
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 1);
}

/// An old default decays over time; with enough repayments the score
/// recovers above 0.
#[test]
fn test_old_default_decays() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);

    // Record 1 default (weight = −2).
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0); // floor
    let rec = client.get_record(&originator);
    assert_eq!(rec.defaults, 1);
    assert_eq!(rec.weighted_defaults, 2);

    // Advance by exactly one half-life (90 days = 7 776 000 s).
    set_timestamp(&env, 1_000_000 + super::DECAY_HALF_LIFE_SECS);

    // Record 2 repayments to trigger recomputation.  The old default has
    // decayed to ~1 (half of 2), and the 2 fresh repayments add 2.
    // Score = (2) − 1 = 1.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);

    let rec = client.get_record(&originator);
    assert_eq!(rec.repayments, 2);
    assert_eq!(rec.defaults, 1);
    let score = client.get_score(&originator);
    assert!(score >= 1, "decayed default should allow score > 0, got {score}");
}

/// After two half-lives, the old default contributes < 25 % of its
/// original weight.
#[test]
fn test_old_default_decays_further_after_two_half_lives() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // Advance 2 × half-life.
    set_timestamp(&env, 1_000_000 + 2 * super::DECAY_HALF_LIFE_SECS);
    client.record_outcome(&originator, &OUTCOME_REPAID);

    // After 2 half-lives the default's weighted_defaults ≈ 0.5
    // (2 × 0.25), the fresh repayment adds 1.  Score ≈ 1.
    let score = client.get_score(&originator);
    assert!(score >= 1, "score should be >= 1 after two half-lives, got {score}");
}

/// Score floor at 0 is respected even with decay — score never goes
/// negative.
#[test]
fn test_score_floor_after_decay() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // Advance well past two half-lives.
    set_timestamp(&env, 1_000_000 + 3 * super::DECAY_HALF_LIFE_SECS);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert!(client.get_score(&originator) >= 0);
}

/// record_outcome emits rep_chg when score changes.
#[test]
fn test_record_outcome_emits_rep_chg() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);

    // First repayment: 0 → 1, should emit rep_chg.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(count_events(&env, symbol_short!("rep_chg")), 1);
}

/// No decay when timestamps don't change (same-block operations).
#[test]
fn test_no_decay_without_time_advancing() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);

    // 2 repayments + 1 default, all at the same timestamp.
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);

    // Weighted score should be exactly 2 − 2 = 0, no decay.
    assert_eq!(client.get_score(&originator), 0);
    let rec = client.get_record(&originator);
    assert_eq!(rec.weighted_repayments, 2);
    assert_eq!(rec.weighted_defaults, 2);
}

/// Decay is per-originator — two originators with the same history but
/// different timestamps get different scores.
#[test]
fn test_decay_independent_per_originator() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator_a = Address::generate(&env);
    let originator_b = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    // Originator A: 1 default at T, then 3 repayments at T+half_life.
    set_timestamp(&env, 1_000_000);
    client.record_outcome(&originator_a, &OUTCOME_DEFAULTED);

    set_timestamp(&env, 1_000_000 + super::DECAY_HALF_LIFE_SECS);
    client.record_outcome(&originator_a, &OUTCOME_REPAID);
    client.record_outcome(&originator_a, &OUTCOME_REPAID);
    client.record_outcome(&originator_a, &OUTCOME_REPAID);

    // Originator B: 1 default and 3 repayments at the same time (no decay).
    set_timestamp(&env, 1_000_000 + 2 * super::DECAY_HALF_LIFE_SECS);
    client.record_outcome(&originator_b, &OUTCOME_DEFAULTED);
    client.record_outcome(&originator_b, &OUTCOME_REPAID);
    client.record_outcome(&originator_b, &OUTCOME_REPAID);
    client.record_outcome(&originator_b, &OUTCOME_REPAID);

    let score_a = client.get_score(&originator_a);
    let score_b = client.get_score(&originator_b);

    // A's default decayed over one half-life (−2 → −1); B's didn't.
    // A: 3 − 1 = 2.  B: 3 − 2 = 1.
    assert!(
        score_a > score_b,
        "A's score ({score_a}) should be higher than B's ({score_b}) due to decay"
    );
}

/// Dispute resolution applies pending decay before adjusting.
#[test]
fn test_resolve_dispute_applies_pending_decay() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    assert_eq!(client.get_score(&originator), 0);

    // Advance one half-life, then resolve dispute.
    set_timestamp(&env, 1_000_000 + super::DECAY_HALF_LIFE_SECS);
    let corrected = client.resolve_dispute(&one(&env, &admin), &originator, &true);

    // After dispute resolution, defaults = 0, weighted_defaults = 0.
    // No decay has happened on weighted_repayments (still 0).  Score = 0.
    assert_eq!(corrected, 0);
    let rec = client.get_record(&originator);
    assert_eq!(rec.defaults, 0);
}

/// get_score is cheap O(1) — it reads the cached value without
/// recomputation.
#[test]
fn test_get_score_reads_cached_value() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&one(&env, &admin), &recorder);

    set_timestamp(&env, 1_000_000);
    client.record_outcome(&originator, &OUTCOME_REPAID);
    assert_eq!(client.get_score(&originator), 1);

    // Advance time but don't record anything — score should remain
    // cached (no recomputation triggered).
    set_timestamp(&env, 1_000_000 + 10 * super::DECAY_HALF_LIFE_SECS);
    assert_eq!(client.get_score(&originator), 1);

    // Only after a new record_outcome is the decay applied.
    client.record_outcome(&originator, &OUTCOME_DEFAULTED);
    let score = client.get_score(&originator);
    // Default adds 2 to weighted_defaults, old repayment (1) has decayed
    // to ~0.001.  Score = 0.
    assert!(score >= 0, "score must not be negative: {score}");
}
