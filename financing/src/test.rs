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
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry_client = invofi_registry::RegistryContractClient::new(env, &registry_id);

    let financing_id =
        env.register(FinancingContract, (admin.clone(), registry_id.clone(), token.clone()));
    let financing_client = super::FinancingContractClient::new(env, &financing_id);

    // Register financing as a trusted caller on the registry so its
    // cross-contract status transition (Pending -> Financed) is allowed.
    registry_client.set_financing_contract(admin, &financing_id);

    (registry_client, financing_client)
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
#[should_panic(expected = "Error(Contract, #8)")]
fn test_blacklisted_cannot_create_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let lender = Address::generate(&env);
    let token = create_token(&env);
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

    let token_id = create_token(&env);
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);

    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), registry_id.clone(), token_id.clone()),
    );
    let fin = super::FinancingContractClient::new(&env, &financing_id);

    reg.set_financing_contract(&admin, &financing_id);
    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);

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
#[should_panic(expected = "Error(Contract, #1)")]
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
    let repayment_id = env.register(
        super::FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
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
    let repayment_id = env.register(
        super::FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
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
    let repayment_id = env.register(
        super::FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
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
    let financing_id = env.register(
        FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    let fin = super::FinancingContractClient::new(&env, &financing_id);
    let ver = fin.version();
    assert!(!ver.is_empty());
}

#[test]
fn test_get_offer_duration_limits() {
    let env = Env::default();
    let financing_id = env.register(
        FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    let fin = super::FinancingContractClient::new(&env, &financing_id);
    let (min, max) = fin.get_offer_duration_limits();
    assert_eq!(min, invofi_common::MIN_OFFER_DURATION_SECS);
    assert_eq!(max, invofi_common::MAX_OFFER_DURATION_SECS);
}

// ─── Task 4A: emergency pause / circuit breaker ──────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
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

#[test]
fn test_pause_blocks_all_financing_state_changes() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_, fin) = setup_contracts(&env, &admin, &token);
    let lender = Address::generate(&env);
    let originator = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let repayment = Address::generate(&env);
    let pos_token = Address::generate(&env);

    fin.pause(&admin);

    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(result.is_err(), "state-changing function should panic while paused");
    }

    assert_paused(|| {
        fin.create_offer(
            &symbol_short!("offx1"),
            &symbol_short!("invx1"),
            &lender,
            &1_000i128,
            &symbol_short!("USDC"),
            &500u32,
            &86_400u64,
        );
    });
    assert_paused(|| {
        fin.withdraw_offer(&symbol_short!("offx2"), &lender);
    });
    assert_paused(|| {
        fin.accept_offer(&symbol_short!("offx3"), &originator);
    });
    assert_paused(|| {
        fin.reject_offer(&symbol_short!("offx4"), &originator);
    });
    assert_paused(|| {
        fin.set_repayment_contract(&admin, &repayment);
    });
    assert_paused(|| {
        fin.transfer_admin(&admin, &new_admin);
    });
    assert_paused(|| {
        fin.register_currency(&admin, &symbol_short!("EUR"), &Address::generate(&env));
    });
    assert_paused(|| {
        fin.set_position_token(&admin, &pos_token);
    });
    assert_paused(|| {
        fin.update_offer_status(&symbol_short!("offx5"), &OfferStatus::Rejected);
    });
    assert_paused(|| {
        fin.update_offer_amount_repaid(&symbol_short!("offx6"), &1_000i128);
    });
    assert_paused(|| {
        fin.update_lender_stats_repaid(&lender, &true);
    });
    assert_paused(|| {
        fin.update_stats_repaid(&1_000i128, &50i128);
    });

    assert_eq!(fin.get_offer_duration_limits().0, invofi_common::MIN_OFFER_DURATION_SECS);
    assert_eq!(fin.get_stats().total_offers, 0);
}

// ─── Position token minting tests (Task 7) ──────────────────────────────────

#[test]
fn test_accept_offer_mints_position_token() {
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
    let fin = super::FinancingContractClient::new(&env, &financing_id);

    // The position token's admin is the financing contract, so accept_offer
    // can mint claim tokens on the lender's behalf (ADR-0002).
    let pos_sac = env.register_stellar_asset_contract_v2(financing_id.clone());
    let pos_token_id = pos_sac.address();

    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    reg.set_financing_contract(&admin, &financing_id);
    assert!(fin.get_position_token().is_none());
    fin.set_position_token(&admin, &pos_token_id);
    assert_eq!(fin.get_position_token(), Some(pos_token_id.clone()));

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
        &300u32,
        &1_296_000u64,
    );

    fin.accept_offer(&offer_id, &originator);

    // DoD: lender's position-token balance equals the offer amount.
    let pos_client = token::TokenClient::new(&env, &pos_token_id);
    assert_eq!(pos_client.balance(&lender), amount);
    // And the principal currency is untouched (already moved to originator).
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&lender), 0);
    assert_eq!(token_client.balance(&originator), amount);
}

