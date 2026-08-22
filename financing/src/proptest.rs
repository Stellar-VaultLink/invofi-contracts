#![cfg(test)]
extern crate std;

use crate::FinancingContract;
use invofi_registry::RegistryContract;
use proptest::prelude::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

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
        );

        assert_eq!(offer.interest_rate, interest_rate);
        assert_eq!(offer.amount, principal);
        assert_eq!(offer.duration, duration);

        let stats = fin.get_stats();
        assert_eq!(stats.total_offers, 1);

        let lender_stats = fin.get_lender_stats(&lender);
        assert_eq!(lender_stats.total_offered, principal);
        assert_eq!(lender_stats.offers_pending, 1);
    }
}
