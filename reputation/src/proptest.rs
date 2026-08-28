#![cfg(test)]
extern crate std;

use crate::{ReputationContract, OUTCOME_DEFAULTED, OUTCOME_REPAID};
use proptest::prelude::*;
use soroban_sdk::{testutils::{Address as _, Ledger as _}, Address, Env};

fn setup(env: &Env) -> (crate::ReputationContractClient<'static>, Address, Address) {
    let admin = Address::generate(env);
    let reputation_id = env.register(ReputationContract, (admin.clone(),));
    let repu = crate::ReputationContractClient::new(env, &reputation_id);
    let recorder = Address::generate(env);
    // ADR-0010: the admin API is threshold-gated; bootstrap deployments pass
    // a one-element signer list.
    let mut signers = soroban_sdk::Vec::new(env);
    signers.push_back(admin.clone());
    repu.set_recorder(&signers, &recorder);
    (repu, admin, recorder)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Floor enforcement — score = max(0, repayments - 2*defaults).
    #[test]
    fn test_score_never_negative(
        outcomes in prop::collection::vec(prop::bool::ANY, 1..30),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let (repu, _admin, _recorder) = setup(&env);
        let originator = Address::generate(&env);

        for is_default in outcomes {
            let outcome = if is_default { OUTCOME_DEFAULTED } else { OUTCOME_REPAID };
            repu.record_outcome(&originator, &outcome);
            prop_assert!(repu.get_score(&originator) >= 0);
        }
    }

    /// A repayment can never lower score — it can raise it or leave it
    /// unchanged (unchanged only when the raw, unfloored score was already
    /// <= -1 before this call, per the max(0, ...) formula).
    #[test]
    fn test_repayment_never_decreases_score(
        outcomes in prop::collection::vec(prop::bool::ANY, 1..30),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let (repu, _admin, _recorder) = setup(&env);
        let originator = Address::generate(&env);

        for is_default in outcomes {
            let before = repu.get_score(&originator);
            let outcome = if is_default { OUTCOME_DEFAULTED } else { OUTCOME_REPAID };
            repu.record_outcome(&originator, &outcome);
            let after = repu.get_score(&originator);
            if !is_default {
                prop_assert!(after >= before, "repayment must never decrease score");
            }
        }
    }

    /// A default can never raise score — it can lower it or leave it
    /// unchanged (unchanged only once the floor has already been hit).
    #[test]
    fn test_default_never_increases_score(
        outcomes in prop::collection::vec(prop::bool::ANY, 1..30),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let (repu, _admin, _recorder) = setup(&env);
        let originator = Address::generate(&env);

        for is_default in outcomes {
            let before = repu.get_score(&originator);
            let outcome = if is_default { OUTCOME_DEFAULTED } else { OUTCOME_REPAID };
            repu.record_outcome(&originator, &outcome);
            let after = repu.get_score(&originator);
            if is_default {
                prop_assert!(after <= before, "default must never increase score");
            }
        }
    }

    /// Same sequence of outcomes on two independent originators/contracts
    /// must always produce the same final score — the formula is a pure
    /// function of the counts.
    #[test]
    fn test_score_deterministic_for_same_sequence(
        outcomes in prop::collection::vec(prop::bool::ANY, 1..30),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let (repu_x, _a1, _r1) = setup(&env);
        let (repu_y, _a2, _r2) = setup(&env);
        let originator_x = Address::generate(&env);
        let originator_y = Address::generate(&env);

        for is_default in outcomes {
            let outcome = if is_default { OUTCOME_DEFAULTED } else { OUTCOME_REPAID };
            repu_x.record_outcome(&originator_x, &outcome);
            repu_y.record_outcome(&originator_y, &outcome);
        }

        prop_assert_eq!(repu_x.get_score(&originator_x), repu_y.get_score(&originator_y));
    }

    /// Decay property: a sequence of outcomes recorded at time T and then
    /// one fresh repayment at T + N*half_life must produce a score ≥ 0
    /// for any N (issue #139).
    #[test]
    fn test_decay_score_nonnegative_with_time_advancement(
        outcomes in prop::collection::vec(prop::bool::ANY, 1..10),
        half_lives in 1u64..20u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let (repu, _admin, _recorder) = setup(&env);
        let originator = Address::generate(&env);

        env.ledger().set_timestamp(1_000_000);
        for is_default in &outcomes {
            let outcome = if *is_default { OUTCOME_DEFAULTED } else { OUTCOME_REPAID };
            repu.record_outcome(&originator, &outcome);
        }

        // Advance well into the future.
        let advance = half_lives * crate::DECAY_HALF_LIFE_SECS;
        env.ledger().set_timestamp(1_000_000 + advance);

        // Record one fresh repayment to trigger recomputation.
        repu.record_outcome(&originator, &OUTCOME_REPAID);
        prop_assert!(repu.get_score(&originator) >= 0, "score must not be negative after decay");
    }

    /// Fresh outcome has full weight: recording one repayment at time T,
    /// advancing by N half-lives, then recording one default must produce
    /// a score that is at least 0 (floor) and never exceeds 1 (the
    /// repayment has decayed, but the default adds its full −2 weight).
    #[test]
    fn test_fresh_default_full_weight_after_decay(
        half_lives in 1u64..50u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let (repu, _admin, _recorder) = setup(&env);
        let originator = Address::generate(&env);

        env.ledger().set_timestamp(1_000_000);
        repu.record_outcome(&originator, &OUTCOME_REPAID);
        let before = repu.get_score(&originator);
        prop_assert_eq!(before, 1);

        // Advance.
        let advance = half_lives * crate::DECAY_HALF_LIFE_SECS;
        env.ledger().set_timestamp(1_000_000 + advance);

        // Fresh default at full weight (−2).
        repu.record_outcome(&originator, &OUTCOME_DEFAULTED);
        let after = repu.get_score(&originator);
        prop_assert!(after >= 0, "score must floor at 0, got {after}");
        // The old repayment has decayed but the fresh default has full weight.
        prop_assert!(after <= 1, "fresh default should overwhelm decayed repayment, got {after}");
    }
}
