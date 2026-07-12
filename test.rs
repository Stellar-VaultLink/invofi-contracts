#![cfg(test)]
extern crate std;

use super::{InvoiceRegistryContract, InvoiceStatus, OfferStatus, RiskTier};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

/// Deploys a test SEP-41 token, mints `amount` to `lender`, and approves the
/// given contract as spender for `amount` — the setup a real lender would do
/// before their offer can be accepted.
fn setup_token(env: &Env, contract_id: &Address, lender: &Address, amount: i128) -> Address {
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token_id = sac.address();

    let asset_client = token::StellarAssetClient::new(env, &token_id);
    asset_client.mint(lender, &amount);

    let token_client = token::TokenClient::new(env, &token_id);
    token_client.approve(lender, contract_id, &amount, &(env.ledger().sequence() + 1000));

    token_id
}

#[test]
fn test_register_and_get_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let invoice_id = symbol_short!("inv001");
    let amount: i128 = 1_000_000_000;
    let currency = symbol_short!("USDC");
    let due_date: u64 = 1_735_689_600;

    let registered = client.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &currency,
        &due_date,
    );

    assert_eq!(registered.id, invoice_id);
    assert_eq!(registered.originator, originator);
    assert_eq!(registered.amount, amount);
    assert_eq!(registered.currency, currency);
    assert_eq!(registered.due_date, due_date);
    assert_eq!(registered.status, InvoiceStatus::Pending);

    let fetched = client.get_invoice(&invoice_id);
    assert_eq!(fetched, registered);
}

#[test]
#[should_panic(expected = "Invoice with this ID already exists")]
fn test_duplicate_invoice_id_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let invoice_id = symbol_short!("dup001");
    let amount: i128 = 500_000_000;
    let currency = symbol_short!("XLM");
    let due_date: u64 = 1_735_689_600;

    client.register_invoice(&invoice_id, &originator, &amount, &currency, &due_date);
    client.register_invoice(&invoice_id, &originator, &amount, &currency, &due_date);
}

#[test]
#[should_panic(expected = "Invoice not found")]
fn test_get_non_existent_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    client.get_invoice(&symbol_short!("nope"));
}

#[test]
fn test_update_invoice_status() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let invoice_id = symbol_short!("inv002");

    client.register_invoice(
        &invoice_id,
        &originator,
        &(1_000_000_000i128),
        &symbol_short!("USDC"),
        &(1_735_689_600u64),
    );

    let updated = client.update_invoice_status(&invoice_id, &InvoiceStatus::Financed);
    assert_eq!(updated.status, InvoiceStatus::Financed);
}

#[test]
fn test_create_and_get_offer() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv003");
    let offer_id = symbol_short!("off001");

    client.register_invoice(
        &invoice_id,
        &originator,
        &(2_000_000_000i128),
        &symbol_short!("USDC"),
        &(1_735_689_600u64),
    );

    let offer = client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &(2_000_000_000i128),
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );

    assert_eq!(offer.id, offer_id);
    assert_eq!(offer.invoice_id, invoice_id);
    assert_eq!(offer.lender, lender);
    assert_eq!(offer.status, OfferStatus::Pending);
    assert_eq!(offer.funded_at, 0u64);

    let fetched = client.get_offer(&offer_id);
    assert_eq!(fetched.id, offer_id);
}

#[test]
fn test_accept_offer() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv004");
    let offer_id = symbol_short!("off002");
    let amount: i128 = 1_000_000_000;

    let token_id = setup_token(&env, &contract_id, &lender, amount);
    client.initialize(&admin, &token_id);

    client.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(1_735_689_600u64),
    );
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &300u32,
        &(1_296_000u64),
    );

    let accepted = client.accept_offer(&offer_id, &originator);
    assert_eq!(accepted.status, OfferStatus::Accepted);

    let invoice = client.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Financed);

    // Principal moved from lender to business immediately on acceptance.
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&lender), 0);
    assert_eq!(token_client.balance(&originator), amount);
}

#[test]
fn test_initialize_twice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);

    client.initialize(&admin, &token);
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_token(), token);

    let result = client.try_initialize(&admin, &token);
    assert!(result.is_err());
}

#[test]
fn test_reject_offer() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv005");
    let offer_id = symbol_short!("off003");

    client.register_invoice(
        &invoice_id,
        &originator,
        &(1_000_000_000i128),
        &symbol_short!("XLM"),
        &(1_735_689_600u64),
    );
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &(1_000_000_000i128),
        &symbol_short!("XLM"),
        &200u32,
        &(864_000u64),
    );

    let rejected = client.reject_offer(&offer_id, &originator);
    assert_eq!(rejected.status, OfferStatus::Rejected);

    // Invoice should remain Pending
    let invoice = client.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
}

