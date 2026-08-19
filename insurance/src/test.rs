
#![cfg(test)]
extern crate std;

use super::{InsuranceContract, InsuranceTier};
use soroban_sdk::{symbol_short, testutils::{Address as _, Ledger}, token, Address, Env};

/// Wrap a single signer in the one-element `Vec<Address>` the threshold-gated
/// admin API expects (ADR-0010). Single-admin/bootstrap deployments pass
/// exactly this.
fn one(env: &Env, signer: &Address) -> soroban_sdk::Vec<Address> {
    let mut v = soroban_sdk::Vec::new(env);
    v.push_back(signer.clone());
    v
}

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

// ─── Multisig admin governance tests (ADR-0010) ─────────────────────────────

#[test]
fn test_insurance_set_signers_requires_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let (_token_id, _insurance_id, client) = setup(&env, &admin);

    let b = Address::generate(&env);
    let mut two_signers = soroban_sdk::Vec::new(&env);
    two_signers.push_back(admin.clone());
    two_signers.push_back(b.clone());
    client.set_signers(&one(&env, &admin), &two_signers, &2u32);

    let result = client.try_pause(&one(&env, &admin));
    assert!(result.is_err(), "one of two required signatures must not pause");

    let mut both = soroban_sdk::Vec::new(&env);
    both.push_back(admin.clone());
    both.push_back(b.clone());
    client.pause(&both);
    assert!(client.contract_is_paused());
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

    client.pause(&one(&env, &admin));
    assert!(client.contract_is_paused());
    client.stake(&staker, &1_000_000);
}

#[test]
fn test_pause_blocks_state_changes_except_unstake() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);
    let staker = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let new_token = Address::generate(&env);

    client.pause(&one(&env, &admin));
    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(result.is_err(), "state-changing function should panic while paused");
    }

    assert_paused(|| {
        client.stake(&staker, &1_000i128);
    });
    assert_paused(|| {
        client.pay_out(&soroban_sdk::symbol_short!("inv_x"), &beneficiary, &1_000i128);
    });
    assert_paused(|| {
        client.transfer_admin(&one(&env, &admin), &new_admin);
    });
    assert_paused(|| {
        client.set_staking_token(&one(&env, &admin), &new_token);
    });
    assert_paused(|| {
        client.set_payout_caller(&one(&env, &admin), &payout_caller);
    });

    assert_eq!(client.get_pool_total(), 0);
    assert_eq!(client.get_stakers_count(), 0);
}

#[test]
fn test_paused_allows_unstake() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    client.stake(&staker, &1_000_000);

    client.pause(&one(&env, &admin));
    assert!(client.contract_is_paused());

    // Emergency withdrawal: unstake succeeds while paused (issue #67).
    client.unstake(&staker, &100_000);

    assert_eq!(client.get_stake(&staker), 900_000);
    assert_eq!(client.get_pool_total(), 900_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 100_000);
    assert_eq!(client.get_contract_token_balance(), 900_000);
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
    client.set_staking_token(&one(&env, &admin), &new_token);
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
    client.set_registry(&one(env, admin), &registry_id);

    // Use the payout_caller as the authorised repayment contract on the
    // registry — mock_all_auths covers its auth so we can drive status.
    reg.set_repayment_contract(&one(env, admin), payout_caller);

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
    reg.set_financing_contract(&one(env, admin), payout_caller);
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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);

    // Deploy registry, wire it — but only advance invoice to Overdue, NOT Defaulted.
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    client.set_registry(&one(&env, &admin), &registry_id);
    reg.set_repayment_contract(&one(&env, &admin), &payout_caller);
    reg.set_financing_contract(&one(&env, &admin), &payout_caller);

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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);

    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    client.set_registry(&one(&env, &admin), &registry_id);

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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
    client.pause(&one(&env, &admin));
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
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
    let invoice_id = symbol_short!("inv_x");

    client.pay_out(&invoice_id, &beneficiary, &500_000);
}

// ─── Yield tests (issue #130) ─────────────────────────────────────────────────
//
// All yield tests advance ledger timestamps with `env.ledger().set_timestamp`
// so the elapsed-seconds formula can be verified against known values.
//
// Formula reminder:
//   yield = principal * rate_bps * elapsed_secs / (10_000 * 31_536_000)

/// Helper: set a fresh ledger timestamp (seconds since Unix epoch).
fn set_ts(env: &Env, ts: u64) {
    env.ledger().set_timestamp(ts);
}

/// Mint tokens to the insurance contract itself so it can pay out yield.
/// In production the pool accumulates revenue from protocol fees / donations;
/// in tests we mint directly.
fn fund_yield_reserve(env: &Env, token_id: &Address, insurance_id: &Address, amount: i128) {
    let asset = token::StellarAssetClient::new(env, token_id);
    asset.mint(insurance_id, &amount);
}

