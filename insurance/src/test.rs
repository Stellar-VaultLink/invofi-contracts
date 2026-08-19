
#![cfg(test)]
extern crate std;

use super::InsuranceContract;
use soroban_sdk::{symbol_short, testutils::{Address as _, Ledger}, token, Address, Env};

/// Deploy the insurance contract + a staking token, and initialize.
fn setup<'a>(
    env: &'a Env,
    admin: &Address,
) -> (Address, Address, super::InsuranceContractClient<'a>) {
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token_id = sac.address();

    let insurance_id = env.register(InsuranceContract, (admin.clone(), token_id.clone()));
    let client = super::InsuranceContractClient::new(env, &insurance_id);

    (token_id, insurance_id, client)
}

/// Mint `amount` of the staking token to `who` and approve the insurance
/// contract as spender (the same flow a real staker runs on-chain).
fn mint_and_approve(
    env: &Env,
    token_id: &Address,
    insurance_id: &Address,
    who: &Address,
    amount: i128,
) {
    let asset = token::StellarAssetClient::new(env, token_id);
    asset.mint(who, &amount);
    let t = token::TokenClient::new(env, token_id);
    t.approve(
        who,
        insurance_id,
        &amount,
        &(env.ledger().sequence() + 1000),
    );
}

// ─── Accounting tests ────────────────────────────────────────────────────────

#[test]
fn test_stake_multiple_stakers_accounting() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker_a = Address::generate(&env);
    let staker_b = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker_a, 1_000_000);
    mint_and_approve(&env, &token_id, &insurance_id, &staker_b, 2_500_000);

    client.stake(&staker_a, &1_000_000);
    client.stake(&staker_b, &2_500_000);

    // DoD: per-staker balances and the pool total are all correct.
    assert_eq!(client.get_stake(&staker_a), 1_000_000);
    assert_eq!(client.get_stake(&staker_b), 2_500_000);
    assert_eq!(client.get_pool_total(), 3_500_000);
    assert_eq!(client.get_stakers_count(), 2);

    // The actual token balance held by the contract matches the accounting.
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&insurance_id), 3_500_000);
    assert_eq!(client.get_contract_token_balance(), 3_500_000);

    // Both stakers emptied their wallets into the pool.
    assert_eq!(token_client.balance(&staker_a), 0);
    assert_eq!(token_client.balance(&staker_b), 0);
}

#[test]
fn test_unstake_partial_then_full() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    // Partial unstake: 600_000 of 1_000_000 remains staked.
    client.unstake(&staker, &400_000);
    assert_eq!(client.get_stake(&staker), 600_000);
    assert_eq!(client.get_pool_total(), 600_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 400_000);
    assert_eq!(client.get_contract_token_balance(), 600_000);

    // Full unstake: balance is zeroed and the staker drops out of the ledger.
    client.unstake(&staker, &600_000);
    assert_eq!(client.get_stake(&staker), 0);
    assert_eq!(client.get_pool_total(), 0);
    assert_eq!(client.get_stakers_count(), 0);
    assert_eq!(token_client.balance(&staker), 1_000_000);
    assert_eq!(client.get_contract_token_balance(), 0);
}

#[test]
fn test_stake_increases_existing_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &400_000);
    client.stake(&staker, &600_000);

    assert_eq!(client.get_stake(&staker), 1_000_000);
    assert_eq!(client.get_pool_total(), 1_000_000);
    assert_eq!(client.get_stakers_count(), 1);
}

// ─── Failure paths ───────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_unstake_exceeds_stake_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &500_000);

    client.unstake(&staker, &600_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_unstake_without_stake_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);

    let stranger = Address::generate(&env);
    client.unstake(&stranger, &1_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_stake_zero_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    client.stake(&staker, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_unstake_zero_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    client.unstake(&staker, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_paused_blocks_stake() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);

    client.pause(&admin);
    assert!(client.contract_is_paused());
    client.stake(&staker, &1_000_000);
}

#[test]
fn test_pause_blocks_all_insurance_state_changes() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);
    let staker = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let new_token = Address::generate(&env);

    client.pause(&admin);
    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(result.is_err(), "state-changing function should panic while paused");
    }

    assert_paused(|| {
        client.stake(&staker, &1_000i128);
    });
    assert_paused(|| {
        client.unstake(&staker, &1_000i128);
    });
    assert_paused(|| {
        client.pay_out(&soroban_sdk::symbol_short!("inv_x"), &beneficiary, &1_000i128);
    });
    assert_paused(|| {
        client.transfer_admin(&admin, &new_admin);
    });
    assert_paused(|| {
        client.set_staking_token(&admin, &new_token);
    });
    assert_paused(|| {
        client.set_payout_caller(&admin, &payout_caller);
    });

    assert_eq!(client.get_pool_total(), 0);
    assert_eq!(client.get_stakers_count(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_paused_blocks_unstake() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    client.pause(&admin);
    client.unstake(&staker, &100_000);
}

#[test]
fn test_constructor_binds_admin_and_staking_token() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    // Admin and staking token are bound atomically at deploy — there is no
    // separate initialize() call a third party could front-run (issue #75).
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_staking_token(), token_id);
}