#[test]
fn test_repay_invoice_partial_then_full() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv006");
    let offer_id = symbol_short!("off004");
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500; // 5.00%
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;

    let token_id = setup_token(&env, &contract_id, &lender, amount);
    client.initialize(&admin, &token_id);

    client.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(1_735_689_600u64),
    );
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &(2_592_000u64),
    );
    client.accept_offer(&offer_id, &originator);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&originator, &total_due);

    let partial_amount = amount / 2;
    let repaid = client.repay_invoice(&invoice_id, &offer_id, &originator, &partial_amount);
    assert_eq!(repaid.status, InvoiceStatus::Financed);

    let offer = client.get_offer(&offer_id);
    assert_eq!(offer.status, OfferStatus::Financed);
    assert_eq!(offer.amount_repaid, partial_amount);

    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&lender), partial_amount);
    assert_eq!(token_client.balance(&originator), amount + total_due - partial_amount);

    let final_amount = total_due - partial_amount;
    let repaid_final = client.repay_invoice(&invoice_id, &offer_id, &originator, &final_amount);
    assert_eq!(repaid_final.status, InvoiceStatus::Repaid);

    let settled_offer = client.get_offer(&offer_id);
    assert_eq!(settled_offer.status, OfferStatus::Repaid);
    assert_eq!(settled_offer.amount_repaid, total_due);
    assert_eq!(token_client.balance(&lender), total_due);
    assert_eq!(token_client.balance(&originator), amount);
}

#[test]
#[should_panic(expected = "Repayment amount exceeds remaining balance")]
fn test_repay_invoice_overpayment_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv010");
    let offer_id = symbol_short!("off008");
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500;
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;

    let token_id = setup_token(&env, &contract_id, &lender, amount);
    client.initialize(&admin, &token_id);

    client.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(1_735_689_600u64),
    );
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &(2_592_000u64),
    );
    client.accept_offer(&offer_id, &originator);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&originator, &total_due);

    client.repay_invoice(&invoice_id, &offer_id, &originator, &(total_due + 1));
}

#[test]
fn test_reclaim_invoice_after_grace_period() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv008");
    let offer_id = symbol_short!("off006");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let token_id = setup_token(&env, &contract_id, &lender, amount);
    client.initialize(&admin, &token_id);

    client.register_invoice(&invoice_id, &originator, &amount, &symbol_short!("USDC"), &due_date);
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    client.accept_offer(&offer_id, &originator);

    // Move past due_date + grace period.
    env.ledger().set_timestamp(due_date + super::GRACE_PERIOD_SECS + 1);
    client.mark_overdue(&invoice_id);

    let reclaimed = client.reclaim_invoice(&invoice_id, &offer_id, &lender);
    assert_eq!(reclaimed.status, OfferStatus::Defaulted);
}

#[test]
#[should_panic(expected = "Grace period has not elapsed")]
fn test_reclaim_before_grace_period_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv009");
    let offer_id = symbol_short!("off007");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let token_id = setup_token(&env, &contract_id, &lender, amount);
    client.initialize(&admin, &token_id);

    client.register_invoice(&invoice_id, &originator, &amount, &symbol_short!("USDC"), &due_date);
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    client.accept_offer(&offer_id, &originator);

    // Just past due_date, but not past the grace period yet.
    env.ledger().set_timestamp(due_date + 1);
    client.mark_overdue(&invoice_id);
    client.reclaim_invoice(&invoice_id, &offer_id, &lender);
}

#[test]
#[should_panic(expected = "Invoice must be Financed before repayment")]
fn test_repay_unfinanced_invoice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv007");
    let offer_id = symbol_short!("off005");

    client.register_invoice(
        &invoice_id,
        &originator,
        &(1_000_000_000i128),
        &symbol_short!("USDC"),
        &(1_735_689_600u64),
    );
    client.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &(1_000_000_000i128),
        &symbol_short!("USDC"),
        &500u32,
        &(2_592_000u64),
    );
    // Note: offer NOT accepted — invoice stays Pending
    client.repay_invoice(&invoice_id, &offer_id, &originator, &1);
}

#[test]
fn test_set_and_get_rate() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.set_rate(&admin, &RiskTier::A, &500u32);
    client.set_rate(&admin, &RiskTier::B, &800u32);
    client.set_rate(&admin, &RiskTier::C, &1200u32);

    assert_eq!(client.get_rate(&RiskTier::A), 500);
    assert_eq!(client.get_rate(&RiskTier::B), 800);
    assert_eq!(client.get_rate(&RiskTier::C), 1200);
}

#[test]
#[should_panic(expected = "rate_bps must be between 0 and 10000")]
fn test_set_rate_out_of_range_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.set_rate(&admin, &RiskTier::A, &10_001u32);
}

#[test]
#[should_panic(expected = "Only admin can set rates")]
fn test_set_rate_unauthorized_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let not_admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.set_rate(&not_admin, &RiskTier::A, &500u32);
}

#[test]
#[should_panic(expected = "Rate not set for this tier")]
fn test_get_unset_rate_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.get_rate(&RiskTier::A);
}

#[test]
fn test_transfer_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.transfer_admin(&admin, &new_admin);
    assert_eq!(client.get_admin(), new_admin);

    // New admin can now set rates; old admin can't.
    client.set_rate(&new_admin, &RiskTier::A, &500u32);
}

#[test]
#[should_panic(expected = "Only the current admin can transfer admin rights")]
fn test_transfer_admin_unauthorized_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let not_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.transfer_admin(&not_admin, &new_admin);
}

