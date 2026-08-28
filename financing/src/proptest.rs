#![cfg(test)]
extern crate std;

use crate::test::create_token;
use crate::FinancingContract;
use invofi_registry::RegistryContract;
use proptest::prelude::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env,
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
    fn test_create_offer_invariants(
        principal in 10_000_000i128..100_000_000_000i128,
        interest_rate in 1u32..=10_000u32,
        duration in 86400u64..31536000u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);

        let admin = Address::generate(&env);
        let originator = Address::generate(&env);
        let lender = Address::generate(&env);
        let invoice_id = symbol_short!("inv_pt");
        let offer_id = symbol_short!("off_pt");
        let token_id = Address::generate(&env);

        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);

        let financing_id = env.register(
            FinancingContract,
            (admin.clone(), registry_id.clone(), token_id.clone()),
        );
        let fin = crate::FinancingContractClient::new(&env, &financing_id);

        reg.register_invoice(
            &invoice_id,
            &originator,
            &principal,
            &symbol_short!("USD"),
            &2_000_000u64,
        );

        let offer = fin.create_offer(
            &offer_id,
            &invoice_id,
            &lender,
            &principal,
            &symbol_short!("USD"),
            &interest_rate,
            &duration,
         0);

        assert_eq!(offer.interest_rate, interest_rate);
        assert_eq!(offer.amount, principal);
        assert_eq!(offer.duration, duration);

        let stats = fin.get_stats();
        assert_eq!(stats.total_offers, 1);

        let lender_stats = fin.get_lender_stats(&lender);
        assert_eq!(lender_stats.total_offered, principal);
        assert_eq!(lender_stats.offers_pending, 1);
    }

        /// Proves the fix above: amount must be > 0 and <= invoice.amount.
        #[test]
        fn test_offer_amount_bounds(
            invoice_amount in 10_000_000i128..100_000_000_000i128,
            overshoot in 1i128..1_000_000_000i128,
        ) {
            let env = Env::default();
            env.mock_all_auths();
            env.ledger().set_timestamp(1_000_000);

            let admin = Address::generate(&env);
            let originator = Address::generate(&env);
            let lender = Address::generate(&env);
            let invoice_id = symbol_short!("inv_bd");
            let token_id = Address::generate(&env);

            let registry_id = env.register(RegistryContract, (admin.clone(),));
            let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
            let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
            let fin = crate::FinancingContractClient::new(&env, &financing_id);

            reg.register_invoice(&invoice_id, &originator, &invoice_amount, &symbol_short!("USD"), &2_000_000u64);

            // Over the invoice amount must be rejected.
            let over = invoice_amount + overshoot;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fin.create_offer(&symbol_short!("off_over"), &invoice_id, &lender, &over, &symbol_short!("USD"), &500u32, &604_800u64)
            }));
            prop_assert!(result.is_err(), "offer above invoice amount must be rejected");

            // Exactly the invoice amount must succeed.
            let offer = fin.create_offer(&symbol_short!("off_ok"), &invoice_id, &lender, &invoice_amount, &symbol_short!("USD"), &500u32, &604_800u64);
            prop_assert_eq!(offer.amount, invoice_amount);
        }

        /// At most one accepted offer per invoice — enforced structurally by the
        /// registry's Pending-only gate on both create_offer and accept_offer.
        #[test]
        fn test_at_most_one_accepted_offer_per_invoice(
            principal in 10_000_000i128..100_000_000_000i128,
        ) {
            let env = Env::default();
            env.mock_all_auths();
            env.ledger().set_timestamp(1_000_000);

            let admin = Address::generate(&env);
            let originator = Address::generate(&env);
            let lender_a = Address::generate(&env);
            let lender_b = Address::generate(&env);
            let invoice_id = symbol_short!("inv_ex");
            let token_id = create_token(&env);

            let registry_id = env.register(RegistryContract, (admin.clone(),));
            let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
            let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
            let fin = crate::FinancingContractClient::new(&env, &financing_id);
            // accept_offer triggers a Pending -> Financed transition through the
            // registry; financing must be registered as that trusted caller.
            reg.set_financing_contract(&one(&env, &admin), &financing_id);

            reg.register_invoice(&invoice_id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);

            let half = principal / 2;
            fin.create_offer(&symbol_short!("off_a"), &invoice_id, &lender_a, &half, &symbol_short!("USD"), &500u32, &604_800u64);
            fin.create_offer(&symbol_short!("off_b"), &invoice_id, &lender_b, &half, &symbol_short!("USD"), &500u32, &604_800u64);

            // Fund lender_a and accept their offer.
            let asset_client = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
            let token_client = soroban_sdk::token::TokenClient::new(&env, &token_id);
            asset_client.mint(&lender_a, &half);
            token_client.approve(&lender_a, &financing_id, &half, &(env.ledger().sequence() + 1000));
            fin.accept_offer(&symbol_short!("off_a"), &originator);

            // lender_b's offer can never be accepted now — invoice is Financed.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fin.accept_offer(&symbol_short!("off_b"), &originator)
            }));
            prop_assert!(result.is_err(), "second accept on a Financed invoice must be rejected");
        }

        /// Position tokens minted == total_financed by construction (single mint
        /// site in settle_acceptance) — this guards that invariant across
        /// multiple invoices/offers.
        #[test]
        fn test_position_tokens_equal_total_financed(
            amounts in prop::collection::vec(10_000_000i128..10_000_000_000i128, 1..5),
        ) {
            let env = Env::default();
            env.mock_all_auths();
            env.ledger().set_timestamp(1_000_000);

            let admin = Address::generate(&env);
            let originator = Address::generate(&env);
            let lender = Address::generate(&env);
            let token_id = create_token(&env);

            let registry_id = env.register(RegistryContract, (admin.clone(),));
            let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
            let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
            let fin = crate::FinancingContractClient::new(&env, &financing_id);

            // The position token's admin is the financing contract itself
            // (ADR-0002) so that accept_offer can mint claim tokens mid-CPI.
            let pos_sac = env.register_stellar_asset_contract_v2(financing_id.clone());
            let pos_token_id = pos_sac.address();
            // accept_offer triggers a Pending -> Financed transition through the
            // registry; financing must be registered as that trusted caller.
            reg.set_financing_contract(&one(&env, &admin), &financing_id);
            fin.set_position_token(&one(&env, &admin), &pos_token_id);

            let asset_client = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
            let token_client = soroban_sdk::token::TokenClient::new(&env, &token_id);
            let pos_client = soroban_sdk::token::TokenClient::new(&env, &pos_token_id);

            let mut expected_total: i128 = 0;
            for (i, amount) in amounts.iter().enumerate() {
                let inv_id = soroban_sdk::Symbol::new(&env, &std::format!("inv{}", i));
                let off_id = soroban_sdk::Symbol::new(&env, &std::format!("off{}", i));
                reg.register_invoice(&inv_id, &originator, amount, &symbol_short!("USD"), &2_000_000u64);
                fin.create_offer(&off_id, &inv_id, &lender, amount, &symbol_short!("USD"), &500u32, &604_800u64);
                asset_client.mint(&lender, amount);
                token_client.approve(&lender, &financing_id, amount, &(env.ledger().sequence() + 1000));
                fin.accept_offer(&off_id, &originator);
                expected_total += amount;
            }

            prop_assert_eq!(fin.get_stats().total_financed, expected_total);
            prop_assert_eq!(pos_client.balance(&lender), expected_total,
                "position tokens minted must equal lender's total accepted amount \
                 (they are never burned on repayment — see PR notes)");
        }

        /// Withdrawn/rejected offers must never mint a position token or count
        /// toward total_financed.
        #[test]
        fn test_withdraw_and_reject_do_not_mint(
            principal in 10_000_000i128..100_000_000_000i128,
        ) {
            let env = Env::default();
            env.mock_all_auths();
            env.ledger().set_timestamp(1_000_000);

            let admin = Address::generate(&env);
            let originator = Address::generate(&env);
            let lender = Address::generate(&env);
            let token_id = create_token(&env);
            let pos_token_admin = Address::generate(&env);
            let pos_sac = env.register_stellar_asset_contract_v2(pos_token_admin);
            let pos_token_id = pos_sac.address();

            let registry_id = env.register(RegistryContract, (admin.clone(),));
            let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
            let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
            let fin = crate::FinancingContractClient::new(&env, &financing_id);
            fin.set_position_token(&one(&env, &admin), &pos_token_id);
            let pos_client = soroban_sdk::token::TokenClient::new(&env, &pos_token_id);

            let invoice_id = symbol_short!("inv_wd");
            reg.register_invoice(&invoice_id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);

            let offer_id = symbol_short!("off_wd");
            fin.create_offer(&offer_id, &invoice_id, &lender, &principal, &symbol_short!("USD"), &500u32, &604_800u64);
            fin.withdraw_offer(&offer_id, &lender);

            prop_assert_eq!(pos_client.balance(&lender), 0);
            prop_assert_eq!(fin.get_stats().total_financed, 0);
        }

    /// The interest-rate cap (`MAX_INTEREST_BPS`) must be enforced on every
    /// term-setting entrypoint: `create_offer`, `amend_offer`, and
    /// `counter_offer`.  Random valid rates are accepted; any rate one above
    /// the cap is rejected on all three paths.
    #[test]
    fn test_interest_rate_cap_enforced_on_all_paths(
        valid_rate in 1u32..=10_000u32,
        principal in 10_000_000i128..100_000_000_000i128,
        duration in 86_400u64..31_536_000u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);

        let admin = Address::generate(&env);
        let originator = Address::generate(&env);
        let lender = Address::generate(&env);
        let token_id = Address::generate(&env);

        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
        let financing_id = env.register(FinancingContract, (admin.clone(), registry_id.clone(), token_id.clone()));
        let fin = crate::FinancingContractClient::new(&env, &financing_id);

        // Register invoice (amount must be >= principal for the offer to be valid)
        let invoice_amount = principal.max(10_000_000);
        let invoice_id = symbol_short!("inv_rc");
        reg.register_invoice(&invoice_id, &originator, &invoice_amount, &symbol_short!("USD"), &2_000_000u64);

        // ── 1. create_offer with valid rate must succeed ────────────────────
        let offer = fin.create_offer(
            &symbol_short!("off_rc"),
            &invoice_id,
            &lender,
            &principal,
            &symbol_short!("USD"),
            &valid_rate,
            &duration,
        );
        prop_assert_eq!(offer.interest_rate, valid_rate);

        // ── 2. create_offer with rate = MAX + 1 must be rejected ───────────
        let over_rate = invofi_common::MAX_INTEREST_BPS + 1;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fin.create_offer(
                &symbol_short!("off_over"),
                &invoice_id,
                &lender,
                &principal,
                &symbol_short!("USD"),
                &over_rate,
                &duration,
            );
        }));
        prop_assert!(result.is_err(), "create_offer: rate {} above cap must be rejected", over_rate);

        // ── 3. amend_offer with rate = MAX + 1 must be rejected ─────────────
        let offer_id = symbol_short!("off_amd");
        fin.create_offer(&offer_id, &invoice_id, &lender, &principal, &symbol_short!("USD"), &valid_rate, &duration);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fin.amend_offer(&offer_id, &lender, &0u32, &principal, &over_rate, &duration);
        }));
        prop_assert!(result.is_err(), "amend_offer: rate {} above cap must be rejected", over_rate);

        // ── 4. counter_offer with rate = MAX + 1 must be rejected ───────────
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fin.counter_offer(&offer_id, &originator, &0u32, &principal, &over_rate, &duration);
        }));
        prop_assert!(result.is_err(), "counter_offer: rate {} above cap must be rejected", over_rate);

        // ── 5. All stored offers must have rate <= MAX_INTEREST_BPS ──────────
        let all_offers = fin.get_all_offers();
        for stored_offer in all_offers.iter() {
            prop_assert!(
                stored_offer.interest_rate <= invofi_common::MAX_INTEREST_BPS,
                "stored offer rate {} exceeds MAX_INTEREST_BPS {}",
                stored_offer.interest_rate,
                invofi_common::MAX_INTEREST_BPS,
            );
        }
    }
}