#[test]
fn test_set_staking_token_admin_only() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);

    let new_token = Address::generate(&env);
    client.set_staking_token(&admin, &new_token);
    assert_eq!(client.get_staking_token(), new_token);
}

// ─── Payout tests (Task 10) ─────────────────────────────────────────────────
//
// Every payout test wires a real registry contract and forces the invoice into
// Defaulted status before calling pay_out. This proves the on-chain Defaulted
// check cannot be bypassed — pay_out cross-reads the registry itself.

use invofi_registry::RegistryContract;

/// Register a registry contract and return its address and a minimal
/// Defaulted invoice id ready for use in payout tests.
///
/// The registry is wired to the insurance client via `set_registry`.
/// The invoice is driven to `Defaulted` via `repayment_marks_defaulted`,
/// which requires the registry's repayment contract to be configured — we
/// set it to an arbitrary address and use `mock_all_auths` to authorise it.
fn setup_with_defaulted_invoice<'a>(
    env: &'a Env,
    admin: &Address,
    payout_caller: &Address,
    client: &super::InsuranceContractClient<'a>,
) -> (invofi_registry::RegistryContractClient<'a>, soroban_sdk::Symbol) {
    use soroban_sdk::symbol_short;
    // Deploy a registry, wire it to the insurance contract.
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(env, &registry_id);
    client.set_registry(admin, &registry_id);

    // Use the payout_caller as the authorised repayment contract on the
    // registry — mock_all_auths covers its auth so we can drive status.
    reg.set_repayment_contract(admin, payout_caller);

    // Register a minimal invoice and drive it to Defaulted.
    let originator = Address::generate(env);
    let invoice_id = symbol_short!("inv_dfl");
    // due_date in the past so mark_invoice_overdue passes the timestamp guard.
    let due_date: u64 = 1_000;
    reg.register_invoice(
        &invoice_id,
        &originator,
        &1_000_000_000i128,
        &symbol_short!("USDC"),
        &due_date,
    );
    // Pending -> Financed (registry needs a financing contract configured).
    reg.set_financing_contract(admin, payout_caller);
    reg.financing_marks_invoice_financed(&invoice_id);
    // Advance ledger past due_date so mark_invoice_overdue passes.
    env.ledger().set_timestamp(due_date + 1);
    // Financed -> Overdue (public, timestamp-gated).
    reg.mark_invoice_overdue(&invoice_id);
    // Overdue -> Defaulted via authorized repayment transition.
    reg.repayment_marks_defaulted(&invoice_id);

    (reg, invoice_id)
}

#[test]
fn test_payout_after_default_covers_claim() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    // Pool: 1M staked by a single staker.
    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    // Configure payout caller and registry with a Defaulted invoice.
    client.set_payout_caller(&admin, &payout_caller);
    assert_eq!(client.get_payout_caller(), Some(payout_caller.clone()));
    let (_reg, invoice_id) =
        setup_with_defaulted_invoice(&env, &admin, &payout_caller, &client);

    // Default triggers a 400k payout claim — fully covered by the pool.
    let paid = client.pay_out(&invoice_id, &beneficiary, &400_000);
    assert_eq!(paid, 400_000);

    // Pool accounting: pool 600k, staker claim 600k, lender funded.
    assert_eq!(client.get_pool_total(), 600_000);
    assert_eq!(client.get_stake(&staker), 600_000);
    assert_eq!(client.get_contract_token_balance(), 600_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&beneficiary), 400_000);
}

#[test]
fn test_payout_pool_depleted_pays_whats_left() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 100_000);
    client.stake(&staker, &100_000);
    client.set_payout_caller(&admin, &payout_caller);
    let (_reg, invoice_id) =
        setup_with_defaulted_invoice(&env, &admin, &payout_caller, &client);

    // Claim (1M) far exceeds the pool (100k) — lender gets everything left.
    // payout is capped at available reserves; it never exceeds pool_total.
    let paid = client.pay_out(&invoice_id, &beneficiary, &1_000_000);
    assert_eq!(paid, 100_000); // capped at available balance, not reverted

    assert_eq!(client.get_pool_total(), 0);
    assert_eq!(client.get_stake(&staker), 0);
    assert_eq!(client.get_stakers_count(), 0);
    assert_eq!(client.get_contract_token_balance(), 0);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&beneficiary), 100_000);

    // Subsequent call against an already-empty pool returns 0.
    assert_eq!(client.pay_out(&invoice_id, &beneficiary, &1_000_000), 0);
}