// ── Zero-rate baseline ────────────────────────────────────────────────────────

/// With the default yield rate of 0 bps, accrued_yield must always return 0
/// and unstake pays exactly the principal — no yield is transferred or emitted.
#[test]
fn test_zero_yield_rate_no_accrual() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    assert_eq!(client.get_yield_rate(), 0);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);

    set_ts(&env, 0);
    client.stake(&staker, &1_000_000);

    // Advance one full year.
    set_ts(&env, 31_536_000);

    // No yield at 0 bps.
    assert_eq!(client.accrued_yield(&staker), 0);

    // Unstake returns exactly the principal.
    client.unstake(&staker, &1_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 1_000_000);
}

// ── Correct math at a known bps rate ─────────────────────────────────────────

/// Stake 1_000_000 at 500 bps (5 %) for exactly one year.
/// Expected yield = 1_000_000 * 500 / 10_000 = 50_000.
#[test]
fn test_yield_math_one_year_500_bps() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &500); // 5 % annual
    assert_eq!(client.get_yield_rate(), 500);

    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 100_000);

    set_ts(&env, 0);
    client.stake(&staker, &1_000_000);

    // Advance exactly one year.
    set_ts(&env, 31_536_000);

    // Preview: 1_000_000 * 500 * 31_536_000 / (10_000 * 31_536_000) = 50_000
    assert_eq!(client.accrued_yield(&staker), 50_000);

    // Unstake pays principal + yield.
    client.unstake(&staker, &1_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 1_050_000); // 1_000_000 + 50_000
}

/// Stake 2_000_000 at 1000 bps (10 %) for half a year.
/// Expected yield = 2_000_000 * 1000 * (31_536_000/2) / (10_000 * 31_536_000)
///               = 2_000_000 * 1000 / (10_000 * 2) = 100_000.
#[test]
fn test_yield_math_half_year_1000_bps() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &1_000); // 10 % annual
    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 2_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 200_000);

    set_ts(&env, 0);
    client.stake(&staker, &2_000_000);

    set_ts(&env, 31_536_000 / 2);

    assert_eq!(client.accrued_yield(&staker), 100_000);

    client.unstake(&staker, &2_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 2_100_000);
}

// ── Unstake pays principal + yield ────────────────────────────────────────────

/// Partial unstake: staker withdraws half their principal mid-period.
/// All banked yield is paid out on the partial unstake; the remaining stake
/// starts a fresh accrual period from that point.
#[test]
fn test_partial_unstake_pays_full_yield_then_resets() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &500); // 5 % annual
    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 2_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 200_000);

    // Stake 2_000_000 at t=0.
    set_ts(&env, 0);
    client.stake(&staker, &2_000_000);

    // Advance one full year → yield on 2_000_000 at 500 bps = 100_000.
    set_ts(&env, 31_536_000);
    assert_eq!(client.accrued_yield(&staker), 100_000);

    // Unstake half (1_000_000). All yield (100_000) is paid out and the
    // clock resets for the remaining 1_000_000 stake.
    client.unstake(&staker, &1_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    // Received: 1_000_000 principal + 100_000 yield = 1_100_000.
    assert_eq!(token_client.balance(&staker), 1_100_000);

    // Pool still has 1_000_000 staked, yield clock just reset.
    assert_eq!(client.get_stake(&staker), 1_000_000);
    assert_eq!(client.accrued_yield(&staker), 0);

    // After another year the remaining 1_000_000 accrues 50_000 more.
    set_ts(&env, 31_536_000 * 2);
    assert_eq!(client.accrued_yield(&staker), 50_000);

    client.unstake(&staker, &1_000_000);
    // Received additional: 1_000_000 + 50_000 = 1_050_000; total = 2_150_000.
    assert_eq!(token_client.balance(&staker), 2_150_000);
}

// ── Rate change is prospective only ──────────────────────────────────────────

/// Staker stakes for one year at 500 bps, then admin changes rate to 1000 bps.
/// Accrual to date (50_000) must be banked and not retroactively altered.
/// After another year the staker earns an additional 100_000 at 1000 bps.
#[test]
fn test_rate_change_prospective_only() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &500); // 5 % annual
    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 300_000);

    set_ts(&env, 0);
    client.stake(&staker, &1_000_000);

    // Advance one year → 50_000 accrued at 500 bps.
    set_ts(&env, 31_536_000);
    assert_eq!(client.accrued_yield(&staker), 50_000);

    // Admin changes rate to 1000 bps. This banks the 50_000 for all stakers.
    client.set_yield_rate(&one(&env, &admin), &1_000);

    // Immediately after rate change, accrued_yield should still show 50_000
    // (the banked amount; no new accrual yet because elapsed since bank = 0).
    assert_eq!(client.accrued_yield(&staker), 50_000);

    // Advance another year — new yield = 1_000_000 * 1000 / 10_000 = 100_000.
    set_ts(&env, 31_536_000 * 2);
    assert_eq!(client.accrued_yield(&staker), 150_000); // 50_000 banked + 100_000 new

    // Unstake: 1_000_000 principal + 150_000 yield.
    client.unstake(&staker, &1_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 1_150_000);
}