// ── Validation tests ────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "amount must be greater than zero")]
fn test_register_invoice_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_v1"),
        &originator,
        &0i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
}

#[test]
#[should_panic(expected = "due_date must be in the future")]
fn test_register_invoice_past_due_date() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(5_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_v2"),
        &originator,
        &1_000i128,
        &symbol_short!("USDC"),
        &1_000_000u64, // in the past relative to timestamp 5_000_000
    );
}

#[test]
#[should_panic(expected = "offer amount must be greater than zero")]
fn test_create_offer_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_v3"),
        &originator,
        &5_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_v1"),
        &symbol_short!("inv_v3"),
        &lender,
        &0i128, // zero amount — should panic
        &symbol_short!("USDC"),
        &500u32,
        &86_400u64,
    );
}

// ── Query helper tests ───────────────────────────────────────────────────

#[test]
fn test_get_invoices_by_status_empty() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let result = client.get_invoices_by_status(&InvoiceStatus::Pending);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_invoices_by_status_matching() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("q_inv_a"),
        &originator,
        &1_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.register_invoice(
        &symbol_short!("q_inv_b"),
        &originator,
        &2_000i128,
        &symbol_short!("XLM"),
        &4_000_000u64,
    );

    let pending = client.get_invoices_by_status(&InvoiceStatus::Pending);
    assert_eq!(pending.len(), 2);

    let financed = client.get_invoices_by_status(&InvoiceStatus::Financed);
    assert_eq!(financed.len(), 0);
}

// ── create_offer bounds tests ────────────────────────────────────────────────

#[test]
#[should_panic(expected = "interest_rate must be at most 10000 bps")]
fn test_create_offer_interest_rate_too_high_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_v4"),
        &originator,
        &1_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_v2"),
        &symbol_short!("inv_v4"),
        &lender,
        &1_000i128,
        &symbol_short!("USDC"),
        &10_001u32, // over 100%
        &86_400u64,
    );
}

#[test]
#[should_panic(expected = "duration must be at least 1 day (86400 seconds)")]
fn test_create_offer_short_duration_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_v5"),
        &originator,
        &1_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_v3"),
        &symbol_short!("inv_v5"),
        &lender,
        &1_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &3_600u64, // 1 hour — below MIN_OFFER_DURATION_SECS, should panic
    );
}

#[test]
#[should_panic(expected = "lender cannot finance their own invoice")]
fn test_create_offer_self_dealing_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_v6"),
        &originator,
        &1_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_v4"),
        &symbol_short!("inv_v6"),
        &originator, // lender == originator — should panic
        &1_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &86_400u64,
    );
}

#[test]
fn test_cancel_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_c1"),
        &originator,
        &1_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );

    let cancelled = client.cancel_invoice(&symbol_short!("inv_c1"), &originator);
    assert_eq!(cancelled.status, InvoiceStatus::Cancelled);
}

#[test]
#[should_panic(expected = "Only Pending invoices can be cancelled")]
fn test_cancel_non_pending_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token_id = setup_token(&env, &contract_id, &lender, 2_000i128);
    client.initialize(&originator, &token_id); // use originator as admin for simplicity

    client.register_invoice(
        &symbol_short!("inv_c2"),
        &originator,
        &1_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_c2"),
        &symbol_short!("inv_c2"),
        &lender,
        &1_000i128,
        &symbol_short!("XLM"),
        &500u32,
        &86_400u64,
    );
    client.accept_offer(&symbol_short!("off_c2"), &originator);
    // Invoice is now Financed — cancel should panic
    client.cancel_invoice(&symbol_short!("inv_c2"), &originator);
}

#[test]
fn test_get_offers_by_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_g1"),
        &originator,
        &5_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_g1a"),
        &symbol_short!("inv_g1"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );
    client.create_offer(
        &symbol_short!("off_g1b"),
        &symbol_short!("inv_g1"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &400u32,
        &86_400u64,
    );

    let offers = client.get_offers_by_invoice(&symbol_short!("inv_g1"));
    assert_eq!(offers.len(), 2);
}

#[test]
fn test_get_offers_by_lender() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let other = Address::generate(&env);

    client.register_invoice(
        &symbol_short!("inv_l1"),
        &originator,
        &1_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_l1"),
        &symbol_short!("inv_l1"),
        &lender,
        &1_000i128,
        &symbol_short!("XLM"),
        &200u32,
        &86_400u64,
    );
    client.create_offer(
        &symbol_short!("off_l2"),
        &symbol_short!("inv_l1"),
        &other,
        &1_000i128,
        &symbol_short!("XLM"),
        &300u32,
        &86_400u64,
    );

    let lender_offers = client.get_offers_by_lender(&lender);
    assert_eq!(lender_offers.len(), 1);
    assert_eq!(lender_offers.get(0).unwrap().lender, lender);
}

