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
        env.ledger().set_timestamp(1_000_000);

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

        fin.set_repayment_contract(&admin, &repayment_id);
        reg.set_repayment_contract(&admin, &repayment_id);
        reg.set_financing_contract(&admin, &financing_id);

        // Set fee
        reg.set_fee(&admin, &fee_bps);

        // Register invoice
        reg.register_invoice(
            &invoice_id,
            &originator,
            &principal,
            &symbol_short!("USD"),
            &2_000_000u64,
        );

        // Calculate expected yield
        let expected_yield = principal * (interest_rate as i128) / 10_000;
        let total_due = principal + expected_yield;

        // Register offer
        fin.create_offer(
            &offer_id,
            &invoice_id,
            &lender,
            &principal,
            &symbol_short!("USD"),
            &interest_rate,
            &2_592_000u64,
        );

        // Mint and approve for lender
        asset_client.mint(&lender, &principal);
        token_client.approve(&lender, &financing_id, &principal, &(env.ledger().sequence() + 1000));
        
        // Accept offer
        fin.accept_offer(&offer_id, &originator, &0);

        // Now repayer (originator) prepares to repay
        let partial_repay_amount = total_due / partial_ratio;
        
        // Ensure partial repay > 0
        let partial_repay_amount = if partial_repay_amount == 0 { 1 } else { partial_repay_amount };
        let full_remaining = total_due - partial_repay_amount;

        // Mint enough token to originator to repay
        asset_client.mint(&originator, &total_due);
        token_client.approve(&originator, &repayment_id, &total_due, &(env.ledger().sequence() + 1000));

        let initial_admin_bal = token_client.balance(&admin);
        let initial_lender_bal = token_client.balance(&lender);
        let initial_orig_bal = token_client.balance(&originator);

        // Perform partial repayment. Version is 1 after accept_offer.
        rep.repay_invoice(&invoice_id, &offer_id, &originator, &partial_repay_amount, &1);

        // Verify partial repayment math
        let actual_fee_bps = fin.get_fee_bps();
        let fee_amount_1 = partial_repay_amount * (actual_fee_bps as i128) / 10_000;
        let lender_amount_1 = partial_repay_amount - fee_amount_1;

        assert_eq!(token_client.balance(&admin), initial_admin_bal + fee_amount_1);
        assert_eq!(token_client.balance(&lender), initial_lender_bal + lender_amount_1);
        assert_eq!(token_client.balance(&originator), initial_orig_bal - partial_repay_amount);

        // Verify state invariants
        let offer = fin.get_offer(&offer_id);
        assert!(offer.amount_repaid <= total_due);
        assert_eq!(offer.amount_repaid, partial_repay_amount);

        let invoice = reg.get_invoice(&invoice_id);
        if full_remaining > 0 {
            assert_eq!(invoice.status, InvoiceStatus::Financed);
            assert_eq!(offer.status, OfferStatus::Financed);
        } else {
            assert_eq!(invoice.status, InvoiceStatus::Repaid);
            assert_eq!(offer.status, OfferStatus::Repaid);
        }

        // Now perform the rest of the repayment
        if full_remaining > 0 {
            // First repay bumped version to 2.
            rep.repay_invoice(&invoice_id, &offer_id, &originator, &full_remaining, &2);
            let fee_amount_2 = full_remaining * (actual_fee_bps as i128) / 10_000;
            let lender_amount_2 = full_remaining - fee_amount_2;

            assert_eq!(token_client.balance(&admin), initial_admin_bal + fee_amount_1 + fee_amount_2);
            assert_eq!(token_client.balance(&lender), initial_lender_bal + lender_amount_1 + lender_amount_2);
            assert_eq!(token_client.balance(&originator), initial_orig_bal - partial_repay_amount - full_remaining);

            let offer_final = fin.get_offer(&offer_id);
            assert_eq!(offer_final.amount_repaid, total_due);
            assert_eq!(offer_final.status, OfferStatus::Repaid);
            
            let invoice_final = reg.get_invoice(&invoice_id);
            assert_eq!(invoice_final.status, InvoiceStatus::Repaid);
        }

        // Assert stats
        let stats = fin.get_stats();
        let total_fee = fee_amount_1 + if full_remaining > 0 { full_remaining * (actual_fee_bps as i128) / 10_000 } else { 0 };
        assert_eq!(stats.total_repaid, partial_repay_amount + full_remaining);
        assert_eq!(stats.total_fee_revenue, total_fee);
    }
}
