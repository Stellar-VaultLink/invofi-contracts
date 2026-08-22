#![cfg(test)]
extern crate std;

use crate::RegistryContract;
use invofi_common::InvoiceStatus;
use proptest::prelude::*;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

/// Deploys a registry and authorizes stand-in financing/repayment addresses
/// (mirrors production wiring without pulling in the financing/repayment
/// crates, which registry does not depend on).
fn setup(
    env: &Env,
) -> (
    crate::RegistryContractClient<'static>,
    Address,
    Address,
    Address,
) {
    let admin = Address::generate(env);
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = crate::RegistryContractClient::new(env, &registry_id);
    let financing = Address::generate(env);
    let repayment = Address::generate(env);
    reg.set_financing_contract(&admin, &financing);
    reg.set_repayment_contract(&admin, &repayment);
    (reg, admin, financing, repayment)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// update_invoice_amount is Pending-gated in the real contract — this is
    /// the actual invariant (not "never modifiable", which is false: the
    /// originator *can* correct an amount while Pending).
    #[test]
    fn test_invoice_amount_immutable_once_financed(
        principal in 10_000_000i128..100_000_000_000i128,
        new_amount in 10_000_000i128..100_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let (reg, admin, financing, _repayment) = setup(&env);
        let originator = Address::generate(&env);
        let id = symbol_short!("inv_pt");

        reg.register_invoice(&id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);
        // Amount edit while Pending must succeed.
        let updated = reg.update_invoice_amount(&id, &originator, &new_amount);
        prop_assert_eq!(updated.amount, new_amount);

        // financing_marks_invoice_financed is called by the authorized
        // financing address only — mock_all_auths lets us stand in for it.
        let _ = admin; // admin unused past setup; keep for clarity of wiring
        let _financing_invoker = financing;
        reg.financing_marks_invoice_financed(&id);

        // Now amount edits must be rejected — status is no longer Pending.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reg.update_invoice_amount(&id, &originator, &(new_amount + 1))
        }));
        prop_assert!(result.is_err(), "amount must be frozen once Financed");

        let invoice = reg.get_invoice(&id);
        prop_assert_eq!(invoice.amount, new_amount, "amount must equal last Pending-state value");
        prop_assert_eq!(invoice.status, InvoiceStatus::Financed);
    }

    /// due_date has no setter anywhere in the registry — this locks that in
    /// as a regression guard so a future PR can't quietly add one without a
    /// failing test forcing a conscious decision.
    #[test]
    fn test_due_date_never_changes(
        principal in 10_000_000i128..100_000_000_000i128,
        due_offset in 86_400u64..31_536_000u64,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let (reg, _admin, _financing, _repayment) = setup(&env);
        let originator = Address::generate(&env);
        let id = symbol_short!("inv_dd");
        let due_date = 1_000_000u64 + due_offset;

        let original = reg.register_invoice(&id, &originator, &principal, &symbol_short!("USD"), &due_date);
        prop_assert_eq!(original.due_date, due_date);

        // Warp past due_date and mark overdue — a state-mutating call that
        // touches the same struct.
        env.ledger().set_timestamp(due_date + 1);
        reg.mark_invoice_overdue(&id);

        let after = reg.get_invoice(&id);
        prop_assert_eq!(after.due_date, due_date, "due_date must never change post-registration");
    }

    /// Cancelled is a terminal state — no transition out of it exists in
    /// common::validate_transition's table.
    #[test]
    fn test_cancelled_invoice_never_reactivated(
        principal in 10_000_000i128..100_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let (reg, _admin, financing, _repayment) = setup(&env);
        let originator = Address::generate(&env);
        let id = symbol_short!("inv_cx");

        reg.register_invoice(&id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);
        reg.cancel_invoice(&id, &originator);
        prop_assert_eq!(reg.get_invoice(&id).status, InvoiceStatus::Cancelled);

        let _ = financing;
        let attempt_financed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reg.financing_marks_invoice_financed(&id)
        }));
        prop_assert!(attempt_financed.is_err(), "Cancelled -> Financed must be rejected");

        let attempt_status = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reg.update_invoice_status(&id, &originator, &InvoiceStatus::Pending)
        }));
        prop_assert!(attempt_status.is_err(), "Cancelled -> Pending must be rejected");

        prop_assert_eq!(reg.get_invoice(&id).status, InvoiceStatus::Cancelled);
    }

    /// Storage count must track registrations exactly across a random batch.
    #[test]
    fn test_invoice_count_matches_storage(
        amounts in prop::collection::vec(10_000_000i128..100_000_000_000i128, 1..20),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let (reg, _admin, _financing, _repayment) = setup(&env);
        let originator = Address::generate(&env);

        for (i, amount) in amounts.iter().enumerate() {
            let id = symbol_for(&env, i as u32);
            reg.register_invoice(&id, &originator, amount, &symbol_short!("USD"), &2_000_000u64);
        }
        prop_assert_eq!(reg.get_invoices_count(), amounts.len() as u32);
    }

    /// Pending -> Financed -> Repaid must move strictly forward; no method
    /// exists to re-enter Financed from Repaid.
    #[test]
    fn test_forward_only_lifecycle(
        principal in 10_000_000i128..100_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000_000);
        let (reg, _admin, _financing, _repayment) = setup(&env);
        let originator = Address::generate(&env);
        let id = symbol_short!("inv_fw");

        reg.register_invoice(&id, &originator, &principal, &symbol_short!("USD"), &2_000_000u64);
        prop_assert_eq!(reg.get_invoice(&id).status, InvoiceStatus::Pending);

        reg.financing_marks_invoice_financed(&id);
        prop_assert_eq!(reg.get_invoice(&id).status, InvoiceStatus::Financed);

        reg.repayment_marks_invoice_repaid(&id, &true);
        prop_assert_eq!(reg.get_invoice(&id).status, InvoiceStatus::Repaid);

        // No path back to Financed or Pending exists.
        let regress = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reg.financing_marks_invoice_financed(&id)
        }));
        prop_assert!(regress.is_err(), "Repaid must be terminal w.r.t. financing");
    }
}

/// Small helper so 0..20 batch-registered invoices get distinct short symbols.
fn symbol_for(env: &Env, i: u32) -> soroban_sdk::Symbol {
    soroban_sdk::Symbol::new(env, &std::format!("inv{:04}", i))
}