#[test]
fn test_calculate_total_due() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token_id = setup_token(&env, &contract_id, &lender, 10_000i128);
    client.initialize(&originator, &token_id);

    client.register_invoice(
        &symbol_short!("inv_d1"),
        &originator,
        &10_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_d1"),
        &symbol_short!("inv_d1"),
        &lender,
        &10_000i128,
        &symbol_short!("XLM"),
        &1_000u32, // 10%
        &86_400u64,
    );
    client.accept_offer(&symbol_short!("off_d1"), &originator);

    // principal=10000, yield=10000*1000/10000=1000, total_due=11000, repaid=0
    let due = client.calculate_total_due(&symbol_short!("off_d1"));
    assert_eq!(due, 11_000i128);
}

// ── Pause tests ──────────────────────────────────────────────────────────────

#[test]
fn test_pause_and_unpause() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    assert!(!client.contract_is_paused());
    client.pause(&admin);
    assert!(client.contract_is_paused());
    client.unpause(&admin);
    assert!(!client.contract_is_paused());
}

#[test]
#[should_panic(expected = "Contract is paused")]
fn test_register_invoice_while_paused_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.pause(&admin);
    client.register_invoice(
        &symbol_short!("inv_p1"),
        &originator,
        &1_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
}

#[test]
#[should_panic(expected = "Only admin can pause")]
fn test_pause_unauthorized_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let not_admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.pause(&not_admin);
}

// ── Protocol fee tests ───────────────────────────────────────────────────────

#[test]
fn test_set_and_get_fee() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    assert_eq!(client.get_fee(), 0);
    client.set_fee(&admin, &200u32); // 2%
    assert_eq!(client.get_fee(), 200);
}

#[test]
#[should_panic(expected = "fee_bps must be at most 500")]
fn test_set_fee_too_high_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    client.initialize(&admin, &token);

    client.set_fee(&admin, &600u32); // over 5% max
}

// ── withdraw_offer tests ────────────────────────────────────────────────────

#[test]
fn test_withdraw_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_w1"),
        &originator,
        &5_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_w1"),
        &symbol_short!("inv_w1"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );

    let withdrawn = client.withdraw_offer(&symbol_short!("off_w1"), &lender);
    assert_eq!(withdrawn.status, OfferStatus::Rejected);
}

#[test]
#[should_panic(expected = "Only the offer lender can withdraw")]
fn test_withdraw_offer_wrong_lender_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let other = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_w2"),
        &originator,
        &5_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    client.create_offer(
        &symbol_short!("off_w2"),
        &symbol_short!("inv_w2"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );
    client.withdraw_offer(&symbol_short!("off_w2"), &other);
}

// ── get_invoices_by_originator / get_all_invoices / get_all_offers tests ─────

#[test]
fn test_get_invoices_by_originator() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let orig_a = Address::generate(&env);
    let orig_b = Address::generate(&env);

    client.register_invoice(&symbol_short!("inv_oa1"), &orig_a, &1_000i128, &symbol_short!("XLM"), &3_000_000u64);
    client.register_invoice(&symbol_short!("inv_oa2"), &orig_a, &2_000i128, &symbol_short!("XLM"), &3_000_000u64);
    client.register_invoice(&symbol_short!("inv_ob1"), &orig_b, &3_000i128, &symbol_short!("XLM"), &3_000_000u64);

    let a_invoices = client.get_invoices_by_originator(&orig_a);
    assert_eq!(a_invoices.len(), 2);

    let b_invoices = client.get_invoices_by_originator(&orig_b);
    assert_eq!(b_invoices.len(), 1);
}

#[test]
fn test_get_all_invoices_and_offers() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let orig = Address::generate(&env);
    let lender = Address::generate(&env);

    client.register_invoice(&symbol_short!("inv_all1"), &orig, &1_000i128, &symbol_short!("XLM"), &3_000_000u64);
    client.register_invoice(&symbol_short!("inv_all2"), &orig, &2_000i128, &symbol_short!("XLM"), &3_000_000u64);
    client.create_offer(&symbol_short!("off_all1"), &symbol_short!("inv_all1"), &lender, &1_000i128, &symbol_short!("XLM"), &200u32, &86_400u64);

    assert_eq!(client.get_all_invoices().len(), 2);
    assert_eq!(client.get_all_offers().len(), 1);
}

// ── update_invoice_amount tests ──────────────────────────────────────────────

#[test]
fn test_update_invoice_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv_ua1"),
        &originator,
        &1_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );

    let updated = client.update_invoice_amount(&symbol_short!("inv_ua1"), &originator, &2_500i128);
    assert_eq!(updated.amount, 2_500i128);

    // Verify persistence
    let fetched = client.get_invoice(&symbol_short!("inv_ua1"));
    assert_eq!(fetched.amount, 2_500i128);
}

#[test]
#[should_panic(expected = "Only Pending invoices can have their amount updated")]
fn test_update_amount_on_financed_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token_id = setup_token(&env, &contract_id, &lender, 5_000i128);
    client.initialize(&originator, &token_id);

    client.register_invoice(&symbol_short!("inv_ua2"), &originator, &5_000i128, &symbol_short!("XLM"), &3_000_000u64);
    client.create_offer(&symbol_short!("off_ua2"), &symbol_short!("inv_ua2"), &lender, &5_000i128, &symbol_short!("XLM"), &300u32, &86_400u64);
    client.accept_offer(&symbol_short!("off_ua2"), &originator);

    // Invoice is now Financed — amount update should panic
    client.update_invoice_amount(&symbol_short!("inv_ua2"), &originator, &1_000i128);
}

