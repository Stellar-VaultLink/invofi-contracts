#![cfg(test)]
extern crate std;

use super::RepaymentContract;
use invofi_common::{InvoiceStatus, OfferStatus};
use invofi_financing::FinancingContract;
use invofi_insurance::InsuranceContract;
use invofi_registry::RegistryContract;
use invofi_reputation::ReputationContract;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

/// Deploy all three contracts (registry, financing, repayment) and return
/// their clients. All share the same admin and token.
fn setup_contracts<'a>(
    env: &'a Env,
    admin: &Address,
    token: &Address,
) -> (
    invofi_registry::RegistryContractClient<'a>,
    invofi_financing::FinancingContractClient<'a>,
    super::RepaymentContractClient<'a>,
) {
    // Registry
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(env, &registry_id);

    // Financing
    let financing_id =
        env.register(FinancingContract, (admin.clone(), registry_id.clone(), token.clone()));
    let fin = invofi_financing::FinancingContractClient::new(env, &financing_id);

    // Repayment
    let repayment_id = env.register(
        RepaymentContract,
        (
            admin.clone(),
            registry_id.clone(),
            financing_id.clone(),
            token.clone(),
        ),
    );
    let rep = super::RepaymentContractClient::new(env, &repayment_id);

    // Register repayment contract with financing (for authorized callbacks)
    fin.set_repayment_contract(admin, &repayment_id);

    // Register both contracts as trusted callers on the registry so the
    // cross-contract status transitions (accept + repay) are allowed.
    reg.set_repayment_contract(admin, &repayment_id);
    reg.set_financing_contract(admin, &financing_id);

    (reg, fin, rep)
}

/// Deploy a fresh test SEP-41 token and return its contract address.
fn create_token(env: &Env) -> Address {
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    sac.address()
}

/// Mint `amount` to `who` and approve `spender` to move those funds (the same
/// flow a real lender runs on-chain before `accept_offer`).
fn mint_and_approve(
    env: &Env,
    token_id: &Address,
    spender: &Address,
    who: &Address,
    amount: i128,
) {
    let asset_client = token::StellarAssetClient::new(env, token_id);
    asset_client.mint(who, &amount);

    let token_client = token::TokenClient::new(env, token_id);
    token_client.approve(who, spender, &amount, &(env.ledger().sequence() + 1000));
}

// ─── Full lifecycle tests ───────────────────────────────────────────────────

#[test]
fn test_repay_invoice_partial_then_full() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv001");
    let offer_id = symbol_short!("off001");
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500; // 5.00%

    // Deploy all three contracts
    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    // Register invoice
    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );

    // Create and accept offer (funded_at = 1_000_000)
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &(2_592_000u64),
    );
    fin.accept_offer(&offer_id, &originator);

    // Advance 1 day to accrue pro-rata interest.
    // accrued = 1B * 500 * 1 / 3_650_000 = 136_986
    env.ledger().set_timestamp(funded_at + 86_400);

    // Mint repayment funds to originator (principal + accrued interest).
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let expected_interest_1 = amount * (interest_rate as i128) / 3_650_000;
    asset_client.mint(&originator, &(amount + expected_interest_1));

    // Partial repayment via Repayment contract (50% of principal)
    let partial_amount = amount / 2;
    let repaid = rep.repay_invoice(&invoice_id, &offer_id, &originator, &partial_amount);
    assert_eq!(repaid.status, InvoiceStatus::Financed);

    // Verify offer state via Financing contract
    let offer = fin.get_offer(&offer_id);
    assert_eq!(offer.status, OfferStatus::Financed);
    assert_eq!(offer.amount_repaid, partial_amount);

    // Verify lender received funds
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&lender), partial_amount);

    // Verify payment history has 1 record
    let history = rep.get_payment_history(&invoice_id);
    assert_eq!(history.len(), 1);

    // Verify remaining principal
    let remaining = rep.get_remaining_principal(&offer_id);
    // principal_portion = 500M - 136_986 (interest) = 499_863_014
    // remaining = 1B - 499_863_014 = 500_136_986
    assert_eq!(remaining, amount - (partial_amount - expected_interest_1));

    // Advance 1 more day for second payment
    env.ledger().set_timestamp(funded_at + 2 * 86_400);

    // Full repayment: remaining principal + accrued interest on remaining.
    let remaining_after = rep.get_remaining_principal(&offer_id);
    let accrued_2 = remaining_after * (interest_rate as i128) * 2 / 3_650_000;
    let total_remaining = remaining_after + accrued_2;
    asset_client.mint(&originator, &total_remaining);
    let repaid_final = rep.repay_invoice(&invoice_id, &offer_id, &originator, &total_remaining);
    assert_eq!(repaid_final.status, InvoiceStatus::Repaid);

    let settled_offer = fin.get_offer(&offer_id);
    assert_eq!(settled_offer.status, OfferStatus::Repaid);

    // Verify payment history has 2 records
    let history2 = rep.get_payment_history(&invoice_id);
    assert_eq!(history2.len(), 2);
}