/// Rate decreasing: staker stakes for one year at 1000 bps, rate drops to 0.
/// Previously accrued yield must be preserved and paid on unstake.
#[test]
fn test_rate_decrease_preserves_accrued_yield() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &1_000); // 10 % annual
    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 1_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 200_000);

    set_ts(&env, 0);
    client.stake(&staker, &1_000_000);

    // One year → 100_000 accrued at 1000 bps.
    set_ts(&env, 31_536_000);
    assert_eq!(client.accrued_yield(&staker), 100_000);

    // Admin drops rate to 0. The 100_000 must be banked.
    client.set_yield_rate(&one(&env, &admin), &0);
    assert_eq!(client.accrued_yield(&staker), 100_000); // banked amount unchanged

    // No further accrual at 0 bps.
    set_ts(&env, 31_536_000 * 2);
    assert_eq!(client.accrued_yield(&staker), 100_000);

    client.unstake(&staker, &1_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 1_100_000);
}

// ── Top-up stake banks yield and resets clock ─────────────────────────────────

/// Staker stakes at t=0, tops up at t=1_year. The first year's yield must
/// be banked on the top-up; the second year accrues on the new total.
#[test]
fn test_topup_banks_yield_and_resets_clock() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &500); // 5 % annual
    let staker = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker, 3_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 200_000);

    // Initial stake of 1_000_000 at t=0.
    set_ts(&env, 0);
    client.stake(&staker, &1_000_000);
    assert_eq!(client.get_stake(&staker), 1_000_000);

    // Advance one year → 50_000 yield accrued on 1_000_000.
    set_ts(&env, 31_536_000);
    assert_eq!(client.accrued_yield(&staker), 50_000);

    // Top-up with 2_000_000. This banks the 50_000 and resets the clock.
    client.stake(&staker, &2_000_000);
    assert_eq!(client.get_stake(&staker), 3_000_000);
    // 50_000 is banked; no new time has elapsed since bank.
    assert_eq!(client.accrued_yield(&staker), 50_000);

    // Advance another year → new yield = 3_000_000 * 500 / 10_000 = 150_000.
    set_ts(&env, 31_536_000 * 2);
    // Total: 50_000 banked + 150_000 new = 200_000.
    assert_eq!(client.accrued_yield(&staker), 200_000);

    client.unstake(&staker, &3_000_000);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker), 3_200_000); // 3_000_000 + 200_000
}

// ── Multiple stakers, independent yield clocks ────────────────────────────────

/// Two stakers stake at different times; their yield clocks are independent.
#[test]
fn test_multiple_stakers_independent_yield() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);

    client.set_yield_rate(&one(&env, &admin), &500); // 5 % annual
    let staker_a = Address::generate(&env);
    let staker_b = Address::generate(&env);
    mint_and_approve(&env, &token_id, &insurance_id, &staker_a, 1_000_000);
    mint_and_approve(&env, &token_id, &insurance_id, &staker_b, 2_000_000);
    fund_yield_reserve(&env, &token_id, &insurance_id, 300_000);

    // A stakes at t=0.
    set_ts(&env, 0);
    client.stake(&staker_a, &1_000_000);

    // B stakes at t=half_year.
    set_ts(&env, 31_536_000 / 2);
    client.stake(&staker_b, &2_000_000);

    // At t=1_year:
    // A has been staking for a full year  → 50_000 yield.
    // B has been staking for half a year  → 50_000 yield.
    set_ts(&env, 31_536_000);
    assert_eq!(client.accrued_yield(&staker_a), 50_000);
    assert_eq!(client.accrued_yield(&staker_b), 50_000);

    // Unstake both and verify balances.
    client.unstake(&staker_a, &1_000_000);
    client.unstake(&staker_b, &2_000_000);

    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&staker_a), 1_050_000);
    assert_eq!(token_client.balance(&staker_b), 2_050_000);
}

// ── set_yield_rate guard: admin only ─────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_set_yield_rate_non_admin_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);

    let impostor = Address::generate(&env);
    // Calling with an address that is not the admin — must panic Unauthorized.
    // mock_all_auths satisfies the require_auth; the explicit admin-equality
    // check rejects the call.
    client.set_yield_rate(&one(&env, &impostor), &500);
}