// ── ProtocolStats tests ───────────────────────────────────────────────────────

#[test]
fn test_stats_increment_on_register_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let token_id = env.register(token::StellarAssetContract, ());
    client.initialize(&admin, &token_id);

    let stats_before = client.get_stats();
    assert_eq!(stats_before.total_invoices, 0);
    assert_eq!(stats_before.total_offers, 0);

    client.register_invoice(&symbol_short!("si1"), &admin, &1_000i128, &symbol_short!("XLM"), &2_000_000u64);
    client.register_invoice(&symbol_short!("si2"), &admin, &2_000i128, &symbol_short!("XLM"), &2_000_000u64);

    let stats_after = client.get_stats();
    assert_eq!(stats_after.total_invoices, 2);
    assert_eq!(stats_after.total_offers, 0);
}

#[test]
fn test_stats_increment_on_create_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let lender = Address::generate(&env);
    let token_id = setup_token(&env, &contract_id, &lender, 5_000i128);
    client.initialize(&admin, &token_id);

    client.register_invoice(&symbol_short!("so1"), &admin, &1_000i128, &symbol_short!("XLM"), &2_000_000u64);
    client.create_offer(&symbol_short!("off_so1"), &symbol_short!("so1"), &lender, &1_000i128, &symbol_short!("XLM"), &200u32, &86_400u64);

    let stats = client.get_stats();
    assert_eq!(stats.total_invoices, 1);
    assert_eq!(stats.total_offers, 1);
}

// ── Blacklist tests ───────────────────────────────────────────────────────────

#[test]
fn test_blacklist_and_unblacklist() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let token_id = env.register(token::StellarAssetContract, ());
    let bad_actor = Address::generate(&env);
    client.initialize(&admin, &token_id);

    // Not blacklisted initially
    assert!(!client.is_blacklisted(&bad_actor));

    // Blacklist
    client.blacklist_address(&admin, &bad_actor);
    assert!(client.is_blacklisted(&bad_actor));

    let list = client.get_blacklist();
    assert_eq!(list.len(), 1);

    // Idempotent — blacklisting again doesn't duplicate
    client.blacklist_address(&admin, &bad_actor);
    assert_eq!(client.get_blacklist().len(), 1);

    // Unblacklist
    client.unblacklist_address(&admin, &bad_actor);
    assert!(!client.is_blacklisted(&bad_actor));
    assert_eq!(client.get_blacklist().len(), 0);
}

#[test]
#[should_panic(expected = "Address is blacklisted")]
fn test_blacklisted_cannot_register_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let token_id = env.register(token::StellarAssetContract, ());
    let bad_actor = Address::generate(&env);
    client.initialize(&admin, &token_id);

    client.blacklist_address(&admin, &bad_actor);
    // Should panic
    client.register_invoice(&symbol_short!("bl1"), &bad_actor, &1_000i128, &symbol_short!("XLM"), &2_000_000u64);
}

#[test]
#[should_panic(expected = "Address is blacklisted")]
fn test_blacklisted_cannot_create_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let lender = Address::generate(&env);
    let token_id = setup_token(&env, &contract_id, &lender, 5_000i128);
    client.initialize(&admin, &token_id);

    client.register_invoice(&symbol_short!("bl2"), &admin, &1_000i128, &symbol_short!("XLM"), &2_000_000u64);
    client.blacklist_address(&admin, &lender);
    // Should panic
    client.create_offer(&symbol_short!("off_bl2"), &symbol_short!("bl2"), &lender, &1_000i128, &symbol_short!("XLM"), &200u32, &86_400u64);
}

#[test]
#[should_panic(expected = "Only admin can blacklist")]
fn test_blacklist_non_admin_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let token_id = env.register(token::StellarAssetContract, ());
    let non_admin = Address::generate(&env);
    let victim = Address::generate(&env);
    client.initialize(&admin, &token_id);

    // non_admin tries to blacklist — should panic
    client.blacklist_address(&non_admin, &victim);
}


// ─── Tests for new query functions and constants (v0.2) ──────────────────────

#[test]
#[should_panic(expected = "amount must be at least MIN_INVOICE_AMOUNT stroops")]
fn test_min_invoice_amount_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("tiny"),
        &originator,
        &1_i128,
        &symbol_short!("USDC"),
        &1_735_689_600_u64,
    );
}

#[test]
#[should_panic(expected = "duration must be at most 365 days")]
fn test_max_offer_duration_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv001"),
        &originator,
        &1_000_000_000_i128,
        &symbol_short!("USDC"),
        &1_735_689_600_u64,
    );
    client.create_offer(
        &symbol_short!("off001"),
        &symbol_short!("inv001"),
        &lender,
        &500_000_000_i128,
        &symbol_short!("USDC"),
        &500_u32,
        &31_622_400_u64,
    );
}