// ─── Edge case tests ──────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_repay_invoice_overpayment_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500;
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &symbol_short!("inv_op"),
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_op"),
        &symbol_short!("inv_op"),
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &(2_592_000u64),
    );
    fin.accept_offer(&symbol_short!("off_op"), &originator);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&originator, &total_due);

    rep.repay_invoice(
        &symbol_short!("inv_op"),
        &symbol_short!("off_op"),
        &originator,
        &(total_due + 1),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_repay_unfinanced_invoice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin, rep) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_uf"),
        &originator,
        &(1_000_000_000i128),
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_uf"),
        &symbol_short!("inv_uf"),
        &lender,
        &(1_000_000_000i128),
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    // Offer NOT accepted — invoice stays Pending
    rep.repay_invoice(
        &symbol_short!("inv_uf"),
        &symbol_short!("off_uf"),
        &originator,
        &1,
    );
}

#[test]
#[should_panic(expected = "repayment amount must be greater than zero")]
fn test_repay_zero_amount_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &symbol_short!("inv_za"),
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_za"),
        &symbol_short!("inv_za"),
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    fin.accept_offer(&symbol_short!("off_za"), &originator);

    rep.repay_invoice(
        &symbol_short!("inv_za"),
        &symbol_short!("off_za"),
        &originator,
        &0,
    );
}

// ─── Overdue / Reclaim tests ──────────────────────────────────────────────

#[test]
fn test_reclaim_invoice_after_grace_period() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv_rc");
    let offer_id = symbol_short!("off_rc");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    fin.accept_offer(&offer_id, &originator);

    // Move past due_date + grace period
    env.ledger()
        .set_timestamp(due_date + invofi_common::GRACE_PERIOD_SECS + 1);

    // Mark overdue via Repayment (delegates to registry)
    rep.mark_overdue(&invoice_id);

    // Verify invoice is Overdue in registry
    let invoice = reg.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Overdue);

    let reclaimed = rep.reclaim_invoice(&invoice_id, &offer_id, &lender);
    assert_eq!(reclaimed.status, OfferStatus::Defaulted);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_reclaim_before_grace_period_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv_gp");
    let offer_id = symbol_short!("off_gp");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    fin.accept_offer(&offer_id, &originator);

    // Just past due_date, but not past grace period
    env.ledger().set_timestamp(due_date + 1);
    rep.mark_overdue(&invoice_id);
    rep.reclaim_invoice(&invoice_id, &offer_id, &lender);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_reclaim_on_non_overdue_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 3_000_000;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &symbol_short!("inv_nr"),
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    fin.create_offer(
        &symbol_short!("off_nr"),
        &symbol_short!("inv_nr"),
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    fin.accept_offer(&symbol_short!("off_nr"), &originator);

    // Invoice is Financed, not Overdue — should panic
    rep.reclaim_invoice(&symbol_short!("inv_nr"), &symbol_short!("off_nr"), &lender);
}

// ─── Query helper tests ───────────────────────────────────────────────────

#[test]
fn test_calculate_total_due() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, 10_000i128);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &symbol_short!("inv_td"),
        &originator,
        &100_000_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_td"),
        &symbol_short!("inv_td"),
        &lender,
        &10_000i128,
        &symbol_short!("XLM"),
        &1_000u32, // 10%
        &86_400u64,
    );
    fin.accept_offer(&symbol_short!("off_td"), &originator);

    // Advance 365 days so pro-rata interest = principal * rate * 365 / 3_650_000
    // = 10_000 * 1_000 * 365 / 3_650_000 = 1_000 (same as flat yield)
    env.ledger().set_timestamp(1_000_000 + 365 * 86_400);
    let due = rep.calculate_total_due(&symbol_short!("off_td"));
    assert_eq!(due, 11_000i128);
}

