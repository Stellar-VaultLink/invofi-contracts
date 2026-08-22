#![cfg(test)]
extern crate std;

use crate::test::{deploy_protocol, mint_and_approve};
use invofi_common::InvoiceStatus;
use proptest::prelude::*;
use soroban_sdk::{symbol_short, testutils::Ledger as _, Env};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// End-to-end cross-crate invariant: a full, on-time repayment must
    /// leave the invoice Repaid, and must move reputation's score for the
    /// originator in the non-decreasing direction proven in
    /// reputation/src/proptest.rs — now checked across real contract
    /// boundaries instead of in isolation.
    #[test]
    fn test_full_repayment_reflects_in_reputation(
        principal in 10_000_000i128..100_000_000_000i128,
        interest_rate in 1u32..=10_000u32,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let funded_at = 1_000_000u64;
        env.ledger().set_timestamp(funded_at);

        let p = deploy_protocol(&env);
        let score_before = p.repu.get_score(&p.originator);

        let invoice_id = symbol_short!("inv_e2e");
        let offer_id = symbol_short!("off_e2e");
        p.reg.register_invoice(&invoice_id, &p.originator, &principal, &symbol_short!("USD"), &2_000_000u64);
        p.fin.create_offer(&offer_id, &invoice_id, &p.lender, &principal, &symbol_short!("USD"), &interest_rate, &2_592_000u64);
        mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, principal);
        p.fin.accept_offer(&offer_id, &p.originator);

        let total_owed = p.rep.calculate_total_due(&offer_id);
        mint_and_approve(&env, &p.token_id, &p.repayment_id, &p.originator, total_owed);
        p.rep.repay_invoice(&invoice_id, &offer_id, &p.originator, &total_owed);

        prop_assert_eq!(p.reg.get_invoice(&invoice_id).status, InvoiceStatus::Repaid);
        prop_assert!(p.repu.get_score(&p.originator) >= score_before,
            "full repayment must never decrease the originator's reputation score");
    }
}
