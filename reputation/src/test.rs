#![cfg(test)]
extern crate std;

use super::{ReputationContract, OUTCOME_DEFAULTED, OUTCOME_REPAID};
use invofi_common::ReputationTier;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

/// Deploy the reputation contract and initialize.
fn setup<'a>(env: &'a Env, admin: &Address) -> super::ReputationContractClient<'a> {
    let rep_id = env.register(ReputationContract, (admin.clone(),));
    super::ReputationContractClient::new(env, &rep_id)
}

/// Deploy the reputation contract with all recorders configured.
fn setup_full<'a>(
    env: &'a Env,
    admin: &Address,
    recorder: &Address,
    dispute_recorder: &Address,
    volume_recorder: &Address,
) -> super::ReputationContractClient<'a> {
    let client = setup(env, admin);
    client.set_recorder(admin, recorder);
    client.set_dispute_recorder(admin, dispute_recorder);
    client.set_volume_recorder(admin, volume_recorder);
    client
}

// ─── Original scoring tests ──────────────────────────────────────────────────

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
    client.set_recorder(&admin, &recorder);
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
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);

    client.pause(&admin);
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
        client.set_recorder(&admin, &recorder);
    });
    assert_paused(|| {
        client.set_dispute_recorder(&admin, &dispute_rec);
    });
    assert_paused(|| {
        client.set_volume_recorder(&admin, &volume_rec);
    });
    assert_paused(|| {
        client.record_dispute_outcome(&originator, &true);
    });
    assert_paused(|| {
        client.record_invoice_volume(&originator, &1_000_000i128);
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
    client.set_recorder(&admin, &recorder);

    client.record_outcome(&originator, &99);
}

// ─── Dispute recording tests ─────────────────────────────────────────────────

#[test]
fn test_record_dispute_outcome_won() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_dispute_recorder(&admin, &dispute_rec);

    assert_eq!(client.get_dispute_score(&originator), 0);

    client.record_dispute_outcome(&originator, &true);
    assert_eq!(client.get_dispute_score(&originator), 1);

    let record = client.get_dispute_record(&originator);
    assert_eq!(record.disputes_won, 1);
    assert_eq!(record.disputes_lost, 0);
}

#[test]
fn test_record_dispute_outcome_lost() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_dispute_recorder(&admin, &dispute_rec);

    client.record_dispute_outcome(&originator, &false);
    assert_eq!(client.get_dispute_score(&originator), 0); // 0 - 1 = -1, floored at 0

    let record = client.get_dispute_record(&originator);
    assert_eq!(record.disputes_won, 0);
    assert_eq!(record.disputes_lost, 1);
}

#[test]
fn test_dispute_score_multiple() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_dispute_recorder(&admin, &dispute_rec);

    // 3 won, 1 lost -> score = 3 - 1 = 2
    client.record_dispute_outcome(&originator, &true);
    client.record_dispute_outcome(&originator, &true);
    client.record_dispute_outcome(&originator, &true);
    client.record_dispute_outcome(&originator, &false);

    assert_eq!(client.get_dispute_score(&originator), 2);

    let record = client.get_dispute_record(&originator);
    assert_eq!(record.disputes_won, 3);
    assert_eq!(record.disputes_lost, 1);
}

#[test]
#[should_panic(expected = "No dispute recorder configured")]
fn test_record_dispute_without_recorder_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    client.record_dispute_outcome(&originator, &true);
}

// ─── Volume recording tests ──────────────────────────────────────────────────

#[test]
fn test_record_invoice_volume() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let admin = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_volume_recorder(&admin, &volume_rec);

    assert_eq!(client.get_volume_score(&originator), 0);

    client.record_invoice_volume(&originator, &100_000_000i128);
    assert_eq!(client.get_volume_score(&originator), 1); // 1 invoice

    let record = client.get_volume_record(&originator);
    assert_eq!(record.invoice_count, 1);
    assert_eq!(record.total_volume, 100_000_000i128);
    assert_eq!(record.last_action_timestamp, 1_000);
}