#[test]
fn test_calculate_total_due_after_partial() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &symbol_short!("inv_tp"),
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_tp"),
        &symbol_short!("inv_tp"),
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &(2_592_000u64),
    );
    fin.accept_offer(&symbol_short!("off_tp"), &originator);

    // Advance 365 days so pro-rata interest = 1_000_000_000 * 500 * 365 / 3_650_000 = 50_000_000
    env.ledger().set_timestamp(funded_at + 365 * 86_400);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&originator, &(amount + 50_000_000)); // principal + accrued interest

    // Partial repayment of 50% of principal
    let partial = amount / 2;
    rep.repay_invoice(
        &symbol_short!("inv_tp"),
        &symbol_short!("off_tp"),
        &originator,
        &partial,
    );

    // After partial payment of 500M:
    // interest_portion = min(500M, 50M accrued) = 50M
    // principal_portion = 500M - 50M = 450M
    // remaining_principal = 1B - 450M = 550M
    // accrued on 550M at same timestamp: 550M * 500 * 365 / 3_650_000 = 27_500_000
    let remaining = rep.calculate_total_due(&symbol_short!("off_tp"));
    assert_eq!(remaining, 550_000_000 + 27_500_000);
}

// ─── Version test ──────────────────────────────────────────────────────────

#[test]
fn test_version_returns_nonempty_string() {
    let env = Env::default();
    env.mock_all_auths();
    let repayment_id = env.register(
        RepaymentContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    let ver = rep.version();
    assert!(!ver.is_empty());
}

#[test]
fn test_get_duration_limits() {
    let env = Env::default();
    let repayment_id = env.register(
        RepaymentContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    let (min, max) = rep.get_duration_limits();
    assert_eq!(min, invofi_common::MIN_OFFER_DURATION_SECS);
    assert_eq!(max, invofi_common::MAX_OFFER_DURATION_SECS);
}

// ─── Task 4A: emergency pause / circuit breaker ──────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_pause_blocks_repay_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("invp2");
    let offer_id = symbol_short!("offp2");
    let amount: i128 = 1_000_000_000;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
    );
    fin.accept_offer(&offer_id, &originator);

    rep.pause(&admin);
    rep.repay_invoice(&invoice_id, &offer_id, &originator, &amount);
}

#[test]
fn test_pause_blocks_all_repayment_state_changes() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_, _, rep) = setup_contracts(&env, &admin, &token);
    let insurance = Address::generate(&env);
    let reputation = Address::generate(&env);
    let new_admin = Address::generate(&env);

    rep.pause(&admin);
    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(result.is_err(), "state-changing function should panic while paused");
    }

    assert_paused(|| {
        rep.repay_invoice(
            &symbol_short!("invx"),
            &symbol_short!("offx"),
            &Address::generate(&env),
            &1_000i128,
        );
    });
    assert_paused(|| {
        rep.mark_overdue(&symbol_short!("invx"));
    });
    assert_paused(|| {
        rep.reclaim_invoice(
            &symbol_short!("invx"),
            &symbol_short!("offx"),
            &Address::generate(&env),
        );
    });
    assert_paused(|| {
        rep.set_insurance(&admin, &insurance);
    });
    assert_paused(|| {
        rep.set_reputation(&admin, &reputation);
    });
    assert_paused(|| {
        rep.transfer_admin(&admin, &new_admin);
    });

    assert_eq!(rep.get_duration_limits().0, invofi_common::MIN_OFFER_DURATION_SECS);
}

// ─── Default-flow integration tests (Task 10 + 11) ───────────────────────────

