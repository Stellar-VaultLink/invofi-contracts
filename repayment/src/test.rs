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
    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(env, &registry_id);
    reg.initialize(admin);

    // Financing
    let financing_id = env.register(FinancingContract, ());
    let fin = invofi_financing::FinancingContractClient::new(env, &financing_id);
    fin.initialize(admin, &registry_id, token);

    // Repayment
    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(env, &repayment_id);
    rep.initialize(admin, &registry_id, &financing_id, token);

    // Register repayment contract with financing (for authorized callbacks)
    fin.set_repayment_contract(admin, &repayment_id);

    // Register both contracts as trusted callers on the registry so the
    // cross-contract status transitions (accept + repay) are allowed.
    reg.set_repayment_contract(admin, &repayment_id);
    reg.set_financing_contract(admin, &financing_id);

    (reg, fin, rep)
}

/// Deploy a test SEP-41 token, mint `amount` to `lender`, and approve the
/// financing contract as spender.
fn setup_token(env: &Env, financing_id: &Address, lender: &Address, amount: i128) -> Address {
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token_id = sac.address();

    let asset_client = token::StellarAssetClient::new(env, &token_id);
    asset_client.mint(lender, &amount);

    let token_client = token::TokenClient::new(env, &token_id);
    token_client.approve(
        lender,
        financing_id,
        &amount,
        &(env.ledger().sequence() + 1000),
    );

    token_id
}

// ─── Full lifecycle tests ───────────────────────────────────────────────────

#[test]
fn test_repay_invoice_partial_then_full() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv001");
    let offer_id = symbol_short!("off001");
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500; // 5.00%
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;

    // Deploy all three contracts
    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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

    // Create and accept offer
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

    // Mint repayment funds to originator
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&originator, &total_due);

    // Partial repayment via Repayment contract
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

    // Full repayment
    let final_amount = total_due - partial_amount;
    let repaid_final = rep.repay_invoice(&invoice_id, &offer_id, &originator, &final_amount);
    assert_eq!(repaid_final.status, InvoiceStatus::Repaid);

    let settled_offer = fin.get_offer(&offer_id);
    assert_eq!(settled_offer.status, OfferStatus::Repaid);
    assert_eq!(settled_offer.amount_repaid, total_due);
    assert_eq!(token_client.balance(&lender), total_due);
}

// ─── Edge case tests ──────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Repayment amount exceeds remaining balance")]
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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
#[should_panic(expected = "Invoice must be Financed before repayment")]
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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
#[should_panic(expected = "Grace period has not elapsed")]
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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
#[should_panic(expected = "Invoice must be Overdue before reclaim")]
fn test_reclaim_on_non_overdue_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 3_000_000;

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, 10_000i128);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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

    // principal=10000, yield=10000*1000/10000=1000, total_due=11000, repaid=0
    let due = rep.calculate_total_due(&symbol_short!("off_td"));
    assert_eq!(due, 11_000i128);
}

#[test]
fn test_calculate_total_due_after_partial() {
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
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

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&originator, &total_due);

    // Partial repayment
    let partial = amount / 2;
    rep.repay_invoice(
        &symbol_short!("inv_tp"),
        &symbol_short!("off_tp"),
        &originator,
        &partial,
    );

    let remaining = rep.calculate_total_due(&symbol_short!("off_tp"));
    assert_eq!(remaining, total_due - partial);
}

// ─── Version test ──────────────────────────────────────────────────────────

#[test]
fn test_version_returns_nonempty_string() {
    let env = Env::default();
    env.mock_all_auths();
    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    let ver = rep.version();
    assert!(ver.len() > 0);
}

#[test]
fn test_get_duration_limits() {
    let env = Env::default();
    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    let (min, max) = rep.get_duration_limits();
    assert_eq!(min, invofi_common::MIN_OFFER_DURATION_SECS);
    assert_eq!(max, invofi_common::MAX_OFFER_DURATION_SECS);
}

// ─── Task 4A: emergency pause / circuit breaker ──────────────────────────────

#[test]
#[should_panic(expected = "Contract is paused")]
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);
    reg.set_financing_contract(&admin, &financing_id);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);

    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);

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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    // Insurance pool, funded by a third-party staker with the same token
    // the loan settles in (300M coverage against a 1.05B obligation).
    let insurance_id = env.register(InsuranceContract, ());
    let ins = invofi_insurance::InsuranceContractClient::new(&env, &insurance_id);
    ins.initialize(&admin, &token_id);
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

    // Reputation contract, recorder = repayment.
    let reputation_id = env.register(ReputationContract, ());
    let repu = invofi_reputation::ReputationContractClient::new(&env, &reputation_id);
    repu.initialize(&admin);
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

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);

    let fin = invofi_financing::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    let repayment_id = env.register(RepaymentContract, ());
    let rep = super::RepaymentContractClient::new(&env, &repayment_id);
    rep.initialize(&admin, &registry_id, &financing_id, &token_id);
    fin.set_repayment_contract(&admin, &repayment_id);
    reg.set_repayment_contract(&admin, &repayment_id);
    reg.set_financing_contract(&admin, &financing_id);

    let reputation_id = env.register(ReputationContract, ());
    let repu = invofi_reputation::ReputationContractClient::new(&env, &reputation_id);
    repu.initialize(&admin);
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

    // Originator repays principal + 5% yield in full.
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
