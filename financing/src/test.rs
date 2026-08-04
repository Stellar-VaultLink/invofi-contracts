#![cfg(test)]
extern crate std;

use super::FinancingContract;
use invofi_common::{InvoiceStatus, OfferStatus};
use invofi_registry::RegistryContract;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

/// Deploy the registry + financing contracts and return both clients.
/// The registry is initialized with `admin`; financing is wired to the
/// registry address and the SEP-41 `token`.
fn setup_contracts<'a>(
    env: &'a Env,
    admin: &Address,
    token: &Address,
) -> (
    invofi_registry::RegistryContractClient<'a>,
    super::FinancingContractClient<'a>,
) {
    let registry_id = env.register(RegistryContract, ());
    let registry_client = invofi_registry::RegistryContractClient::new(env, &registry_id);
    registry_client.initialize(admin);

    let financing_id = env.register(FinancingContract, ());
    let financing_client = super::FinancingContractClient::new(env, &financing_id);
    financing_client.initialize(admin, &registry_id, token);

    // Register financing as a trusted caller on the registry so its
    // cross-contract status transition (Pending -> Financed) is allowed.
    registry_client.set_financing_contract(admin, &financing_id);

    (registry_client, financing_client)
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

// ─── Offer CRUD tests ────────────────────────────────────────────────────────

#[test]
fn test_create_and_get_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv003");
    let offer_id = symbol_short!("off001");

    reg.register_invoice(
        &invoice_id,
        &originator,
        &(2_000_000_000i128),
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );

    let offer = fin.create_offer(
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

    let fetched = fin.get_offer(&offer_id);
    assert_eq!(fetched.id, offer_id);
}

#[test]
#[should_panic(expected = "offer amount must be greater than zero")]
fn test_create_offer_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    reg.register_invoice(
        &symbol_short!("inv_v3"),
        &originator,
        &50_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_v1"),
        &symbol_short!("inv_v3"),
        &lender,
        &0i128,
        &symbol_short!("USDC"),
        &500u32,
        &86_400u64,
    );
}

#[test]
#[should_panic(expected = "interest_rate must be at most 10000 bps")]
fn test_create_offer_interest_rate_too_high_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    reg.register_invoice(
        &symbol_short!("inv_v4"),
        &originator,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_v2"),
        &symbol_short!("inv_v4"),
        &lender,
        &1_000i128,
        &symbol_short!("USDC"),
        &10_001u32,
        &86_400u64,
    );
}

#[test]
#[should_panic(expected = "duration must be at least 1 day (86400 seconds)")]
fn test_create_offer_short_duration_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    reg.register_invoice(
        &symbol_short!("inv_v5"),
        &originator,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_v3"),
        &symbol_short!("inv_v5"),
        &lender,
        &1_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &3_600u64,
    );
}

#[test]
#[should_panic(expected = "duration must be at most 365 days")]
fn test_max_offer_duration_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    reg.register_invoice(
        &symbol_short!("inv001"),
        &originator,
        &1_000_000_000_i128,
        &symbol_short!("USDC"),
        &1_735_689_600_u64,
    );
    fin.create_offer(
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
#[should_panic(expected = "lender cannot finance their own invoice")]
fn test_create_offer_self_dealing_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    reg.register_invoice(
        &symbol_short!("inv_v6"),
        &originator,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_v4"),
        &symbol_short!("inv_v6"),
        &originator, // lender == originator
        &1_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &86_400u64,
    );
}

#[test]
#[should_panic(expected = "Address is blacklisted")]
fn test_blacklisted_cannot_create_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = setup_token(&env, &Address::generate(&env), &lender, 5_000i128);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("bl2"),
        &admin,
        &10_000_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
    // Blacklist on registry, then try to create offer on financing
    reg.blacklist_address(&admin, &lender);
    fin.create_offer(
        &symbol_short!("off_bl2"),
        &symbol_short!("bl2"),
        &lender,
        &1_000i128,
        &symbol_short!("XLM"),
        &200u32,
        &86_400u64,
    );
}

// ─── Accept / Reject tests ──────────────────────────────────────────────────

#[test]
fn test_accept_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("inv004");
    let offer_id = symbol_short!("off002");
    let amount: i128 = 1_000_000_000;

    let financing_id = env.register(FinancingContract, ());
    let token_id = setup_token(&env, &financing_id, &lender, amount);

    let registry_id = env.register(RegistryContract, ());
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);
    reg.initialize(&admin);
    reg.set_financing_contract(&admin, &financing_id);

    let fin = super::FinancingContractClient::new(&env, &financing_id);
    fin.initialize(&admin, &registry_id, &token_id);

    reg.register_invoice(
        &invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(3_000_000u64),
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &300u32,
        &(1_296_000u64),
    );

    let accepted = fin.accept_offer(&offer_id, &originator);
    assert_eq!(accepted.status, OfferStatus::Accepted);

    // Invoice should now be Financed in registry
    let invoice = reg.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Financed);

    // Principal moved from lender to business
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&lender), 0);
    assert_eq!(token_client.balance(&originator), amount);
}