#[test]
fn test_reclaim_triggers_defaulted_payout_and_reputation() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let staker = Address::generate(&env);
    let invoice_id = symbol_short!("inv_dfl");
    let offer_id = symbol_short!("off_dfl");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    // Insurance pool, funded by a third-party staker with the same token
    // the loan settles in (300M coverage against a 1.05B obligation).
    let insurance_id = env.register(InsuranceContract, (admin.clone(), token_id.clone()));
    let ins = invofi_insurance::InsuranceContractClient::new(&env, &insurance_id);
    let asset = token::StellarAssetClient::new(&env, &token_id);
    asset.mint(&staker, &300_000_000);
    let tok = token::TokenClient::new(&env, &token_id);
    tok.approve(
        &staker,
        &insurance_id,
        &300_000_000,
        &(env.ledger().sequence() + 1000),
    );
    ins.stake(&staker, &300_000_000);
    ins.set_payout_caller(&admin, &repayment_id);
    // Wire the registry into the insurance contract so pay_out can verify
    // the invoice is Defaulted on-chain before moving staked funds.
    ins.set_registry(&admin, &registry_id);

    // Reputation contract, recorder = repayment.
    let reputation_id = env.register(ReputationContract, (admin.clone(),));
    let repu = invofi_reputation::ReputationContractClient::new(&env, &reputation_id);
    repu.set_recorder(&admin, &repayment_id);

    // Wire repayment -> insurance + reputation.
    rep.set_insurance(&admin, &insurance_id);
    rep.set_reputation(&admin, &reputation_id);

    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    fin.accept_offer(&offer_id, &originator);

    // Past due + grace period, then mark overdue.
    env.ledger()
        .set_timestamp(due_date + invofi_common::GRACE_PERIOD_SECS + 1);
    rep.mark_overdue(&invoice_id);

    // Lender's principal moved out on acceptance — zero balance before reclaim.
    assert_eq!(tok.balance(&lender), 0);

    let reclaimed = rep.reclaim_invoice(&invoice_id, &offer_id, &lender);
    assert_eq!(reclaimed.status, OfferStatus::Defaulted);

    // 1. Invoice transitioned Overdue -> Defaulted in the registry.
    let invoice = reg.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Defaulted);

    // 2. Lender received the insurance payout, capped at the pool (300M).
    let total_due = amount + amount * 500 / 10_000; // 1_050_000_000
    assert!(total_due > 300_000_000);
    assert_eq!(tok.balance(&lender), 300_000_000);

    // 3. Pool drained exactly: accounting total, token balance, staker claim.
    assert_eq!(ins.get_pool_total(), 0);
    assert_eq!(ins.get_stake(&staker), 0);
    assert_eq!(ins.get_contract_token_balance(), 0);

    // 4. Reputation: one default recorded -> score floored at 0.
    assert_eq!(repu.get_score(&originator), 0);
    let rec = repu.get_record(&originator);
    assert_eq!(rec.repayments, 0);
    assert_eq!(rec.defaults, 1);
}

#[test]
fn test_full_repay_records_reputation_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv_rs");
    let offer_id = symbol_short!("off_rs");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let token_id = create_token(&env);
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
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    let reputation_id = env.register(ReputationContract, (admin.clone(),));
    let repu = invofi_reputation::ReputationContractClient::new(&env, &reputation_id);
    repu.set_recorder(&admin, &repayment_id);
    rep.set_reputation(&admin, &reputation_id);

    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    fin.accept_offer(&offer_id, &originator);

    // Advance 365 days so pro-rata interest = 50_000_000 (matches flat yield).
    env.ledger().set_timestamp(1_000_000 + 365 * 86_400);

    // Originator repays principal + accrued interest in full.
    let total_due = amount + amount * 500 / 10_000;
    let asset = token::StellarAssetClient::new(&env, &token_id);
    asset.mint(&originator, &total_due);
    rep.repay_invoice(&invoice_id, &offer_id, &originator, &total_due);

    // Reputation: one successful repayment -> score 1.
    assert_eq!(repu.get_score(&originator), 1);
    let rec = repu.get_record(&originator);
    assert_eq!(rec.repayments, 1);
    assert_eq!(rec.defaults, 0);
}

// ─── Repayment schedule helper tests (issue #133) ──────────────────────────

