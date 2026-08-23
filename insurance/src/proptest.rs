#![cfg(test)]
extern crate std;

use crate::InsuranceContract;
use invofi_registry::RegistryContract;
use proptest::prelude::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

/// Wrap a single signer in the one-element `Vec<Address>` the threshold-gated
/// admin API expects (ADR-0010). Single-admin/bootstrap deployments pass
/// exactly this.
fn one(env: &Env, signer: &Address) -> soroban_sdk::Vec<Address> {
    let mut v = soroban_sdk::Vec::new(env);
    v.push_back(signer.clone());
    v
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Pool total must equal the sum of every staker's balance at every
    /// point — no operation may let them drift apart.
    #[test]
    fn test_stake_keeps_pool_and_balances_in_sync(
        amounts in prop::collection::vec(1_000_000i128..1_000_000_000i128, 1..5),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token_id = sac.address();
        let asset_client = token::StellarAssetClient::new(&env, &token_id);

        let insurance_id = env.register(InsuranceContract, (admin.clone(), token_id.clone()));
        let ins = crate::InsuranceContractClient::new(&env, &insurance_id);

        let mut stakers = std::vec::Vec::new();
        let mut running_total: i128 = 0;
        for amount in amounts.iter() {
            let staker = Address::generate(&env);
            asset_client.mint(&staker, amount);
            let token_client = token::TokenClient::new(&env, &token_id);
            token_client.approve(&staker, &insurance_id, amount, &(env.ledger().sequence() + 1000));
            ins.stake(&staker, amount);
            running_total += amount;
            stakers.push((staker, *amount));

            let sum_of_balances: i128 = stakers.iter().map(|(s, _)| ins.get_stake(s)).sum();
            prop_assert_eq!(sum_of_balances, ins.get_pool_total());
            prop_assert_eq!(ins.get_pool_total(), running_total);
        }
    }

    /// Unstaking more than a staker's own balance must always be rejected.
    #[test]
    fn test_unstake_never_exceeds_own_balance(
        staked in 1_000_000i128..1_000_000_000i128,
        overshoot in 1i128..1_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let staker = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token_id = sac.address();
        let asset_client = token::StellarAssetClient::new(&env, &token_id);
        let token_client = token::TokenClient::new(&env, &token_id);

        let insurance_id = env.register(InsuranceContract, (admin.clone(), token_id.clone()));
        let ins = crate::InsuranceContractClient::new(&env, &insurance_id);

        asset_client.mint(&staker, &staked);
        token_client.approve(&staker, &insurance_id, &staked, &(env.ledger().sequence() + 1000));
        ins.stake(&staker, &staked);

        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ins.unstake(&staker, &(staked + overshoot))
        }));
        prop_assert!(attempt.is_err(), "unstake above own balance must be rejected");
        prop_assert_eq!(ins.get_stake(&staker), staked, "balance must be untouched by the rejected attempt");
    }

    /// pay_out must never move more than the pool holds, and the pro-rata
    /// reduction must keep sum(stakes) == pool_total afterward.
    #[test]
    fn test_payout_never_exceeds_pool_balance(
        stake_a in 1_000_000i128..500_000_000i128,
        stake_b in 1_000_000i128..500_000_000i128,
        requested_payout_extra in 1i128..1_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token_id = sac.address();
        let asset_client = token::StellarAssetClient::new(&env, &token_id);
        let token_client = token::TokenClient::new(&env, &token_id);

        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
        let insurance_id = env.register(InsuranceContract, (admin.clone(), token_id.clone()));
        let ins = crate::InsuranceContractClient::new(&env, &insurance_id);

        // Stand-in for the real repayment contract's address.
        let repayment_stub = Address::generate(&env);
        ins.set_payout_caller(&one(&env, &admin), &repayment_stub);
        ins.set_registry(&one(&env, &admin), &registry_id);
        reg.set_financing_contract(&one(&env, &admin), &repayment_stub);
        reg.set_repayment_contract(&one(&env, &admin), &repayment_stub);

        let staker_a = Address::generate(&env);
        let staker_b = Address::generate(&env);
        for (staker, amount) in [(&staker_a, stake_a), (&staker_b, stake_b)] {
            asset_client.mint(staker, &amount);
            token_client.approve(staker, &insurance_id, &amount, &(env.ledger().sequence() + 1000));
            ins.stake(staker, &amount);
        }
        let pool_before = ins.get_pool_total();

        // Drive an invoice to Defaulted via the real transition path.
        let invoice_id = symbol_short!("inv_pay");
        reg.register_invoice(&invoice_id, &Address::generate(&env), &10_000_000i128, &symbol_short!("USD"), &2_000_000u64);
        reg.financing_marks_invoice_financed(&invoice_id);
        env.ledger().set_timestamp(2_000_001);
        reg.mark_invoice_overdue(&invoice_id);
        reg.repayment_marks_defaulted(&invoice_id);

        let beneficiary = Address::generate(&env);
        let requested = pool_before + requested_payout_extra; // deliberately over the pool
        let paid = ins.pay_out(&invoice_id, &beneficiary, &requested);

        prop_assert_eq!(paid, pool_before, "payout must be capped at the pre-payout pool balance");
        prop_assert_eq!(ins.get_pool_total(), 0);
        prop_assert_eq!(ins.get_stake(&staker_a) + ins.get_stake(&staker_b), 0);
    }
}
