#![cfg(test)]
extern crate std;

use crate::{ReputationContract, OUTCOME_DEFAULTED, OUTCOME_REPAID};
use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup(env: &Env) -> (crate::ReputationContractClient<'static>, Address, Address) {
    let admin = Address::generate(env);
    let reputation_id = env.register(ReputationContract, (admin.clone(),));
    let repu = crate::ReputationContractClient::new(env, &reputation_id);
    let recorder = Address::generate(env);
    repu.set_recorder(&admin, &recorder);
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
}
