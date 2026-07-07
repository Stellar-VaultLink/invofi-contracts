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
    assert_eq!(token_client.balance(&originator), total_due - partial_amount);

    let final_amount = total_due - partial_amount;
    let repaid_final = client.repay_invoice(&invoice_id, &offer_id, &originator, &final_amount);
    assert_eq!(repaid_final.status, InvoiceStatus::Repaid);

    let settled_offer = client.get_offer(&offer_id);
    assert_eq!(settled_offer.status, OfferStatus::Repaid);
    assert_eq!(settled_offer.amount_repaid, total_due);
    assert_eq!(token_client.balance(&lender), total_due);
    assert_eq!(token_client.balance(&originator), 0);
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
#[should_panic(expected = "duration must be greater than zero")]
fn test_create_offer_zero_duration_panics() {
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
        &0u64, // zero duration — should panic
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