#[test]
fn test_get_invoices_and_offers_count() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    assert_eq!(client.get_invoices_count(), 0);
    assert_eq!(client.get_offers_count(), 0);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("i1"), &originator, &amount, &currency, &due_date);
    client.register_invoice(&symbol_short!("i2"), &originator, &amount, &currency, &due_date);
    assert_eq!(client.get_invoices_count(), 2);

    client.create_offer(
        &symbol_short!("o1"),
        &symbol_short!("i1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    assert_eq!(client.get_offers_count(), 1);
}

#[test]
fn test_get_offers_by_status_filters_correctly() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &due_date);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    client.create_offer(
        &symbol_short!("off2"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &400_u32,
        &86_400_u64,
    );

    let pending = client.get_offers_by_status(&OfferStatus::Pending);
    assert_eq!(pending.len(), 2);

    client.reject_offer(&symbol_short!("off1"), &originator);
    let still_pending = client.get_offers_by_status(&OfferStatus::Pending);
    assert_eq!(still_pending.len(), 1);
    let rejected = client.get_offers_by_status(&OfferStatus::Rejected);
    assert_eq!(rejected.len(), 1);
}

#[test]
fn test_get_invoices_by_currency_filters_correctly() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let usdc = symbol_short!("USDC");
    let xlm = symbol_short!("XLM");

    client.register_invoice(&symbol_short!("u1"), &originator, &amount, &usdc, &due_date);
    client.register_invoice(&symbol_short!("u2"), &originator, &amount, &usdc, &due_date);
    client.register_invoice(&symbol_short!("x1"), &originator, &amount, &xlm, &due_date);

    let usdc_invoices = client.get_invoices_by_currency(&usdc);
    let xlm_invoices = client.get_invoices_by_currency(&xlm);
    assert_eq!(usdc_invoices.len(), 2);
    assert_eq!(xlm_invoices.len(), 1);
}

#[test]
fn test_get_invoices_due_before_timestamp() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let currency = symbol_short!("USDC");

    env.ledger().set_timestamp(1000);
    client.register_invoice(&symbol_short!("soon"), &originator, &amount, &currency, &2000_u64);
    client.register_invoice(&symbol_short!("later"), &originator, &amount, &currency, &9999_u64);

    let early = client.get_invoices_due_before(&5000_u64);
    assert_eq!(early.len(), 1);
    assert_eq!(early.get(0).unwrap().id, symbol_short!("soon"));

    let all = client.get_invoices_due_before(&10000_u64);
    assert_eq!(all.len(), 2);
}

#[test]
fn test_get_pending_offers_by_invoice_excludes_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &due_date);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    client.create_offer(
        &symbol_short!("off2"),
        &symbol_short!("inv1"),
        &lender,
        &300_000_000_i128,
        &currency,
        &250_u32,
        &86_400_u64,
    );

    let pending = client.get_pending_offers_by_invoice(&symbol_short!("inv1"));
    assert_eq!(pending.len(), 2);

    client.reject_offer(&symbol_short!("off1"), &originator);
    let still_pending = client.get_pending_offers_by_invoice(&symbol_short!("inv1"));
    assert_eq!(still_pending.len(), 1);
    assert_eq!(still_pending.get(0).unwrap().id, symbol_short!("off2"));
}

#[test]
fn test_version_returns_nonempty_string() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let ver = client.version();
    assert!(ver.len() > 0);
}


// ─── Dispute and lender stats tests ─────────────────────────────────────────

#[test]
fn test_raise_dispute_changes_status_to_disputed() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &due_date);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );

    let token_id = setup_token(&env, &contract_id, &lender, 500_000_000_i128);
    client.initialize(&originator, &token_id);
    client.accept_offer(&symbol_short!("off1"), &originator);

    // Now raise dispute
    let disputed = client.raise_dispute(&symbol_short!("inv1"), &originator);
    assert_eq!(disputed.status, InvoiceStatus::Disputed);
}

#[test]
#[should_panic(expected = "Only Financed invoices can be disputed")]
fn test_raise_dispute_on_pending_invoice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    client.register_invoice(
        &symbol_short!("inv1"),
        &originator,
        &1_000_000_000_i128,
        &symbol_short!("USDC"),
        &1_735_689_600_u64,
    );
    // Pending — cannot be disputed
    client.raise_dispute(&symbol_short!("inv1"), &originator);
}

#[test]
fn test_resolve_dispute_restores_financed_status() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &1_735_689_600_u64);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    let token_id = setup_token(&env, &contract_id, &lender, 500_000_000_i128);
    client.initialize(&admin, &token_id);
    client.accept_offer(&symbol_short!("off1"), &originator);
    client.raise_dispute(&symbol_short!("inv1"), &originator);

    // Admin resolves back to Financed
    let resolved = client.resolve_dispute(
        &admin,
        &symbol_short!("inv1"),
        &InvoiceStatus::Financed,
    );
    assert_eq!(resolved.status, InvoiceStatus::Financed);
}