/// The repayment contract's get_installment_due proxy delegates correctly
/// to the financing contract.
#[test]
fn test_repayment_get_installment_due_proxy() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_200_000_000;

    let token_id = create_token(&env);
    let (reg, fin, rep) = setup_contracts(&env, &admin, &token_id);

    reg.register_invoice(
        &symbol_short!("inv_pr"),
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(env.ledger().timestamp() + 100_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_pr"),
        &symbol_short!("inv_pr"),
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(31_536_000u64),
    );

    // 4 weekly installments.
    // installment_principal = 1_200_000_000 / 4 = 300_000_000
    // installment_amount    = 300_000_000 + 300_000_000*500/10_000 = 315_000_000
    let first_due = env.ledger().timestamp() + 604_800;
    let sched = fin.schedule_repayment(
        &symbol_short!("off_pr"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &4u32,
        &first_due,
    );

    // Before first_due: proxy returns 0.
    assert_eq!(rep.get_installment_due(&symbol_short!("off_pr")), 0);

    // Advance past first_due — installment 1 elapsed, 0 paid → returns 1.
    env.ledger().set_timestamp(first_due + 1);
    assert_eq!(rep.get_installment_due(&symbol_short!("off_pr")), 1);

    // Mark installment 1 as paid via financing callback, then advance past
    // second period — installment 2 now due → returns 2.
    fin.update_offer_amount_repaid(&symbol_short!("off_pr"), &sched.installment_amount);
    env.ledger().set_timestamp(first_due + 604_800 + 1);
    assert_eq!(rep.get_installment_due(&symbol_short!("off_pr")), 2);
}

/// After a full repayment, get_installment_due returns 0 via the proxy.
#[test]
fn test_repayment_get_installment_due_zero_after_full_repay() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_050_000_000; // 12 × 87_500_000 principal
    // 500 bps interest → per-installment: 87_500_000 + 4_375_000 = 91_875_000
    // 12 installments × 91_875_000 = 1_102_500_000 total due

    let token_id = create_token(&env);
    let (reg, fin, rep) = setup_contracts(&env, &admin, &token_id);

    let interest_rate: u32 = 500;
    let yield_amt = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amt;

    reg.register_invoice(
        &symbol_short!("inv_fd"),
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(env.ledger().timestamp() + 100_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_fd"),
        &symbol_short!("inv_fd"),
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &(31_536_000u64),
    );

    let first_due = env.ledger().timestamp() + 604_800;
    fin.schedule_repayment(
        &symbol_short!("off_fd"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &12u32,
        &first_due,
    );

    // Accept offer + fund the repayer.
    mint_and_approve(&env, &token_id, &fin.address, &lender, amount);
    fin.accept_offer(&symbol_short!("off_fd"), &originator);

    // Advance 365 days so pro-rata interest = flat yield (52_500_000).
    env.ledger().set_timestamp(1_000_000 + 365 * 86_400);

    let asset = token::StellarAssetClient::new(&env, &token_id);
    asset.mint(&originator, &total_due);

    // Fully repay.
    rep.repay_invoice(
        &symbol_short!("inv_fd"),
        &symbol_short!("off_fd"),
        &originator,
        &total_due,
    );

    // Advance past all 12 periods — proxy must return 0 (Repaid offer).
    env.ledger().set_timestamp(first_due + 12 * 604_800 + 1);
    assert_eq!(rep.get_installment_due(&symbol_short!("off_fd")), 0);
}

// ─── Overdue penalty interest (ADR-0007, issue #49) ─────────────────────────

/// Principal for the penalty fixtures.
const PEN_AMOUNT: i128 = 1_000_000_000;
/// 5.00% flat yield → 50_000_000.
const PEN_RATE: u32 = 500;
/// The **frozen** accrual base: principal + yield. Per ADR-0007 decision 2
/// this does not shrink as repayments land.
const PEN_TOTAL_DUE: i128 = 1_050_000_000;
/// Invoice due date used by every penalty fixture.
const PEN_DUE_DATE: u64 = 1_735_689_600;
/// 0.10% per day of the frozen base → 1_050_000 per elapsed day.
const PEN_BPS: u32 = 10;
/// Ceiling at 30% of the frozen base → 315_000_000, reached at day 300.
const PEN_CAP_BPS: u32 = 3_000;
const PEN_PER_DAY: i128 = 1_050_000;
const PEN_CAP: i128 = 315_000_000;

struct PenCase<'a> {
    reg: invofi_registry::RegistryContractClient<'a>,
    fin: invofi_financing::FinancingContractClient<'a>,
    rep: super::RepaymentContractClient<'a>,
    token_id: Address,
    registry_id: Address,
    repayment_id: Address,
    admin: Address,
    originator: Address,
    lender: Address,
}

/// A Financed invoice of `PEN_AMOUNT` at `PEN_RATE`, due at `PEN_DUE_DATE`,
/// with the originator funded well past `PEN_TOTAL_DUE` so tests can settle
/// principal, yield and penalty. Penalty accrual is left **disabled** — each
/// test opts in via `set_penalty`, which is the deployed default per ADR-0007
/// decision 6.
fn setup_penalty_case<'a>(env: &'a Env) -> PenCase<'a> {
    let admin = Address::generate(env);
    let originator = Address::generate(env);
    let lender = Address::generate(env);

    let token_id = create_token(env);
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(env, &registry_id);

    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), registry_id.clone(), token_id.clone()),
    );
    let fin = invofi_financing::FinancingContractClient::new(env, &financing_id);

    let repayment_id = env.register(
        RepaymentContract,
        (
            admin.clone(),
            registry_id.clone(),
            financing_id.clone(),
            token_id.clone(),
        ),
    );
    let rep = super::RepaymentContractClient::new(env, &repayment_id);

    mint_and_approve(env, &token_id, &financing_id, &lender, PEN_AMOUNT);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    reg.register_invoice(
        &symbol_short!("inv_pen"),
        &originator,
        &PEN_AMOUNT,
        &symbol_short!("USDC"),
        &PEN_DUE_DATE,
    );
    fin.create_offer(
        &symbol_short!("off_pen"),
        &symbol_short!("inv_pen"),
        &lender,
        &PEN_AMOUNT,
        &symbol_short!("USDC"),
        &PEN_RATE,
        &(2_592_000u64),
    );
    fin.accept_offer(&symbol_short!("off_pen"), &originator);

    // Fund the originator beyond the capped worst case.
    token::StellarAssetClient::new(env, &token_id).mint(&originator, &2_000_000_000);

    PenCase {
        reg,
        fin,
        rep,
        token_id,
        registry_id,
        repayment_id,
        admin,
        originator,
        lender,
    }
}