#[test]
fn test_payout_pro_rata_multiple_stakers_exact() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    // Pool 4M split 1M / 3M.
    let staker_a = Address::generate(&env);
    let staker_b = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker_a, 1_000_000);
    mint_and_approve(&env, &token_id, &insurance_id, &staker_b, 3_000_000);
    client.stake(&staker_a, &1_000_000);
    client.stake(&staker_b, &3_000_000);
    client.set_payout_caller(&admin, &payout_caller);
    let (_reg, invoice_id) =
        setup_with_defaulted_invoice(&env, &admin, &payout_caller, &client);

    // 2M payout -> each staker loses exactly their pro-rata share.
    let paid = client.pay_out(&invoice_id, &beneficiary, &2_000_000);
    assert_eq!(paid, 2_000_000);

    assert_eq!(client.get_pool_total(), 2_000_000);
    assert_eq!(client.get_stake(&staker_a), 500_000); // 25 % of the loss
    assert_eq!(client.get_stake(&staker_b), 1_500_000); // 75 % of the loss
    assert_eq!(client.get_contract_token_balance(), 2_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&beneficiary), 2_000_000);
}

// ── Rejection: invoice not Defaulted ─────────────────────────────────────────

/// pay_out must reject when the invoice is in any status other than Defaulted.
/// We test the most dangerous case: the invoice is Overdue (one step before
/// Defaulted) — the payout must still revert with a clear error.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_payout_rejected_when_invoice_overdue_not_defaulted() {
    use invofi_common::InvoiceStatus;
    use soroban_sdk::symbol_short;

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);
    client.set_payout_caller(&admin, &payout_caller);

    // Deploy registry, wire it — but only advance invoice to Overdue, NOT Defaulted.
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    client.set_registry(&admin, &registry_id);
    reg.set_repayment_contract(&admin, &payout_caller);
    reg.set_financing_contract(&admin, &payout_caller);

    let originator = Address::generate(&env);
    let invoice_id = symbol_short!("inv_ovd");
    reg.register_invoice(
        &invoice_id,
        &originator,
        &1_000_000_000i128,
        &symbol_short!("USDC"),
        &1_000u64,
    );
    reg.financing_marks_invoice_financed(&invoice_id);
    // Advance past due_date so mark_invoice_overdue passes.
    env.ledger().set_timestamp(1_001);
    // Stopped here — invoice is Overdue, not Defaulted.
    reg.mark_invoice_overdue(&invoice_id);

    let invoice = reg.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Overdue);

    // Must panic: "Invoice is not Defaulted"
    client.pay_out(&invoice_id, &beneficiary, &400_000);
}

/// pay_out must reject when the invoice is Pending (completely wrong state).
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_payout_rejected_when_invoice_pending() {
    use soroban_sdk::symbol_short;

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);
    client.set_payout_caller(&admin, &payout_caller);

    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    client.set_registry(&admin, &registry_id);

    let originator = Address::generate(&env);
    let invoice_id = symbol_short!("inv_pnd");
    reg.register_invoice(
        &invoice_id,
        &originator,
        &1_000_000_000i128,
        &symbol_short!("USDC"),
        &1_000_000u64,
    );
    // Invoice is still Pending — must panic.
    client.pay_out(&invoice_id, &beneficiary, &400_000);
}

// ── Existing guard tests (updated signatures) ─────────────────────────────────

#[test]
#[should_panic(expected = "No payout caller configured")]
fn test_payout_without_caller_panics() {
    use soroban_sdk::symbol_short;

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);
    let invoice_id = symbol_short!("inv_x");

    client.pay_out(&invoice_id, &beneficiary, &1_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_payout_zero_amount_panics() {
    use soroban_sdk::symbol_short;

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);
    client.set_payout_caller(&admin, &payout_caller);
    let invoice_id = symbol_short!("inv_x");

    client.pay_out(&invoice_id, &beneficiary, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_paused_blocks_payout() {
    use soroban_sdk::symbol_short;

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);
    client.set_payout_caller(&admin, &payout_caller);
    client.pause(&admin);
    let invoice_id = symbol_short!("inv_x");

    client.pay_out(&invoice_id, &beneficiary, &1_000);
}

// ── Registry not configured guard ─────────────────────────────────────────────

#[test]
#[should_panic(expected = "Registry not configured")]
fn test_payout_without_registry_panics() {
    use soroban_sdk::symbol_short;

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);
    // Payout caller configured, but NO registry wired — must fail-closed.
    client.set_payout_caller(&admin, &payout_caller);
    let invoice_id = symbol_short!("inv_x");

    client.pay_out(&invoice_id, &beneficiary, &500_000);
}

// ─── Partial Claim Tests (Issue #137) ────────────────────────────────────────