#[test]
fn test_accept_offer_without_position_token_still_works() {
    // Backward compatibility: a deployment without a configured position
    // token keeps financing working exactly as before (no mint attempted).
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let originator = Address::generate(&env);
    let lender = Address::generate(&env);
    let invoice_id = symbol_short!("invp3");
    let offer_id = symbol_short!("offp3");
    let amount: i128 = 1_000_000_000;

    let token_id = create_token(&env);
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(&env, &registry_id);

    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), registry_id.clone(), token_id.clone()),
    );
    let fin = super::FinancingContractClient::new(&env, &financing_id);

    reg.set_financing_contract(&admin, &financing_id);
    mint_and_approve(&env, &token_id, &financing_id, &lender, amount);
    assert!(fin.get_position_token().is_none());

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
        &300u32,
        &1_296_000u64,
    );

    let accepted = fin.accept_offer(&offer_id, &originator);
    assert_eq!(accepted.status, OfferStatus::Accepted);

    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&originator), amount);
}

// ─── Repayment schedule tests (issue #133) ──────────────────────────────────

/// Helper: create a pending offer on a fresh invoice and return the offer id,
/// invoice id, originator and lender addresses.
fn setup_offer_for_schedule<'a>(
    env: &'a Env,
    admin: &Address,
    token: &Address,
) -> (
    invofi_registry::RegistryContractClient<'a>,
    super::FinancingContractClient<'a>,
    Address, // originator
    Address, // lender
) {
    let (reg, fin) = setup_contracts(env, admin, token);
    let originator = Address::generate(env);
    let lender = Address::generate(env);

    reg.register_invoice(
        &symbol_short!("inv_sc1"),
        &originator,
        &1_200_000_000i128, // 1.2 billion units (12 installments of 100M each)
        &symbol_short!("USDC"),
        &(env.ledger().timestamp() + 100_000_000u64),
    );
    fin.create_offer(
        &symbol_short!("off_sc1"),
        &symbol_short!("inv_sc1"),
        &lender,
        &1_200_000_000i128,
        &symbol_short!("USDC"),
        &500u32, // 5.00% per installment slice
        &(31_536_000u64),
    );

    (reg, fin, originator, lender)
}

/// Acceptance criterion 1: schedule created and readable.
#[test]
fn test_schedule_created_and_readable() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    // No schedule yet
    assert!(fin.get_schedule(&symbol_short!("off_sc1")).is_none());

    let first_due = env.ledger().timestamp() + 604_800; // +1 week
    let sched = fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &12u32,
        &first_due,
    );

    assert_eq!(sched.count, 12);
    assert_eq!(sched.frequency, invofi_common::ScheduleFrequency::Weekly);
    assert_eq!(sched.first_due, first_due);

    let fetched = fin.get_schedule(&symbol_short!("off_sc1"));
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().count, 12);
}

/// Acceptance criterion 2: installment math matches the documented model.
///
/// offer.amount = 1 200 000 000, count = 12, interest_rate = 500 bps (5%)
/// installment_principal = 1_200_000_000 / 12 = 100_000_000
/// installment_yield     = 100_000_000 * 500 / 10_000 = 5_000_000
/// installment_amount    = 105_000_000
#[test]
fn test_installment_math_matches_documented_model() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    let first_due = env.ledger().timestamp() + 604_800;
    let sched = fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &12u32,
        &first_due,
    );

    // Documented model: principal_slice + yield_on_slice
    let expected_principal_slice = 1_200_000_000i128 / 12;       // 100_000_000
    let expected_yield = expected_principal_slice * 500 / 10_000;  // 5_000_000
    let expected_installment = expected_principal_slice + expected_yield; // 105_000_000

    assert_eq!(sched.installment_amount, expected_installment);
}

