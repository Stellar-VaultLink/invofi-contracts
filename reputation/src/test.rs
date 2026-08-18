#![cfg(test)]
extern crate std;

use super::{ReputationContract, OUTCOME_DEFAULTED, OUTCOME_REPAID};
use soroban_sdk::{testutils::Address as _, Address, Env};

/// Deploy the reputation contract and initialize.
fn setup<'a>(env: &'a Env, admin: &Address) -> super::ReputationContractClient<'a> {
    let rep_id = env.register(ReputationContract, (admin.clone(),));
    let client = super::ReputationContractClient::new(env, &rep_id);
    client
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

    client.pause(&admin);
    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(result.is_err(), "state-changing function should panic while paused");
    }

    assert_paused(|| {
        client.record_outcome(&originator, &OUTCOME_REPAID);
    });
    assert_paused(|| {
        client.set_recorder(&admin, &recorder);
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