#[test]
fn test_reject_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let invoice_id = symbol_short!("inv005");
    let offer_id = symbol_short!("off003");

    reg.register_invoice(
        &invoice_id,
        &originator,
        &(1_000_000_000i128),
        &symbol_short!("XLM"),
        &(3_000_000u64),
    );
    fin.create_offer(
        &offer_id,
        &invoice_id,
        &lender,
        &(1_000_000_000i128),
        &symbol_short!("XLM"),
        &200u32,
        &(864_000u64),
    );

    let rejected = fin.reject_offer(&offer_id, &originator);
    assert_eq!(rejected.status, OfferStatus::Rejected);

    let invoice = reg.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
}

#[test]
fn test_withdraw_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_w1"),
        &originator,
        &50_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_w1"),
        &symbol_short!("inv_w1"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );

    let withdrawn = fin.withdraw_offer(&symbol_short!("off_w1"), &lender);
    assert_eq!(withdrawn.status, OfferStatus::Rejected);
}

#[test]
#[should_panic(expected = "Only the offer lender can withdraw")]
fn test_withdraw_offer_wrong_lender_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let other = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_w2"),
        &originator,
        &50_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_w2"),
        &symbol_short!("inv_w2"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );
    fin.withdraw_offer(&symbol_short!("off_w2"), &other);
}

// ─── Query helper tests ───────────────────────────────────────────────────

#[test]
fn test_get_offers_by_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_g1"),
        &originator,
        &50_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_g1a"),
        &symbol_short!("inv_g1"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );
    fin.create_offer(
        &symbol_short!("off_g1b"),
        &symbol_short!("inv_g1"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &400u32,
        &86_400u64,
    );

    let offers = fin.get_offers_by_invoice(&symbol_short!("inv_g1"));
    assert_eq!(offers.len(), 2);
}

#[test]
fn test_get_offers_by_lender() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let other = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_l1"),
        &originator,
        &10_000_000i128,
        &symbol_short!("XLM"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_l1"),
        &symbol_short!("inv_l1"),
        &lender,
        &1_000i128,
        &symbol_short!("XLM"),
        &200u32,
        &86_400u64,
    );
    fin.create_offer(
        &symbol_short!("off_l2"),
        &symbol_short!("inv_l1"),
        &other,
        &1_000i128,
        &symbol_short!("XLM"),
        &300u32,
        &86_400u64,
    );

    let lender_offers = fin.get_offers_by_lender(&lender);
    assert_eq!(lender_offers.len(), 1);
    assert_eq!(lender_offers.get(0).unwrap().lender, lender);
}

#[test]
fn test_get_offers_count() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    assert_eq!(fin.get_offers_count(), 0);

    reg.register_invoice(
        &symbol_short!("i1"),
        &originator,
        &1_000_000_000_i128,
        &symbol_short!("USDC"),
        &1_735_689_600_u64,
    );
    fin.create_offer(
        &symbol_short!("o1"),
        &symbol_short!("i1"),
        &lender,
        &500_000_000_i128,
        &symbol_short!("USDC"),
        &300_u32,
        &86_400_u64,
    );
    assert_eq!(fin.get_offers_count(), 1);
}

#[test]
fn test_get_offers_by_status_filters_correctly() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv1"),
        &originator,
        &1_000_000_000i128,
        &symbol_short!("USDC"),
        &1_735_689_600u64,
    );
    fin.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );
    fin.create_offer(
        &symbol_short!("off2"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000i128,
        &symbol_short!("USDC"),
        &400u32,
        &86_400u64,
    );

    let pending = fin.get_offers_by_status(&OfferStatus::Pending);
    assert_eq!(pending.len(), 2);

    fin.reject_offer(&symbol_short!("off1"), &originator);
    let still_pending = fin.get_offers_by_status(&OfferStatus::Pending);
    assert_eq!(still_pending.len(), 1);
    let rejected = fin.get_offers_by_status(&OfferStatus::Rejected);
    assert_eq!(rejected.len(), 1);
}

#[test]
fn test_get_pending_offers_by_invoice_excludes_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv1"),
        &originator,
        &1_000_000_000i128,
        &symbol_short!("USDC"),
        &1_735_689_600u64,
    );
    fin.create_offer(
        &symbol_short!("off1"),
        &symbol_short!("inv1"),
        &lender,
        &500_000_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );
    fin.create_offer(
        &symbol_short!("off2"),
        &symbol_short!("inv1"),
        &lender,
        &300_000_000i128,
        &symbol_short!("USDC"),
        &250u32,
        &86_400u64,
    );

    let pending = fin.get_pending_offers_by_invoice(&symbol_short!("inv1"));
    assert_eq!(pending.len(), 2);

    fin.reject_offer(&symbol_short!("off1"), &originator);
    let still_pending = fin.get_pending_offers_by_invoice(&symbol_short!("inv1"));
    assert_eq!(still_pending.len(), 1);
    assert_eq!(still_pending.get(0).unwrap().id, symbol_short!("off2"));
}