/// Acceptance criterion 3: off-schedule (ad-hoc) payment still permitted
/// without corrupting state.
#[test]
fn test_off_schedule_repayment_does_not_corrupt_state() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    let first_due = env.ledger().timestamp() + 604_800;
    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &12u32,
        &first_due,
    );

    // Register a fake repayment contract (auth is mocked for callbacks)
    let repayment_id = env.register(
        super::FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    fin.set_repayment_contract(&admin, &repayment_id);

    // Simulate an ad-hoc off-schedule partial repayment (42M — not a multiple
    // of installment_amount = 105M). State should remain readable and correct.
    fin.update_offer_amount_repaid(&symbol_short!("off_sc1"), &42_000_000i128);

    let offer = fin.get_offer(&symbol_short!("off_sc1"));
    assert_eq!(offer.amount_repaid, 42_000_000i128);

    // Schedule is still intact — state not corrupted.
    let sched = fin.get_schedule(&symbol_short!("off_sc1")).unwrap();
    assert_eq!(sched.count, 12);
    assert_eq!(sched.installment_amount, 105_000_000i128);
}

/// Lender can also create the schedule on a Pending offer.
#[test]
fn test_lender_can_schedule_on_pending_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, _originator, lender) = setup_offer_for_schedule(&env, &admin, &token);

    let first_due = env.ledger().timestamp() + 86_400;
    let sched = fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &lender,
        &invofi_common::ScheduleFrequency::Monthly,
        &4u32,
        &first_due,
    );

    assert_eq!(sched.count, 4);
    assert_eq!(sched.frequency, invofi_common::ScheduleFrequency::Monthly);
}

/// get_installment_due returns 0 before the first_due timestamp.
#[test]
fn test_get_installment_due_returns_zero_before_first_due() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    let first_due = env.ledger().timestamp() + 604_800; // 1 week in the future
    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &4u32,
        &first_due,
    );

    // Now is before first_due — nothing due yet.
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 0);
}

/// get_installment_due returns 1 after the first_due timestamp.
#[test]
fn test_get_installment_due_returns_one_after_first_due() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    let first_due = env.ledger().timestamp() + 604_800;
    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &4u32,
        &first_due,
    );

    // Advance time past first_due but before second installment.
    env.ledger().set_timestamp(first_due + 1);
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 1);
}

/// get_installment_due returns the first unpaid elapsed installment (1).
/// Even when 2 periods have elapsed, if installment 1 is still unpaid,
/// the helper returns 1 — the caller should pay installment 1 first.
#[test]
fn test_get_installment_due_advances_over_time() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    let first_due = env.ledger().timestamp() + 604_800;
    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &4u32,
        &first_due,
    );

    // Advance past the second weekly period — 2 installments elapsed, 0 paid.
    // The helper returns 1 (the first unpaid due installment).
    env.ledger().set_timestamp(first_due + 604_800 + 1);
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 1);
}

/// get_installment_due returns 0 when all installments are covered.
#[test]
fn test_get_installment_due_zero_when_all_paid() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    // Schedule with 4 weekly installments.
    // offer.amount = 1_200_000_000, count = 4, rate = 500 bps
    // installment_principal = 1_200_000_000 / 4 = 300_000_000
    // installment_yield     = 300_000_000 * 500 / 10_000 = 15_000_000
    // installment_amount    = 315_000_000
    // 4 installments fully paid = 4 × 315_000_000 = 1_260_000_000
    let first_due = env.ledger().timestamp() + 604_800;
    let sched = fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &4u32,
        &first_due,
    );
    let full_paid = sched.installment_amount * 4;

    // Register a fake repayment contract (auth is mocked)
    let repayment_id = env.register(
        super::FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    fin.set_repayment_contract(&admin, &repayment_id);

    // Simulate full coverage: 4 installments × installment_amount.
    fin.update_offer_amount_repaid(&symbol_short!("off_sc1"), &full_paid);

    // Advance past all 4 periods.
    env.ledger().set_timestamp(first_due + 4 * 604_800);

    // Nothing is due — all installments covered.
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 0);
}