#[test]
fn test_get_lender_stats_after_create_offer() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let offer_amount: i128 = 500_000_000;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &1_735_689_600_u64);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &offer_amount,
        &currency,
        &300_u32,
        &86_400_u64,
    );

    let stats = client.get_lender_stats(&lender);
    assert_eq!(stats.total_offered, offer_amount);
    assert_eq!(stats.offers_pending, 1);
}


// ─── Pagination and batch tests ───────────────────────────────────────────────

#[test]
fn test_get_invoices_paginated() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let currency = symbol_short!("USDC");

    // Register 5 invoices
    for i in 0u32..5 {
        let id = soroban_sdk::symbol_short!(match i {
            0 => "i0", 1 => "i1", 2 => "i2", 3 => "i3", _ => "i4",
        });
        client.register_invoice(&id, &originator, &amount, &currency, &due_date);
    }

    // Page 1: offset 0, limit 3
    let page1 = client.get_invoices_paginated(&0_u32, &3_u32);
    assert_eq!(page1.len(), 3);

    // Page 2: offset 3, limit 3 (only 2 remaining)
    let page2 = client.get_invoices_paginated(&3_u32, &3_u32);
    assert_eq!(page2.len(), 2);
}

#[test]
fn test_get_offers_paginated() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 2_000_000_000;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &1_735_689_600_u64);

    // Create 4 offers
    for (id, rate) in [("o1", 100u32), ("o2", 200), ("o3", 300), ("o4", 400)] {
        let sym = soroban_sdk::symbol_short!(id);
        client.create_offer(
            &sym,
            &symbol_short!("inv1"),
            &lender,
            &100_000_000_i128,
            &currency,
            &rate,
            &86_400_u64,
        );
    }

    let page1 = client.get_offers_paginated(&0_u32, &2_u32);
    assert_eq!(page1.len(), 2);

    let page2 = client.get_offers_paginated(&2_u32, &2_u32);
    assert_eq!(page2.len(), 2);

    let page3 = client.get_offers_paginated(&4_u32, &2_u32);
    assert_eq!(page3.len(), 0);
}

#[test]
fn test_batch_get_invoices_skips_missing() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("real"), &originator, &amount, &currency, &1_735_689_600_u64);

    let mut ids = soroban_sdk::Vec::new(&env);
    ids.push_back(symbol_short!("real"));
    ids.push_back(symbol_short!("fake")); // does not exist

    let results = client.batch_get_invoices(&ids);
    // Only the real invoice should be returned
    assert_eq!(results.len(), 1);
    assert_eq!(results.get(0).unwrap().id, symbol_short!("real"));
}


// ─── Constant introspection and edge case tests ───────────────────────────────

#[test]
fn test_get_min_invoice_amount_matches_constant() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    assert_eq!(client.get_min_invoice_amount(), super::MIN_INVOICE_AMOUNT);
}

#[test]
fn test_get_offer_duration_limits() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);
    let (min, max) = client.get_offer_duration_limits();
    assert_eq!(min, super::MIN_OFFER_DURATION_SECS);
    assert_eq!(max, super::MAX_OFFER_DURATION_SECS);
    assert!(min < max);
}

#[test]
#[should_panic(expected = "Cannot resolve to Disputed status")]
fn test_resolve_dispute_to_disputed_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &1_000_000_000_i128, &currency, &1_735_689_600_u64);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    let token_id = setup_token(&env, &contract_id, &lender, 500_000_000_i128);
    client.initialize(&admin, &token_id);
    client.accept_offer(&symbol_short!("off1"), &originator);
    client.raise_dispute(&symbol_short!("inv1"), &originator);

    // Trying to resolve to Disputed is invalid
    client.resolve_dispute(&admin, &symbol_short!("inv1"), &InvoiceStatus::Disputed);
}

#[test]
fn test_get_invoices_due_before_excludes_repaid() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let currency = symbol_short!("USDC");

    env.ledger().set_timestamp(1000);

    // Invoice due at 2000 — will be Pending
    client.register_invoice(&symbol_short!("inv1"), &originator, &amount, &currency, &2000_u64);
    // Invoice due at 2000 — will be Financed then Repaid
    client.register_invoice(&symbol_short!("inv2"), &originator, &amount, &currency, &2000_u64);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv2"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    let token_id = setup_token(&env, &contract_id, &lender, 1_000_000_000_i128);
    client.initialize(&originator, &token_id);
    client.accept_offer(&symbol_short!("off1"), &originator);
    // Repay so inv2 moves to Repaid
    client.repay_invoice(&symbol_short!("inv2"), &symbol_short!("off1"), &originator, &515_000_000_i128);

    // Both have due_date=2000, but query at 5000 should only return inv1 (Pending)
    // inv2 is Repaid and should not appear
    let due = client.get_invoices_due_before(&5000_u64);
    assert_eq!(due.len(), 1);
    assert_eq!(due.get(0).unwrap().id, symbol_short!("inv1"));
}


// ─── Full lifecycle integration test ─────────────────────────────────────────