#[test]
fn test_reserve_and_claim_payout_basic() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_id = symbol_short!("off_1");
    let lender = Address::generate(&env);

    // Reserve 500_000 for this offer
    let reserved = client.reserve_payout(&offer_id, &500_000);
    assert_eq!(reserved, 500_000);
    assert_eq!(client.get_reserved(&offer_id), 500_000);

    // Claim 300_000 from the reserved amount
    let (paid, remaining) = client.claim_payout(&offer_id, &lender, &300_000);
    assert_eq!(paid, 300_000);
    assert_eq!(remaining, 200_000);
    assert_eq!(client.get_paid(&offer_id), 300_000);
    assert_eq!(client.get_pool_total(), 700_000);
    assert_eq!(client.get_stake(&staker), 700_000);

    // Claim remaining 200_000
    let (paid2, remaining2) = client.claim_payout(&offer_id, &lender, &200_000);
    assert_eq!(paid2, 200_000);
    assert_eq!(remaining2, 0);
    assert_eq!(client.get_pool_total(), 500_000);
}

#[test]
fn test_claim_payout_cannot_exceed_reserved() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_id = symbol_short!("off_1");
    let lender = Address::generate(&env);

    // Reserve only 200_000
    client.reserve_payout(&offer_id, &200_000);

    // Try to claim 500_000 — should be capped at 200_000
    let (paid, remaining) = client.claim_payout(&offer_id, &lender, &500_000);
    assert_eq!(paid, 200_000);
    assert_eq!(remaining, 0);
}

#[test]
fn test_claim_payout_cannot_exceed_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 100_000);
    client.stake(&staker, &100_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_id = symbol_short!("off_1");
    let lender = Address::generate(&env);

    // Reserve more than pool
    client.reserve_payout(&offer_id, &500_000);

    // Claim — should be capped at pool total (100_000)
    let (paid, remaining) = client.claim_payout(&offer_id, &lender, &500_000);
    assert_eq!(paid, 100_000);
    assert_eq!(client.get_pool_total(), 0);
}

#[test]
fn test_claim_payout_no_reservation_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_id = symbol_short!("off_1");
    let lender = Address::generate(&env);

    // No reservation — should panic
    let result = client.try_claim_payout(&offer_id, &lender, &100_000);
    assert!(result.is_err());
}

#[test]
fn test_reserve_zero_amount_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_token_id, _insurance_id, client) = setup(&env, &admin);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_id = symbol_short!("off_1");
    let result = client.try_reserve_payout(&offer_id, &0);
    assert!(result.is_err());
}

#[test]
fn test_multiple_offers_independent_reservations() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_a = symbol_short!("off_a");
    let offer_b = symbol_short!("off_b");
    let lender = Address::generate(&env);

    // Reserve for both offers
    client.reserve_payout(&offer_a, &400_000);
    client.reserve_payout(&offer_b, &300_000);

    // Claim from offer_a
    let (paid_a, _) = client.claim_payout(&offer_a, &lender, &400_000);
    assert_eq!(paid_a, 400_000);

    // offer_b reservation is independent — still claimable
    let (paid_b, _) = client.claim_payout(&offer_b, &lender, &300_000);
    assert_eq!(paid_b, 300_000);

    assert_eq!(client.get_pool_total(), 300_000);
}

#[test]
fn test_aggregate_reservations_capped_by_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_a = symbol_short!("off_a");
    let offer_b = symbol_short!("off_b");
    let offer_c = symbol_short!("off_c");

    // Reserve 800k for offer_a
    let r1 = client.reserve_payout(&offer_a, &800_000);
    assert_eq!(r1, 800_000);

    // Try to reserve 500k for offer_b — only 200k available
    let r2 = client.reserve_payout(&offer_b, &500_000);
    assert_eq!(r2, 200_000);

    // Pool is fully reserved — nothing left for offer_c
    let r3 = client.reserve_payout(&offer_c, &100_000);
    assert_eq!(r3, 0);

    // Claim from offer_a uses available pool
    let lender = Address::generate(&env);
    let (paid_a, _) = client.claim_payout(&offer_a, &lender, &800_000);
    assert_eq!(paid_a, 800_000);
}

#[test]
fn test_claim_after_depletion_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, _insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &_insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    let payout_caller = Address::generate(&env);
    client.set_payout_caller(&admin, &payout_caller);

    let offer_a = symbol_short!("off_a");
    let offer_b = symbol_short!("off_b");
    let lender = Address::generate(&env);

    // Reserve all pool capacity for offer_a
    client.reserve_payout(&offer_a, &1_000_000);

    // offer_b can't reserve — pool fully reserved by offer_a
    let r = client.reserve_payout(&offer_b, &500_000);
    assert_eq!(r, 0);

    // Claim from offer_b has no reservation, panics
    let result = client.try_claim_payout(&offer_b, &lender, &500_000);
    assert!(result.is_err());
}
