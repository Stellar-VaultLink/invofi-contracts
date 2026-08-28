#![cfg(test)]
extern crate std;

use crate::RepaymentContract;
use invofi_common::{InvoiceStatus, OfferStatus};
use invofi_financing::FinancingContract;
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

    #[test]
    fn test_repay_invoice_math_invariants(
        principal in 10_000_000i128..100_000_000_000i128,
        interest_rate in 1u32..=10_000u32,
        fee_bps in 0u32..500u32,
        partial_ratio in 1i128..100i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();

        // Use a funded_at close to the payment time so pro-rata interest
        // is predictable. We advance exactly 365 days, so pro-rata interest
        // equals principal * rate_bps * 365 / 3_650_000.
        let funded_at: u64 = 1_000_000;
        env.ledger().set_timestamp(funded_at);

        let admin = Address::generate(&env);
        let originator = Address::generate(&env);
        let lender = Address::generate(&env);
        let invoice_id = symbol_short!("inv_pt");
        let offer_id = symbol_short!("off_pt");

        // Set up token
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin.clone());
        let token_id = sac.address();

        let asset_client = token::StellarAssetClient::new(&env, &token_id);
        let token_client = token::TokenClient::new(&env, &token_id);

        // Deploy contracts
        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);

        let financing_id = env.register(
            FinancingContract,
            (admin.clone(), registry_id.clone(), token_id.clone()),
        );
        let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);

        let repayment_id = env.register(
            RepaymentContract,
            (
                admin.clone(),
                registry_id.clone(),
                financing_id.clone(),
                token_id.clone(),
            ),
        );
        let rep = crate::RepaymentContractClient::new(&env, &repayment_id);

        fin.set_repayment_contract(&one(&env, &admin), &repayment_id);
        reg.set_repayment_contract(&one(&env, &admin), &repayment_id);
        reg.set_financing_contract(&one(&env, &admin), &financing_id);

        // Set fee
        reg.set_fee(&one(&env, &admin), &fee_bps);

        // Register invoice
        reg.register_invoice(
            &invoice_id,
            &originator,
            &principal,
            &symbol_short!("USD"),
            &2_000_000u64,
        );

        // Register offer
        fin.create_offer(
            &offer_id,
            &invoice_id,
            &lender,
            &principal,
            &symbol_short!("USD"),
            &interest_rate,
            &2_592_000u64,
            &0u64,
        );

        // Mint and approve for lender
        asset_client.mint(&lender, &principal);
        token_client.approve(&lender, &financing_id, &principal, &(env.ledger().sequence() + 1000));

        // Accept offer (funded_at = 1_000_000)
        fin.accept_offer(&offer_id, &originator);

        // Advance 365 days so pro-rata interest is predictable.
        // accrued = principal * rate_bps * 365 / 3_650_000
        env.ledger().set_timestamp(funded_at + 365 * 86_400);
        let accrued_interest = principal * (interest_rate as i128) * 365 / 3_650_000;
        let total_owed = principal + accrued_interest;

        // Calculate partial payment amount (must be >= 1% of principal for partials)
        let min_payment = principal / 100;
        let partial_repay_amount = (total_owed / partial_ratio).max(min_payment);
        // Ensure partial doesn't exceed total_owed
        let partial_repay_amount = partial_repay_amount.min(total_owed - 1).max(min_payment);
        let full_remaining = total_owed - partial_repay_amount;

        // Mint enough tokens to originator
        asset_client.mint(&originator, &total_owed);
        token_client.approve(&originator, &repayment_id, &total_owed, &(env.ledger().sequence() + 1000));

        let initial_admin_bal = token_client.balance(&admin);
        let initial_lender_bal = token_client.balance(&lender);
        let initial_orig_bal = token_client.balance(&originator);

        // Perform partial repayment
        rep.repay_invoice(&invoice_id, &offer_id, &originator, &partial_repay_amount);

        // Verify partial repayment math
        let actual_fee_bps = fin.get_fee_bps();
        let fee_amount_1 = partial_repay_amount * (actual_fee_bps as i128) / 10_000;
        let lender_amount_1 = partial_repay_amount - fee_amount_1;

        assert_eq!(token_client.balance(&admin), initial_admin_bal + fee_amount_1);
        assert_eq!(token_client.balance(&lender), initial_lender_bal + lender_amount_1);
        assert_eq!(token_client.balance(&originator), initial_orig_bal - partial_repay_amount);

        // Verify state invariants
        let offer = fin.get_offer(&offer_id);
        assert_eq!(offer.amount_repaid, partial_repay_amount);

        let invoice = reg.get_invoice(&invoice_id);
        if full_remaining > 0 {
            assert_eq!(invoice.status, InvoiceStatus::Financed);
            assert_eq!(offer.status, OfferStatus::Financed);
        } else {
            assert_eq!(invoice.status, InvoiceStatus::Repaid);
            assert_eq!(offer.status, OfferStatus::Repaid);
        }

        // Now perform the rest of the repayment.
        // After the first payment, pro-rata interest is recalculated on the
        // new remaining principal, so we query the contract for the actual
        // remaining balance instead of using the pre-calculated full_remaining.
        if full_remaining > 0 {
            let actual_remaining = rep.calculate_total_due(&offer_id);
            // Mint enough for the actual remaining balance
            asset_client.mint(&originator, &actual_remaining);
            token_client.approve(&originator, &repayment_id, &actual_remaining, &(env.ledger().sequence() + 1000));

            rep.repay_invoice(&invoice_id, &offer_id, &originator, &actual_remaining);
            let fee_amount_2 = actual_remaining * (actual_fee_bps as i128) / 10_000;
            let lender_amount_2 = actual_remaining - fee_amount_2;

            assert_eq!(token_client.balance(&admin), initial_admin_bal + fee_amount_1 + fee_amount_2);
            assert_eq!(token_client.balance(&lender), initial_lender_bal + lender_amount_1 + lender_amount_2);

            let offer_final = fin.get_offer(&offer_id);
            assert_eq!(offer_final.status, OfferStatus::Repaid);

            let invoice_final = reg.get_invoice(&invoice_id);
            assert_eq!(invoice_final.status, InvoiceStatus::Repaid);

            // Verify payment history has 2 records
            let history = rep.get_payment_history(&invoice_id);
            assert_eq!(history.len(), 2);

            // Verify remaining principal is 0
            assert_eq!(rep.get_remaining_principal(&offer_id), 0);
        }

        // Assert stats
        let stats = fin.get_stats();
        let total_repaid = stats.total_repaid;
        assert!(total_repaid >= partial_repay_amount);
    }

        /// repay_invoice already rejects amount > total_owed on-chain (line
    /// ~433 of repayment/src/lib.rs) — this proves that boundary holds under
    /// fuzzing rather than just at the one hand-picked value in unit tests.
    #[test]
    fn test_repay_never_exceeds_total_owed(
        principal in 10_000_000i128..100_000_000_000i128,
        interest_rate in 1u32..=10_000u32,
        days_elapsed in 1u64..365u64,
        overshoot in 1i128..1_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let funded_at: u64 = 1_000_000;
        env.ledger().set_timestamp(funded_at);

        let admin = Address::generate(&env);
        let originator = Address::generate(&env);
        let lender = Address::generate(&env);
        let invoice_id = symbol_short!("inv_ex");
        let offer_id = symbol_short!("off_ex");
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token_id = sac.address();
        let asset_client = token::StellarAssetClient::new(&env, &token_id);
        let token_client = token::TokenClient::new(&env, &token_id);

        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
        let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
        let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
        let repayment_id = env.register(RepaymentContract, (admin.clone(), registry_id.clone(), financing_id.clone(), token_id.clone()));
        let rep = crate::RepaymentContractClient::new(&env, &repayment_id);
        fin.set_repayment_contract(&one(&env, &admin), &repayment_id);
        reg.set_repayment_contract(&one(&env, &admin), &repayment_id);
        reg.set_financing_contract(&one(&env, &admin), &financing_id);

        reg.register_invoice(&invoice_id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);
        fin.create_offer(&offer_id, &invoice_id, &lender, &principal, &symbol_short!("USD"), &interest_rate, &2_592_000u64);
        asset_client.mint(&lender, &principal);
        token_client.approve(&lender, &financing_id, &principal, &(env.ledger().sequence() + 1000));
        fin.accept_offer(&offer_id, &originator);

        env.ledger().set_timestamp(funded_at + days_elapsed * 86_400);
        let total_owed = rep.calculate_total_due(&offer_id);

        asset_client.mint(&originator, &(total_owed + overshoot));
        token_client.approve(&originator, &repayment_id, &(total_owed + overshoot), &(env.ledger().sequence() + 1000));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rep.repay_invoice(&invoice_id, &offer_id, &originator, &(total_owed + overshoot))
        }));
        prop_assert!(result.is_err(), "repayment above total_owed must be rejected");
    }

    /// Interest accrues linearly with elapsed time and must never decrease
    /// for a fixed remaining principal — pro_rata_interest is a pure
    /// function of (remaining, rate, days), monotonic in days by formula.
    #[test]
    fn test_interest_monotonic_with_time(
        principal in 10_000_000i128..100_000_000_000i128,
        interest_rate in 1u32..=10_000u32,
        t1_days in 1u64..180u64,
        t2_extra_days in 1u64..180u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let funded_at: u64 = 1_000_000;
        env.ledger().set_timestamp(funded_at);

        let admin = Address::generate(&env);
        let originator = Address::generate(&env);
        let lender = Address::generate(&env);
        let invoice_id = symbol_short!("inv_mt");
        let offer_id = symbol_short!("off_mt");
        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token_id = sac.address();
        let asset_client = token::StellarAssetClient::new(&env, &token_id);
        let token_client = token::TokenClient::new(&env, &token_id);

        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
        let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
        let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
        let repayment_id = env.register(RepaymentContract, (admin.clone(), registry_id.clone(), financing_id.clone(), token_id.clone()));
        let rep = crate::RepaymentContractClient::new(&env, &repayment_id);
        fin.set_repayment_contract(&one(&env, &admin), &repayment_id);
        reg.set_repayment_contract(&one(&env, &admin), &repayment_id);
        reg.set_financing_contract(&one(&env, &admin), &financing_id);

        reg.register_invoice(&invoice_id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);
        fin.create_offer(&offer_id, &invoice_id, &lender, &principal, &symbol_short!("USD"), &interest_rate, &31_536_000u64);
        asset_client.mint(&lender, &principal);
        token_client.approve(&lender, &financing_id, &principal, &(env.ledger().sequence() + 1000));
        fin.accept_offer(&offer_id, &originator);

        env.ledger().set_timestamp(funded_at + t1_days * 86_400);
        let interest_t1 = rep.calculate_accrued_interest(&offer_id);

        env.ledger().set_timestamp(funded_at + (t1_days + t2_extra_days) * 86_400);
        let interest_t2 = rep.calculate_accrued_interest(&offer_id);

        prop_assert!(interest_t2 >= interest_t1, "accrued interest must never decrease as time passes with no repayment");
    }
}