// ── accrued_yield for non-staker returns 0 ───────────────────────────────────

#[test]
fn test_accrued_yield_non_staker_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (_, _, client) = setup(&env, &admin);
    client.set_yield_rate(&one(&env, &admin), &500);

    let stranger = Address::generate(&env);
    set_ts(&env, 31_536_000);
    assert_eq!(client.accrued_yield(&stranger), 0);
}

#[test]
fn test_tier_economics_staking_yield_and_pool_isolation() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);
    let conservative = Address::generate(&env);
    let balanced = Address::generate(&env);
    let aggressive = Address::generate(&env);
    for staker in [&conservative, &balanced, &aggressive] {
        mint_and_approve(&env, &token_id, &insurance_id, staker, 1_000_000);
    }
    fund_yield_reserve(&env, &token_id, &insurance_id, 200_000);
    set_ts(&env, 0);
    client.stake_tier(&conservative, &InsuranceTier::Conservative, &1_000_000);
    client.stake_tier(&balanced, &InsuranceTier::Balanced, &1_000_000);
    client.stake_tier(&aggressive, &InsuranceTier::Aggressive, &1_000_000);

    assert_eq!(client.get_pool(&InsuranceTier::Conservative).apy_bps, 200);
    assert_eq!(
        client.get_pool(&InsuranceTier::Conservative).payout_cap_bps,
        5_000
    );
    assert_eq!(client.get_pool(&InsuranceTier::Balanced).apy_bps, 500);
    assert_eq!(
        client.get_pool(&InsuranceTier::Balanced).payout_cap_bps,
        7_500
    );
    assert_eq!(client.get_pool(&InsuranceTier::Aggressive).apy_bps, 1_000);
    assert_eq!(
        client.get_pool(&InsuranceTier::Aggressive).payout_cap_bps,
        10_000
    );

    set_ts(&env, 31_536_000);
    assert_eq!(
        client.accrued_tier_yield(&conservative, &InsuranceTier::Conservative),
        20_000
    );
    assert_eq!(
        client.accrued_tier_yield(&balanced, &InsuranceTier::Balanced),
        50_000
    );
    assert_eq!(
        client.accrued_tier_yield(&aggressive, &InsuranceTier::Aggressive),
        100_000
    );

    client.unstake_tier(&conservative, &InsuranceTier::Conservative, &1_000_000);
    assert_eq!(client.get_pool(&InsuranceTier::Conservative).balance, 0);
    assert_eq!(client.get_pool(&InsuranceTier::Balanced).balance, 1_000_000);
    assert_eq!(
        client.get_pool(&InsuranceTier::Aggressive).balance,
        1_000_000
    );
}

#[test]
fn test_risk_assessment_uses_age_history_and_amount() {
    let env = Env::default();
    assert_eq!(
        super::InsuranceContractClient::new(
            &env,
            &env.register(
                InsuranceContract,
                (Address::generate(&env), Address::generate(&env))
            )
        )
        .assess_risk(&0, &10, &0, &10_000_000),
        InsuranceTier::Conservative
    );
}

#[test]
fn test_tier_payout_caps_are_isolated() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let payout_caller = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_id, insurance_id, client) = setup(&env, &admin);
    let staker_a = Address::generate(&env);
    let staker_b = Address::generate(&env);
    let staker_c = Address::generate(&env);
    for (staker, tier) in [
        (&staker_a, InsuranceTier::Conservative),
        (&staker_b, InsuranceTier::Balanced),
        (&staker_c, InsuranceTier::Aggressive),
    ] {
        mint_and_approve(&env, &token_id, &insurance_id, staker, 1_000_000);
        client.stake_tier(staker, &tier, &1_000_000);
    }
    client.set_payout_caller(&one(&env, &admin), &payout_caller);
    let (_registry, invoice_id) =
        setup_with_defaulted_invoice(&env, &admin, &payout_caller, &client);

    assert_eq!(
        client.pay_out_tier(
            &invoice_id,
            &InsuranceTier::Conservative,
            &beneficiary,
            &1_000_000
        ),
        500_000
    );
    assert_eq!(client.get_pool(&InsuranceTier::Balanced).balance, 1_000_000);
    assert_eq!(
        client.get_pool(&InsuranceTier::Aggressive).balance,
        1_000_000
    );
    assert_eq!(
        client.pay_out_tier(
            &invoice_id,
            &InsuranceTier::Balanced,
            &beneficiary,
            &1_000_000
        ),
        750_000
    );
    assert_eq!(
        client.pay_out_tier(
            &invoice_id,
            &InsuranceTier::Aggressive,
            &beneficiary,
            &1_000_000
        ),
        1_000_000
    );
}