/// Advance the ledger to exactly `days` whole days past the due date.
fn at_days_overdue(env: &Env, days: u64) {
    env.ledger().set_timestamp(PEN_DUE_DATE + days * 86_400);
}

#[test]
fn test_penalty_disabled_by_default() {
    let env = Env::default();
    env.mock_all_auths();
    // Set initial timestamp close to PEN_DUE_DATE so pro-rata interest is predictable.
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);

    // Deployed default: both parameters zero, accrual inert.
    assert_eq!(c.rep.get_penalty_bps(), 0);
    assert_eq!(c.rep.get_penalty_cap_bps(), 0);

    // Ten days past due and still no penalty — a deployment that never calls
    // set_penalty behaves exactly as it did before ADR-0007.
    at_days_overdue(&env, 10);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), 0);
    // Pro-rata interest at 40 days: 1B * 500 * 40 / 3_650_000 = 5_479_452
    let expected_pro_rata = PEN_AMOUNT * (PEN_RATE as i128) * 40 / 3_650_000;
    assert_eq!(
        c.rep.calculate_total_due(&symbol_short!("off_pen")),
        PEN_AMOUNT + expected_pro_rata
    );
}

#[test]
fn test_penalty_zero_cap_disables_accrual() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);

    // A rate with a zero ceiling accrues nothing — the cap is a hard bound,
    // so a zero cap is a hard zero.
    c.rep.set_penalty(&c.admin, &PEN_BPS, &0u32);
    at_days_overdue(&env, 10);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), 0);
}

#[test]
fn test_penalty_accrues_in_whole_days() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    at_days_overdue(&env, 1);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        PEN_PER_DAY
    );

    at_days_overdue(&env, 5);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        5 * PEN_PER_DAY
    );

    // calculate_total_due reports remaining principal + pro-rata interest + penalty.
    // At 35 days since funded: pro-rata = 1B * 500 * 35 / 3_650_000 = 4_794_520
    let expected_pro_rata = PEN_AMOUNT * (PEN_RATE as i128) * 35 / 3_650_000;
    assert_eq!(
        c.rep.calculate_total_due(&symbol_short!("off_pen")),
        PEN_AMOUNT + expected_pro_rata + 5 * PEN_PER_DAY
    );
}

#[test]
fn test_penalty_truncates_partial_day_toward_borrower() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    // One second short of day 5: the day in progress is not charged, so the
    // borrower is billed for 4 days. ADR-0007 decision 3 — rounding runs in
    // the borrower's favour, deliberately.
    env.ledger().set_timestamp(PEN_DUE_DATE + 5 * 86_400 - 1);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        4 * PEN_PER_DAY
    );

    // The boundary second itself tips it to 5.
    env.ledger().set_timestamp(PEN_DUE_DATE + 5 * 86_400);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        5 * PEN_PER_DAY
    );
}