#[test]
fn test_record_volume_accumulates() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let admin = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_volume_recorder(&admin, &volume_rec);

    client.record_invoice_volume(&originator, &50_000_000i128);

    env.ledger().set_timestamp(2_000);
    client.record_invoice_volume(&originator, &75_000_000i128);

    let record = client.get_volume_record(&originator);
    assert_eq!(record.invoice_count, 2);
    assert_eq!(record.total_volume, 125_000_000i128);
    assert_eq!(record.last_action_timestamp, 2_000);
    assert_eq!(client.get_volume_score(&originator), 2);
}

#[test]
#[should_panic(expected = "No volume recorder configured")]
fn test_record_volume_without_recorder_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    client.record_invoice_volume(&originator, &100_000_000i128);
}

// ─── Unified score tests ─────────────────────────────────────────────────────

#[test]
fn test_unified_score_only_repayment() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&admin, &recorder);

    // 5 repayments, 0 defaults -> simple score = 5
    for _ in 0..5 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }

    // Unified: (5 * 60 + 0 * 20 + 0 * 20) / 100 = 300 / 100 = 3
    assert_eq!(client.get_unified_score(&originator), 3);
}

#[test]
fn test_unified_score_with_disputes_and_volume() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    // 10 repayments, 0 defaults -> simple score = 10
    for _ in 0..10 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    // 3 disputes won, 1 lost -> dispute score = 2
    client.record_dispute_outcome(&originator, &true);
    client.record_dispute_outcome(&originator, &true);
    client.record_dispute_outcome(&originator, &true);
    client.record_dispute_outcome(&originator, &false);
    // 5 invoices -> volume score = 5
    for _ in 0..5 {
        client.record_invoice_volume(&originator, &50_000_000i128);
    }

    // Unified: (10 * 60 + 2 * 20 + 5 * 20) / 100 = (600 + 40 + 100) / 100 = 740 / 100 = 7
    assert_eq!(client.get_unified_score(&originator), 7);
}

#[test]
fn test_unified_score_zero_for_fresh_address() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    assert_eq!(client.get_unified_score(&originator), 0);
}

// ─── Effective score (time-decay) tests ──────────────────────────────────────

#[test]
fn test_effective_score_no_decay_when_no_actions() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);
    client.set_recorder(&admin, &recorder);

    for _ in 0..10 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }

    // No volume recorded -> last_action_timestamp is 0 -> no decay
    assert_eq!(client.get_effective_score(&originator), client.get_unified_score(&originator));
}

#[test]
fn test_effective_score_no_decay_within_same_day() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    for _ in 0..10 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    // Same timestamp -> 0 days elapsed -> no decay
    assert_eq!(client.get_effective_score(&originator), client.get_unified_score(&originator));
}

#[test]
fn test_effective_score_with_time_decay() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    for _ in 0..10 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    let unified = client.get_unified_score(&originator);
    assert!(unified > 0);

    // Advance 10 days
    env.ledger().set_timestamp(100_000 + 10 * 86_400);

    let effective = client.get_effective_score(&originator);
    // Should be less than unified due to decay
    assert!(effective < unified);
    // But still positive
    assert!(effective > 0);
}

#[test]
fn test_effective_score_more_decay_over_longer_period() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    for _ in 0..10 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    let unified = client.get_unified_score(&originator);

    // 10 days
    env.ledger().set_timestamp(100_000 + 10 * 86_400);
    let effective_10 = client.get_effective_score(&originator);

    // 30 days
    env.ledger().set_timestamp(100_000 + 30 * 86_400);
    let effective_30 = client.get_effective_score(&originator);

    assert!(effective_10 < unified);
    assert!(effective_30 < effective_10);
}

// ─── Tier tests ──────────────────────────────────────────────────────────────

#[test]
fn test_tier_bronze_for_zero_score() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    assert_eq!(client.get_tier(&originator), ReputationTier::Bronze);
}

#[test]
fn test_tier_silver() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    // Enough to get effective score >= 100 (Silver)
    // We need repayment_score * 60 / 100 >= 100
    // repayment_score needs to be >= 167 (167 * 60 / 100 = 100.2)
    for _ in 0..170 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    // Unified: (170 * 60 + 0 * 20 + 1 * 20) / 100 = (10200 + 20) / 100 = 102
    let effective = client.get_effective_score(&originator);
    assert!(effective >= 100, "effective score should be >= 100, got {}", effective);
    assert_eq!(client.get_tier(&originator), ReputationTier::Silver);
}