/// get_installment_due returns 0 when no schedule exists.
#[test]
fn test_get_installment_due_returns_zero_with_no_schedule() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, _originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    // No schedule set — must return 0.
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 0);
}

/// schedule_repayment panics for a count of zero.
#[test]
#[should_panic(expected = "count must be at least 1")]
fn test_schedule_count_zero_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &0u32,
        &(env.ledger().timestamp() + 604_800),
    );
}

/// schedule_repayment panics when first_due is in the past.
#[test]
#[should_panic(expected = "first_due must be in the future")]
fn test_schedule_first_due_in_past_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Weekly,
        &4u32,
        &999_999u64, // before current timestamp
    );
}

/// A third party (neither lender nor originator) cannot set a schedule.
#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_schedule_unauthorized_caller_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, _originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    let intruder = Address::generate(&env);
    fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &intruder,
        &invofi_common::ScheduleFrequency::Daily,
        &30u32,
        &(env.ledger().timestamp() + 86_400),
    );
}

/// Daily frequency: period_secs = 86_400.
/// When 2 daily periods elapsed and 1 installment already paid,
/// the helper returns 2 (the next unpaid due installment).
#[test]
fn test_daily_frequency_period() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let (_reg, fin, originator, _lender) = setup_offer_for_schedule(&env, &admin, &token);

    // count = 5 daily → installment_principal = 1_200_000_000 / 5 = 240_000_000
    //                    installment_yield     = 240_000_000 * 500 / 10_000 = 12_000_000
    //                    installment_amount    = 252_000_000
    let first_due = env.ledger().timestamp() + 86_400;
    let sched = fin.schedule_repayment(
        &symbol_short!("off_sc1"),
        &originator,
        &invofi_common::ScheduleFrequency::Daily,
        &5u32,
        &first_due,
    );

    // Register a fake repayment contract (auth is mocked)
    let repayment_id = env.register(
        super::FinancingContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    fin.set_repayment_contract(&admin, &repayment_id);

    // Mark installment 1 as paid.
    fin.update_offer_amount_repaid(&symbol_short!("off_sc1"), &sched.installment_amount);

    // Advance past 2 daily periods — installment 1 paid, installment 2 is now due.
    env.ledger().set_timestamp(first_due + 86_400 + 1);
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 2);
}

// ─── Schema version tests (issue #66) ───────────────────────────────────────

mod schema_version_tests {
    use super::FinancingContract;
    use crate::FinancingContractClient;
    use invofi_registry::RegistryContract;
    use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env};

    fn deploy(env: &'_ Env) -> (Address, Address, FinancingContractClient<'_>) {
        let admin = Address::generate(env);
        let token = Address::generate(env);
        let registry_id = env.register(RegistryContract, (admin.clone(),));
        let financing_id =
            env.register(FinancingContract, (admin.clone(), registry_id.clone(), token.clone()));
        let client = FinancingContractClient::new(env, &financing_id);
        invofi_registry::RegistryContractClient::new(env, &registry_id)
            .set_financing_contract(&admin, &financing_id);
        (admin, financing_id, client)
    }

    // ── Matching version ─────────────────────────────────────────────────────

    #[test]
    fn normal_deployment_register_currency_works() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, _, client) = deploy(&env);
        let token = Address::generate(&env);
        client.register_currency(&admin, &symbol_short!("USDC"), &token); // must succeed
    }

    // ── Legacy fallback ──────────────────────────────────────────────────────

    #[test]
    fn legacy_absent_schver_register_currency_works() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, contract_id, client) = deploy(&env);

        env.as_contract(&contract_id, || {
            env.storage().instance().remove(&symbol_short!("schver"));
        });

        let token = Address::generate(&env);
        client.register_currency(&admin, &symbol_short!("USDC"), &token); // legacy fallback
    }

    // ── Mismatch panics ──────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn mismatched_schver_blocks_register_currency() {
        let env = Env::default();
        env.mock_all_auths();
        let (admin, contract_id, client) = deploy(&env);

        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .set(&symbol_short!("schver"), &2u32);
        });

        let token = Address::generate(&env);
        client.register_currency(&admin, &symbol_short!("USDC"), &token); // must panic #10
    }
}