#[test]
fn test_get_offers_paginated() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv1"),
        &originator,
        &2_000_000_000i128,
        &symbol_short!("USDC"),
        &1_735_689_600u64,
    );

    for (id, rate) in [("o1", 100u32), ("o2", 200), ("o3", 300), ("o4", 400)] {
        let sym = soroban_sdk::Symbol::new(&env, id);
        fin.create_offer(
            &sym,
            &symbol_short!("inv1"),
            &lender,
            &100_000_000i128,
            &symbol_short!("USDC"),
            &rate,
            &86_400u64,
        );
    }

    let page1 = fin.get_offers_paginated(&0_u32, &2_u32);
    assert_eq!(page1.len(), 2);

    let page2 = fin.get_offers_paginated(&2_u32, &2_u32);
    assert_eq!(page2.len(), 2);

    let page3 = fin.get_offers_paginated(&4_u32, &2_u32);
    assert_eq!(page3.len(), 0);
}

// ─── Cross-contract callback tests ────────────────────────────────────────

#[test]
fn test_update_offer_status_and_amount_repaid() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_cr"),
        &originator,
        &50_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_cr"),
        &symbol_short!("inv_cr"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );

    // Register a fake repayment contract (auth is mocked)
    let repayment_id = env.register(super::FinancingContract, ());
    fin.set_repayment_contract(&admin, &repayment_id);

    // Simulate repayment callback
    fin.update_offer_status(&symbol_short!("off_cr"), &OfferStatus::Financed);
    fin.update_offer_amount_repaid(&symbol_short!("off_cr"), &2_500i128);

    let offer = fin.get_offer(&symbol_short!("off_cr"));
    assert_eq!(offer.status, OfferStatus::Financed);
    assert_eq!(offer.amount_repaid, 2_500i128);
}

#[test]
fn test_update_lender_stats_repaid() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    reg.register_invoice(
        &symbol_short!("inv_ls"),
        &originator,
        &50_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    fin.create_offer(
        &symbol_short!("off_ls"),
        &symbol_short!("inv_ls"),
        &lender,
        &5_000i128,
        &symbol_short!("USDC"),
        &300u32,
        &86_400u64,
    );

    let stats_before = fin.get_lender_stats(&lender);
    assert_eq!(stats_before.offers_repaid, 0);

    // Register a fake repayment contract (auth is mocked)
    let repayment_id = env.register(super::FinancingContract, ());
    fin.set_repayment_contract(&admin, &repayment_id);

    fin.update_lender_stats_repaid(&lender, &true);

    let stats_after = fin.get_lender_stats(&lender);
    assert_eq!(stats_after.offers_repaid, 1);
}

#[test]
fn test_update_stats_repaid_and_get_fee_bps() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_, fin) = setup_contracts(&env, &admin, &token);

    // Default fee_bps is 0
    assert_eq!(fin.get_fee_bps(), 0);

    // Register a fake repayment contract (auth is mocked)
    let repayment_id = env.register(super::FinancingContract, ());
    fin.set_repayment_contract(&admin, &repayment_id);

    // Simulate stats update
    fin.update_stats_repaid(&1_000_000i128, &50_000i128);
    let stats = fin.get_stats();
    assert_eq!(stats.total_repaid, 1_000_000i128);
    assert_eq!(stats.total_fee_revenue, 50_000i128);
}

// ─── Version test ─────────────────────────────────────────────────────────────

#[test]
fn test_version_returns_nonempty_string() {
    let env = Env::default();
    env.mock_all_auths();
    let financing_id = env.register(FinancingContract, ());
    let fin = super::FinancingContractClient::new(&env, &financing_id);
    let ver = fin.version();
    assert!(ver.len() > 0);
}

#[test]
fn test_get_offer_duration_limits() {
    let env = Env::default();
    let financing_id = env.register(FinancingContract, ());
    let fin = super::FinancingContractClient::new(&env, &financing_id);
    let (min, max) = fin.get_offer_duration_limits();
    assert_eq!(min, invofi_common::MIN_OFFER_DURATION_SECS);
    assert_eq!(max, invofi_common::MAX_OFFER_DURATION_SECS);
}

// ─── Task 4A: emergency pause / circuit breaker ──────────────────────────────

#[test]
#[should_panic(expected = "Contract is paused")]
fn test_pause_blocks_create_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (reg, fin) = setup_contracts(&env, &admin, &token);

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("invp1");
    reg.register_invoice(
        &invoice_id,
        &originator,
        &2_000_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );

    fin.pause(&admin);
    fin.create_offer(
        &symbol_short!("offp1"),
        &invoice_id,
        &lender,
        &2_000_000_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
    );
}