#[test]
fn test_penalty_not_accrued_before_or_at_due_date() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    env.ledger().set_timestamp(PEN_DUE_DATE - 1);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), 0);

    // Exactly on the due date is not yet late.
    env.ledger().set_timestamp(PEN_DUE_DATE);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), 0);

    // And the first second past it has not completed a day.
    env.ledger().set_timestamp(PEN_DUE_DATE + 1);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), 0);
}

#[test]
fn test_penalty_stops_at_hard_cap() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    // Day 299: still below the ceiling, accruing linearly.
    at_days_overdue(&env, 299);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        299 * PEN_PER_DAY
    );

    // Day 300: raw accrual meets the ceiling exactly.
    at_days_overdue(&env, 300);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), PEN_CAP);

    // Day 400 and day 5_000: pinned at the ceiling. Without this bound a
    // long-abandoned invoice would accrue purely as a function of neglect.
    at_days_overdue(&env, 400);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), PEN_CAP);
    at_days_overdue(&env, 5_000);
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), PEN_CAP);
}

#[test]
fn test_penalty_base_frozen_across_partial_repayment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    at_days_overdue(&env, 5);
    let before = c.rep.calculate_penalty(&symbol_short!("off_pen"));
    assert_eq!(before, 5 * PEN_PER_DAY);

    // Pay down almost the entire obligation at day 5.
    c.rep.repay_invoice(
        &symbol_short!("inv_pen"),
        &symbol_short!("off_pen"),
        &c.originator,
        &1_000_000_000i128,
    );

    // The accrued penalty is unchanged. This is the retroactive-erasure hole
    // ADR-0007 decision 2 closes: if the base tracked the *outstanding*
    // balance, this 95% paydown would have collapsed the 5 days of accrued
    // penalty to a fraction of its value.
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), before);

    // Accrual continues on the frozen base, not on the reduced outstanding.
    at_days_overdue(&env, 10);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        10 * PEN_PER_DAY
    );

    // Monotonically non-decreasing across the whole window, repayment or not.
    let mut last = 0i128;
    for day in [6u64, 7, 20, 100, 299, 300, 900] {
        at_days_overdue(&env, day);
        let p = c.rep.calculate_penalty(&symbol_short!("off_pen"));
        assert!(p >= last, "penalty must never decrease over time");
        last = p;
    }
}

#[test]
fn test_penalty_must_be_settled_for_full_repayment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    at_days_overdue(&env, 5);
    let _penalty = 5 * PEN_PER_DAY;

    // Pay half the principal (interest is paid first, then principal).
    // This ensures some principal remains outstanding so the offer stays Financed.
    let half_principal = PEN_AMOUNT / 2;
    let partial_payment = half_principal; // enough to pay accrued interest + some principal

    // Query the total owed to verify it exceeds partial_payment.
    let total = c.rep.calculate_total_due(&symbol_short!("off_pen"));
    assert!(total > partial_payment);

    let inv = c.rep.repay_invoice(
        &symbol_short!("inv_pen"),
        &symbol_short!("off_pen"),
        &c.originator,
        &partial_payment,
    );
    assert_eq!(inv.status, InvoiceStatus::Financed);
    assert_eq!(
        c.fin.get_offer(&symbol_short!("off_pen")).status,
        OfferStatus::Financed
    );

    // Remaining balance is still positive.
    let remaining_after = c.rep.calculate_total_due(&symbol_short!("off_pen"));
    assert!(remaining_after > 0);

    // Now pay the remaining balance to close it out.
    token::StellarAssetClient::new(&env, &c.token_id).mint(&c.originator, &remaining_after);
    let inv = c.rep.repay_invoice(
        &symbol_short!("inv_pen"),
        &symbol_short!("off_pen"),
        &c.originator,
        &remaining_after,
    );
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    let offer = c.fin.get_offer(&symbol_short!("off_pen"));
    assert_eq!(offer.status, OfferStatus::Repaid);
    assert_eq!(c.rep.calculate_total_due(&symbol_short!("off_pen")), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_penalty_overpayment_beyond_accrued_total_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    at_days_overdue(&env, 5);
    // Total owed = principal + pro-rata interest + penalty.
    // Paying one stroop more than the total should panic.
    let accrued = PEN_AMOUNT * (PEN_RATE as i128) * 35 / 3_650_000;
    let total = PEN_AMOUNT + accrued + 5 * PEN_PER_DAY;
    c.rep.repay_invoice(
        &symbol_short!("inv_pen"),
        &symbol_short!("off_pen"),
        &c.originator,
        &(total + 1),
    );
}