#[test]
fn test_full_invoice_financing_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let business = Address::generate(&env);
    let lender_a = Address::generate(&env);
    let lender_b = Address::generate(&env);
    let currency = symbol_short!("USDC");
    let due_date: u64 = 1_735_689_600;
    let invoice_amount: i128 = 5_000_000_000; // 500 USDC

    // ── 1. Setup ────────────────────────────────────────────────────────────
    let offer_a_amount: i128 = 4_000_000_000; // 400 USDC
    let offer_b_amount: i128 = 3_000_000_000; // 300 USDC
    let token_id = setup_token(&env, &contract_id, &lender_a, offer_a_amount + offer_b_amount);
    client.initialize(&admin, &token_id);
    // Also mint to lender_b
    let token_admin = Address::generate(&env);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&lender_b, &offer_b_amount);
    let token_client = token::TokenClient::new(&env, &token_id);
    token_client.approve(&lender_b, &contract_id, &offer_b_amount, &(env.ledger().sequence() + 1000));

    // ── 2. Business registers an invoice ────────────────────────────────────
    let inv = client.register_invoice(
        &symbol_short!("main_inv"),
        &business,
        &invoice_amount,
        &currency,
        &due_date,
    );
    assert_eq!(inv.status, InvoiceStatus::Pending);
    assert_eq!(client.get_invoices_count(), 1);

    // ── 3. Two lenders submit offers ────────────────────────────────────────
    client.create_offer(
        &symbol_short!("off_a"),
        &symbol_short!("main_inv"),
        &lender_a,
        &offer_a_amount,
        &currency,
        &500_u32, // 5%
        &86_400_u64,
    );
    client.create_offer(
        &symbol_short!("off_b"),
        &symbol_short!("main_inv"),
        &lender_b,
        &offer_b_amount,
        &currency,
        &300_u32, // 3%
        &86_400_u64,
    );
    assert_eq!(client.get_offers_count(), 2);

    let pending = client.get_pending_offers_by_invoice(&symbol_short!("main_inv"));
    assert_eq!(pending.len(), 2);

    // ── 4. Business rejects offer B and accepts offer A ─────────────────────
    client.reject_offer(&symbol_short!("off_b"), &business);
    client.accept_offer(&symbol_short!("off_a"), &business);

    let accepted_inv = client.get_invoice(&symbol_short!("main_inv"));
    assert_eq!(accepted_inv.status, InvoiceStatus::Financed);

    // ── 5. Business repays ──────────────────────────────────────────────────
    let total_due = client.calculate_total_due(&symbol_short!("off_a"));
    assert!(total_due > offer_a_amount); // principal + yield

    // Mint repayment tokens to business
    asset_client.mint(&business, &total_due);
    client.repay_invoice(
        &symbol_short!("main_inv"),
        &symbol_short!("off_a"),
        &business,
        &total_due,
    );

    let repaid_inv = client.get_invoice(&symbol_short!("main_inv"));
    assert_eq!(repaid_inv.status, InvoiceStatus::Repaid);

    // ── 6. Protocol stats ───────────────────────────────────────────────────
    let stats = client.get_stats();
    assert_eq!(stats.total_invoices, 1);
    assert_eq!(stats.total_offers, 2);
}


// ─── Blacklist and stats interaction tests ───────────────────────────────────

#[test]
fn test_stats_increment_in_full_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let currency = symbol_short!("USDC");
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;

    let stats0 = client.get_stats();
    assert_eq!(stats0.total_invoices, 0);
    assert_eq!(stats0.total_offers, 0);

    client.register_invoice(&symbol_short!("i1"), &originator, &amount, &currency, &due_date);
    client.register_invoice(&symbol_short!("i2"), &originator, &amount, &currency, &due_date);

    let stats1 = client.get_stats();
    assert_eq!(stats1.total_invoices, 2);

    client.create_offer(
        &symbol_short!("o1"),
        &symbol_short!("i1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    client.create_offer(
        &symbol_short!("o2"),
        &symbol_short!("i2"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );

    let stats2 = client.get_stats();
    assert_eq!(stats2.total_invoices, 2);
    assert_eq!(stats2.total_offers, 2);
}

#[test]
fn test_blacklisted_cannot_raise_dispute() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(InvoiceRegistryContract, ());
    let client = super::InvoiceRegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let currency = symbol_short!("USDC");

    client.register_invoice(&symbol_short!("inv1"), &originator, &1_000_000_000_i128, &currency, &1_735_689_600_u64);
    client.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000_i128,
        &currency,
        &300_u32,
        &86_400_u64,
    );
    let token_id = setup_token(&env, &contract_id, &lender, 500_000_000_i128);
    client.initialize(&admin, &token_id);
    client.accept_offer(&symbol_short!("off1"), &originator);

    // Blacklist the originator and verify is_blacklisted
    client.blacklist_address(&admin, &originator);
    assert!(client.is_blacklisted(&originator));

    // Unblacklist and verify removed
    client.unblacklist_address(&admin, &originator);
    assert!(!client.is_blacklisted(&originator));

    // Now the originator can dispute again (no longer blacklisted)
    let disputed = client.raise_dispute(&symbol_short!("inv1"), &originator);
    assert_eq!(disputed.status, super::InvoiceStatus::Disputed);
}