#[test]
fn test_tier_gold() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    // Need effective score >= 500 (Gold)
    // repayment_score * 60 / 100 >= 500 -> repayment_score >= 834
    for _ in 0..840 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    let effective = client.get_effective_score(&originator);
    assert!(effective >= 500, "effective score should be >= 500, got {}", effective);
    assert_eq!(client.get_tier(&originator), ReputationTier::Gold);
}

#[test]
fn test_tier_platinum() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    // Need effective score >= 1000 (Platinum)
    // repayment_score * 60 / 100 >= 1000 -> repayment_score >= 1667
    for _ in 0..1670 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    let effective = client.get_effective_score(&originator);
    assert!(effective >= 1000, "effective score should be >= 1000, got {}", effective);
    assert_eq!(client.get_tier(&originator), ReputationTier::Platinum);
}

// ─── Governance weight tests ─────────────────────────────────────────────────

#[test]
fn test_governance_weight_zero_for_bronze() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup(&env, &admin);

    // Bronze (score < 100) -> weight = 0
    assert_eq!(client.get_governance_weight(&originator), 0);
}

#[test]
fn test_governance_weight_proportional_to_score() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    for _ in 0..500 {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator, &50_000_000i128);

    let effective = client.get_effective_score(&originator);
    let weight = client.get_governance_weight(&originator);
    assert_eq!(weight, (effective / 100) as u32);
}

// ─── Dispute recorder / volume recorder admin tests ──────────────────────────

#[test]
fn test_set_and_get_dispute_recorder() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let client = setup(&env, &admin);

    assert_eq!(client.get_dispute_recorder(), None);
    client.set_dispute_recorder(&admin, &dispute_rec);
    assert_eq!(client.get_dispute_recorder(), Some(dispute_rec));
}

#[test]
fn test_set_and_get_volume_recorder() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let client = setup(&env, &admin);

    assert_eq!(client.get_volume_recorder(), None);
    client.set_volume_recorder(&admin, &volume_rec);
    assert_eq!(client.get_volume_recorder(), Some(volume_rec));
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_set_dispute_recorder_unauthorized_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let not_admin = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let client = setup(&env, &admin);

    client.set_dispute_recorder(&not_admin, &dispute_rec);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_set_volume_recorder_unauthorized_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let not_admin = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let client = setup(&env, &admin);

    client.set_volume_recorder(&not_admin, &volume_rec);
}

// ─── Independent originator scores ───────────────────────────────────────────

#[test]
fn test_scores_independent_per_originator() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    let dispute_rec = Address::generate(&env);
    let volume_rec = Address::generate(&env);
    let originator_a = Address::generate(&env);
    let originator_b = Address::generate(&env);
    let client = setup_full(&env, &admin, &recorder, &dispute_rec, &volume_rec);

    // Originator A: 5 repayments
    for _ in 0..5 {
        client.record_outcome(&originator_a, &OUTCOME_REPAID);
    }
    client.record_invoice_volume(&originator_a, &50_000_000i128);

    // Originator B: 10 repayments, 2 disputes won
    for _ in 0..10 {
        client.record_outcome(&originator_b, &OUTCOME_REPAID);
    }
    client.record_dispute_outcome(&originator_b, &true);
    client.record_dispute_outcome(&originator_b, &true);
    client.record_invoice_volume(&originator_b, &100_000_000i128);

    let score_a = client.get_unified_score(&originator_a);
    let score_b = client.get_unified_score(&originator_b);

    assert!(score_b > score_a, "B should score higher than A");
    assert_eq!(client.get_tier(&originator_a), ReputationTier::Bronze);
}

// ─── Version test ────────────────────────────────────────────────────────────

#[test]
fn test_version_returns_nonempty_string() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(ReputationContract, (Address::generate(&env),));
    let client = super::ReputationContractClient::new(&env, &contract_id);
    let ver = client.version();
    assert!(!ver.is_empty());
}