#[test]
fn test_penalty_accrues_while_invoice_marked_overdue() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    // Accrual is anchored on due_date, not on the status transition, so
    // flipping the invoice to Overdue neither starts nor resets the meter.
    at_days_overdue(&env, 5);
    let before = c.rep.calculate_penalty(&symbol_short!("off_pen"));
    c.rep.mark_overdue(&symbol_short!("inv_pen"));
    assert_eq!(
        c.reg.get_invoice(&symbol_short!("inv_pen")).status,
        InvoiceStatus::Overdue
    );
    assert_eq!(c.rep.calculate_penalty(&symbol_short!("off_pen")), before);

    at_days_overdue(&env, 9);
    assert_eq!(
        c.rep.calculate_penalty(&symbol_short!("off_pen")),
        9 * PEN_PER_DAY
    );
}

#[test]
fn test_penalty_excluded_from_insurance_payout() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);

    // A pool deliberately deep enough to cover the full claim, so the
    // assertion below distinguishes "penalty excluded" from "pool exhausted".
    let staker = Address::generate(&env);
    let insurance_id = env.register(InsuranceContract, (c.admin.clone(), c.token_id.clone()));
    let ins = invofi_insurance::InsuranceContractClient::new(&env, &insurance_id);
    let asset = token::StellarAssetClient::new(&env, &c.token_id);
    asset.mint(&staker, &3_000_000_000);
    let tok = token::TokenClient::new(&env, &c.token_id);
    tok.approve(
        &staker,
        &insurance_id,
        &3_000_000_000,
        &(env.ledger().sequence() + 1000),
    );
    ins.stake(&staker, &3_000_000_000);
    ins.set_payout_caller(&c.admin, &c.repayment_id);
    // pay_out verifies on-chain that the invoice is Defaulted before moving
    // staked funds, so the pool needs the registry wired or it fails closed.
    ins.set_registry(&c.admin, &c.registry_id);
    c.rep.set_insurance(&c.admin, &insurance_id);

    // Past due plus the grace period, then default.
    env.ledger()
        .set_timestamp(PEN_DUE_DATE + invofi_common::GRACE_PERIOD_SECS + 1);
    c.rep.mark_overdue(&symbol_short!("inv_pen"));

    // Seven whole days elapsed, so a non-trivial penalty has accrued — the
    // test would be vacuous if this were zero.
    let accrued = c.rep.calculate_penalty(&symbol_short!("off_pen"));
    assert_eq!(accrued, 7 * PEN_PER_DAY);
    assert_eq!(tok.balance(&c.lender), 0);

    c.rep.reclaim_invoice(
        &symbol_short!("inv_pen"),
        &symbol_short!("off_pen"),
        &c.lender,
    );

    // The pool paid principal + yield only. ADR-0007 decision 8: penalty is a
    // punitive charge owed by the originator, not an insured credit loss, so
    // stakers do not fund it.
    assert_eq!(tok.balance(&c.lender), PEN_TOTAL_DUE);
    assert_eq!(ins.get_pool_total(), 3_000_000_000 - PEN_TOTAL_DUE);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_set_penalty_admin_only() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);

    let stranger = Address::generate(&env);
    c.rep.set_penalty(&stranger, &PEN_BPS, &PEN_CAP_BPS);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_set_penalty_rejects_excessive_rate() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);

    c.rep.set_penalty(&c.admin, &501u32, &PEN_CAP_BPS);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_set_penalty_rejects_excessive_cap() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);

    c.rep.set_penalty(&c.admin, &PEN_BPS, &10_001u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_set_penalty_blocked_while_paused() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(PEN_DUE_DATE - 2_592_000);
    let c = setup_penalty_case(&env);

    c.rep.pause(&c.admin);
    c.rep.set_penalty(&c.admin, &PEN_BPS, &PEN_CAP_BPS);
}
