#![cfg(test)]
extern crate std;

use super::FinancingContract;
use invofi_common::{InvoiceStatus, NegotiationParty, NegotiationStatus, OfferStatus};
use invofi_registry::RegistryContract;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _, Ledger as _},
    token, Address, Env, Symbol, TryFromVal,
};

/// Wrap a single signer in the one-element `Vec<Address>` the threshold-gated
/// admin API expects (ADR-0010). Single-admin/bootstrap deployments pass
/// exactly this.
fn one(env: &Env, signer: &Address) -> soroban_sdk::Vec<Address> {
    let mut v = soroban_sdk::Vec::new(env);
    v.push_back(signer.clone());
    v
}

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

    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), registry_id.clone(), token.clone()),
    );
    let financing_client = super::FinancingContractClient::new(env, &financing_id);

    // Register financing as a trusted caller on the registry so its
    // cross-contract status transition (Pending -> Financed) is allowed.
    registry_client.set_financing_contract(&one(env, admin), &financing_id);

    (registry_client, financing_client)
}

/// Deploy a fresh test SEP-41 token and return its contract address.
pub(crate) fn create_token(env: &Env) -> Address {
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    sac.address()
}

/// Mint `amount` to `who` and approve `spender` to move those funds (the same
/// flow a real lender runs on-chain before `accept_offer`).
fn mint_and_approve(env: &Env, token_id: &Address, spender: &Address, who: &Address, amount: i128) {
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
fn test_create_offer_interest_rate_at_cap_passes() {
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
    let offer = fin.create_offer(
        &symbol_short!("off_v2"),
        &symbol_short!("inv_v4"),
        &lender,
        &1_000i128,
        &symbol_short!("USDC"),
        &invofi_common::MAX_INTEREST_BPS,
        &86_400u64,
    );
    assert_eq!(offer.interest_rate, invofi_common::MAX_INTEREST_BPS);
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
    reg.blacklist_address(&one(&env, &admin), &lender);
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

    reg.set_financing_contract(&one(&env, &admin), &financing_id);
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
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

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
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

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
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

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

// ─── Multisig admin governance tests (ADR-0010) ─────────────────────────────

#[test]
fn test_financing_bootstrap_admin_config_defaults() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), Address::generate(&env), Address::generate(&env)),
    );
    let fin = super::FinancingContractClient::new(&env, &financing_id);

    let cfg = fin.get_admin_config();
    assert_eq!(cfg.signers.len(), 1);
    assert_eq!(cfg.signers.get(0).unwrap(), admin);
    assert_eq!(cfg.threshold, 1);
}

#[test]
fn test_financing_set_signers_requires_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), Address::generate(&env), Address::generate(&env)),
    );
    let fin = super::FinancingContractClient::new(&env, &financing_id);

    let b = Address::generate(&env);
    let mut two_signers = soroban_sdk::Vec::new(&env);
    two_signers.push_back(admin.clone());
    two_signers.push_back(b.clone());
    fin.set_signers(&one(&env, &admin), &two_signers, &2u32);

    // A single signer is no longer sufficient once threshold is 2.
    let result = fin.try_pause(&one(&env, &admin));
    assert!(result.is_err());

    let mut both = soroban_sdk::Vec::new(&env);
    both.push_back(admin.clone());
    both.push_back(b.clone());
    fin.pause(&both);
    assert!(fin.contract_is_paused());
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

    fin.pause(&one(&env, &admin));
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

    fin.pause(&one(&env, &admin));

    fn assert_paused<F: FnOnce()>(f: F) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(
            result.is_err(),
            "state-changing function should panic while paused"
        );
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
        fin.set_repayment_contract(&one(&env, &admin), &repayment);
    });
    assert_paused(|| {
        fin.transfer_admin(&one(&env, &admin), &new_admin);
    });
    assert_paused(|| {
        fin.register_currency(&one(&env, &admin), &symbol_short!("EUR"), &Address::generate(&env));
    });
    assert_paused(|| {
        fin.set_position_token(&one(&env, &admin), &pos_token);
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

    assert_eq!(
        fin.get_offer_duration_limits().0,
        invofi_common::MIN_OFFER_DURATION_SECS
    );
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
    reg.set_financing_contract(&one(&env, &admin), &financing_id);
    assert!(fin.get_position_token().is_none());
    fin.set_position_token(&one(&env, &admin), &pos_token_id);
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

    reg.set_financing_contract(&one(&env, &admin), &financing_id);
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
    let expected_principal_slice = 1_200_000_000i128 / 12; // 100_000_000
    let expected_yield = expected_principal_slice * 500 / 10_000; // 5_000_000
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
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

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
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

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
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

    // Mark installment 1 as paid.
    fin.update_offer_amount_repaid(&symbol_short!("off_sc1"), &sched.installment_amount);

    // Advance past 2 daily periods — installment 1 paid, installment 2 is now due.
    env.ledger().set_timestamp(first_due + 86_400 + 1);
    assert_eq!(fin.get_installment_due(&symbol_short!("off_sc1")), 2);
}

// ─── Offer negotiation: amendment & counter-offer (issue #180) ───────────────
//
// The tests below drive the negotiation through the public contract client —
// the same entrypoints a wallet calls — and assert on real settlement effects
// (token balances, registry invoice status), not on internal helpers.

/// Deploy registry + financing wired to a real SEP-41 token, register an
/// invoice, and post an offer the lender can actually fund. Returns everything
/// a negotiation test needs.
#[allow(clippy::type_complexity)]
fn setup_negotiation<'a>(
    env: &'a Env,
    invoice_id: &soroban_sdk::Symbol,
    offer_id: &soroban_sdk::Symbol,
    amount: i128,
) -> (
    invofi_registry::RegistryContractClient<'a>,
    super::FinancingContractClient<'a>,
    Address, // admin
    Address, // originator
    Address, // lender
    Address, // token
) {
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
    let fin = super::FinancingContractClient::new(env, &financing_id);

    reg.set_financing_contract(&one(&env, &admin), &financing_id);
    // The lender's standing allowance to the financing contract is their
    // pre-commitment: it is what makes auto-accept executable without a second
    // signature from them at match time.
    mint_and_approve(env, &token_id, &financing_id, &lender, amount * 4);

    reg.register_invoice(
        invoice_id,
        &originator,
        &amount,
        &symbol_short!("USDC"),
        &(9_000_000u64),
    );
    fin.create_offer(
        offer_id,
        invoice_id,
        &lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &(1_296_000u64),
    );

    (reg, fin, admin, originator, lender, token_id)
}

#[test]
fn test_amend_offer_rewrites_terms_and_opens_negotiation() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv01");
    let offer_id = symbol_short!("noff01");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    let amended = fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &900_000_000i128,
        &400u32,
        &(2_592_000u64),
    );

    assert_eq!(amended.status, OfferStatus::Pending);
    assert_eq!(amended.amount, 900_000_000);
    assert_eq!(amended.interest_rate, 400);
    assert_eq!(amended.duration, 2_592_000);

    // The offer itself is the lender's standing position, so a plain read
    // sees the amended terms.
    let stored = fin.get_offer(&offer_id);
    assert_eq!(stored.amount, 900_000_000);
    assert_eq!(stored.interest_rate, 400);

    let history = fin.get_negotiation(&offer_id);
    assert_eq!(history.len(), 1);
    let round = history.get(0).unwrap();
    assert_eq!(round.party, NegotiationParty::Lender);
    assert_eq!(round.amount, 900_000_000);
    assert_eq!(round.interest_rate, 400);
    assert_eq!(round.duration, 2_592_000);
    assert_eq!(round.timestamp, 1_000_000);

    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Open
    );
    // 72-hour default window, frozen at open.
    assert_eq!(fin.get_negotiation_deadline(&offer_id), 1_000_000 + 259_200);

    // total_offered follows the amendment by its delta rather than
    // double-counting the offer.
    assert_eq!(fin.get_lender_stats(&lender).total_offered, 900_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_amend_offer_by_non_lender_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv02");
    let offer_id = symbol_short!("noff02");
    let (_reg, fin, _admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &originator,
        &0u32,
        &900_000_000i128,
        &400u32,
        &(2_592_000u64),
    );
}

#[test]
fn test_counter_offer_records_round_without_touching_the_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv03");
    let offer_id = symbol_short!("noff03");
    let (_reg, fin, _admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    let returned = fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &250u32,
        &(1_296_000u64),
    );

    // A counter-offer is a proposal, not a rewrite: the offer still says what
    // the lender said.
    assert_eq!(returned.status, OfferStatus::Pending);
    assert_eq!(fin.get_offer(&offer_id).interest_rate, 500);

    let history = fin.get_negotiation(&offer_id);
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().party, NegotiationParty::Originator);
    assert_eq!(history.get(0).unwrap().interest_rate, 250);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_counter_offer_by_non_originator_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv04");
    let offer_id = symbol_short!("noff04");
    let (_reg, fin, _admin, _originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    let stranger = Address::generate(&env);
    fin.counter_offer(
        &offer_id,
        &stranger,
        &0u32,
        &1_000_000_000i128,
        &250u32,
        &(1_296_000u64),
    );
}

#[test]
fn test_negotiation_history_records_every_round_in_order() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv05");
    let offer_id = symbol_short!("noff05");
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &450u32,
        &(1_296_000u64),
    );
    env.ledger().set_timestamp(1_000_100);
    fin.counter_offer(
        &offer_id,
        &originator,
        &1u32,
        &1_000_000_000i128,
        &300u32,
        &(1_296_000u64),
    );
    env.ledger().set_timestamp(1_000_200);
    fin.amend_offer(
        &offer_id,
        &lender,
        &2u32,
        &1_000_000_000i128,
        &380u32,
        &(1_296_000u64),
    );

    let history = fin.get_negotiation(&offer_id);
    assert_eq!(history.len(), 3);
    assert_eq!(history.get(0).unwrap().party, NegotiationParty::Lender);
    assert_eq!(history.get(0).unwrap().interest_rate, 450);
    assert_eq!(history.get(1).unwrap().party, NegotiationParty::Originator);
    assert_eq!(history.get(1).unwrap().interest_rate, 300);
    assert_eq!(history.get(2).unwrap().party, NegotiationParty::Lender);
    assert_eq!(history.get(2).unwrap().interest_rate, 380);
    assert_eq!(history.get(2).unwrap().timestamp, 1_000_200);

    // The deadline was frozen by the *first* round, not pushed out by later
    // ones — a negotiation cannot be extended indefinitely by ping-pong.
    assert_eq!(fin.get_negotiation_deadline(&offer_id), 1_000_000 + 259_200);
}

// ── Auto-accept: the two paths that actually move money ──────────────────────

#[test]
fn test_auto_accept_when_lender_amends_onto_the_live_counter_offer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv06");
    let offer_id = symbol_short!("noff06");
    let amount: i128 = 1_000_000_000;
    let (reg, fin, _admin, originator, lender, token_id) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    // Originator counters at a lower rate and a shorter duration.
    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &(amount),
        &250u32,
        &(864_000u64),
    );

    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&originator), 0);

    // The lender meets it exactly. Agreement -> settlement in this same call,
    // with no further signature from the originator.
    let settled = fin.amend_offer(&offer_id, &lender, &1u32, &(amount), &250u32, &(864_000u64));

    assert_eq!(settled.status, OfferStatus::Accepted);
    assert_eq!(settled.interest_rate, 250);
    assert_eq!(settled.duration, 864_000);
    assert_eq!(settled.funded_at, 1_000_000);

    // The headline, proved from the outside: the invoice is financed and the
    // principal is in the business's account.
    assert_eq!(reg.get_invoice(&invoice_id).status, InvoiceStatus::Financed);
    assert_eq!(token_client.balance(&originator), amount);

    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Accepted
    );
    assert_eq!(fin.get_negotiation(&offer_id).len(), 2);
}

#[test]
fn test_auto_accept_when_originator_counters_at_the_standing_terms() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv07");
    let offer_id = symbol_short!("noff07");
    let amount: i128 = 1_000_000_000;
    let (reg, fin, _admin, originator, lender, token_id) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    // Lender amends down to 300 bps; the originator takes exactly that.
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &(amount),
        &300u32,
        &(1_296_000u64),
    );
    let settled = fin.counter_offer(
        &offer_id,
        &originator,
        &1u32,
        &(amount),
        &300u32,
        &(1_296_000u64),
    );

    assert_eq!(settled.status, OfferStatus::Accepted);
    assert_eq!(reg.get_invoice(&invoice_id).status, InvoiceStatus::Financed);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&originator), amount);
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Accepted
    );
}

#[test]
fn test_near_miss_terms_do_not_auto_accept() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv08");
    let offer_id = symbol_short!("noff08");
    let amount: i128 = 1_000_000_000;
    let (reg, fin, _admin, originator, lender, token_id) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &(amount),
        &250u32,
        &(864_000u64),
    );
    // One basis point off is not agreement.
    let still_open = fin.amend_offer(&offer_id, &lender, &1u32, &(amount), &251u32, &(864_000u64));

    assert_eq!(still_open.status, OfferStatus::Pending);
    assert_eq!(reg.get_invoice(&invoice_id).status, InvoiceStatus::Pending);
    let token_client = token::TokenClient::new(&env, &token_id);
    assert_eq!(token_client.balance(&originator), 0);
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Open
    );
}

#[test]
fn test_superseded_counter_offer_is_not_executable() {
    // Adversarial: the originator proposes T1, then moves off it to T2. A
    // lender who later amends onto T1 must not be able to settle against a
    // proposal the originator has abandoned.
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv09");
    let offer_id = symbol_short!("noff09");
    let amount: i128 = 1_000_000_000;
    let (reg, fin, _admin, originator, lender, token_id) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &(amount),
        &200u32,
        &(864_000u64),
    );
    fin.counter_offer(
        &offer_id,
        &originator,
        &1u32,
        &(amount),
        &220u32,
        &(864_000u64),
    );

    // The lender reaches for the abandoned T1.
    let result = fin.amend_offer(&offer_id, &lender, &2u32, &(amount), &200u32, &(864_000u64));

    assert_eq!(result.status, OfferStatus::Pending);
    assert_eq!(reg.get_invoice(&invoice_id).status, InvoiceStatus::Pending);
    assert_eq!(
        token::TokenClient::new(&env, &token_id).balance(&originator),
        0
    );
}

// ── Optimistic concurrency ───────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_stale_round_index_is_rejected() {
    // Adversarial: the lender reads the negotiation at round 0, the originator
    // counters before the lender's transaction lands, and the lender's
    // amendment would otherwise apply to a term-set that changed underneath
    // it — and could auto-execute against a counter-offer it never saw.
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv10");
    let offer_id = symbol_short!("noff10");
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &250u32,
        &(864_000u64),
    );

    // Lender still believes the history is empty.
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_counter_offer_with_stale_round_index_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv11");
    let offer_id = symbol_short!("noff11");
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &250u32,
        &(864_000u64),
    );
}

// ── Expiry ───────────────────────────────────────────────────────────────────

#[test]
fn test_negotiation_status_expires_on_read_without_any_call() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv12");
    let offer_id = symbol_short!("noff12");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );

    // On the deadline itself the negotiation is still open.
    env.ledger().set_timestamp(1_000_000 + 259_200);
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Open
    );

    // One second later it is expired — derived, with nothing having been
    // called in between.
    env.ledger().set_timestamp(1_000_000 + 259_201);
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Expired
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_amend_after_expiry_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv13");
    let offer_id = symbol_short!("noff13");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );

    env.ledger().set_timestamp(1_000_000 + 259_201);
    fin.amend_offer(
        &offer_id,
        &lender,
        &1u32,
        &1_000_000_000i128,
        &350u32,
        &(1_296_000u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_expired_counter_offer_cannot_be_executed_by_the_lender() {
    // The commitment a counter-offer represents is bounded by the window: an
    // originator who proposed terms three days ago is not still on the hook
    // for them.
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv14");
    let offer_id = symbol_short!("noff14");
    let amount: i128 = 1_000_000_000;
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &(amount),
        &250u32,
        &(864_000u64),
    );

    env.ledger().set_timestamp(1_000_000 + 259_201);
    fin.amend_offer(&offer_id, &lender, &1u32, &(amount), &250u32, &(864_000u64));
}

#[test]
fn test_close_negotiation_after_expiry_is_permissionless() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv15");
    let offer_id = symbol_short!("noff15");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );

    env.ledger().set_timestamp(1_000_000 + 300_000);
    let keeper = Address::generate(&env);
    let outcome = fin.close_negotiation(&offer_id, &keeper);

    assert_eq!(outcome, NegotiationStatus::Expired);
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Expired
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_close_negotiation_twice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv16");
    let offer_id = symbol_short!("noff16");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
    fin.close_negotiation(&offer_id, &lender);
    fin.close_negotiation(&offer_id, &lender);
}

// ── Early close / revocation ─────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_originator_can_revoke_a_counter_offer_before_the_window_ends() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv17");
    let offer_id = symbol_short!("noff17");
    let amount: i128 = 1_000_000_000;
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &(amount),
        &250u32,
        &(864_000u64),
    );
    assert_eq!(
        fin.close_negotiation(&offer_id, &originator),
        NegotiationStatus::Closed
    );

    // The lender can no longer settle against the revoked proposal.
    fin.amend_offer(&offer_id, &lender, &1u32, &(amount), &250u32, &(864_000u64));
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_stranger_cannot_close_an_open_negotiation() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv18");
    let offer_id = symbol_short!("noff18");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
    fin.close_negotiation(&offer_id, &Address::generate(&env));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_close_negotiation_that_was_never_opened_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv19");
    let offer_id = symbol_short!("noff19");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.close_negotiation(&offer_id, &lender);
}

#[test]
fn test_withdrawing_the_offer_ends_its_negotiation() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv20");
    let offer_id = symbol_short!("noff20");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
    fin.withdraw_offer(&offer_id, &lender);

    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Closed
    );
}

// ── Guards ───────────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_amend_after_acceptance_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv21");
    let offer_id = symbol_short!("noff21");
    let amount: i128 = 1_000_000_000;
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    fin.accept_offer(&offer_id, &originator);
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &(amount),
        &400u32,
        &(1_296_000u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_amend_cannot_reach_terms_create_offer_would_reject() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv22");
    let offer_id = symbol_short!("noff22");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // 10 001 bps is above the create_offer ceiling.
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &10_001u32,
        &(1_296_000u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_counter_offer_below_minimum_duration_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv23");
    let offer_id = symbol_short!("noff23");
    let (_reg, fin, _admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &250u32,
        &(86_399u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_negotiation_round_cap_is_enforced() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv24");
    let offer_id = symbol_short!("noff24");
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // 20 rounds fit; the 21st does not. Terms differ every round so nothing
    // auto-accepts along the way.
    for round in 0u32..20 {
        let rate = 400 + round;
        if round % 2 == 0 {
            fin.amend_offer(
                &offer_id,
                &lender,
                &round,
                &1_000_000_000i128,
                &rate,
                &(1_296_000u64),
            );
        } else {
            fin.counter_offer(
                &offer_id,
                &originator,
                &round,
                &900_000_000i128,
                &rate,
                &(1_296_000u64),
            );
        }
    }
    assert_eq!(fin.get_negotiation(&offer_id).len(), 20);

    fin.amend_offer(
        &offer_id,
        &lender,
        &20u32,
        &1_000_000_000i128,
        &499u32,
        &(1_296_000u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_amend_offer_is_pause_guarded() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv25");
    let offer_id = symbol_short!("noff25");
    let (_reg, fin, admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.pause(&one(&env, &admin));
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_counter_offer_is_pause_guarded() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv26");
    let offer_id = symbol_short!("noff26");
    let (_reg, fin, admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.pause(&one(&env, &admin));
    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &250u32,
        &(1_296_000u64),
    );
}

// ── Window configuration ─────────────────────────────────────────────────────

#[test]
fn test_negotiation_window_is_configurable_and_deadlines_are_frozen() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv27");
    let offer_id = symbol_short!("noff27");
    let (_reg, fin, admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    assert_eq!(fin.get_negotiation_window(), 259_200);

    fin.set_negotiation_window(&one(&env, &admin), &7_200u64);
    assert_eq!(fin.get_negotiation_window(), 7_200);

    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
    assert_eq!(fin.get_negotiation_deadline(&offer_id), 1_007_200);

    // Widening the window afterwards must not resurrect a negotiation that is
    // already running against a frozen deadline.
    fin.set_negotiation_window(&one(&env, &admin), &2_592_000u64);
    assert_eq!(fin.get_negotiation_deadline(&offer_id), 1_007_200);
    env.ledger().set_timestamp(1_007_201);
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Expired
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_set_negotiation_window_is_admin_only() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv28");
    let offer_id = symbol_short!("noff28");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.set_negotiation_window(&one(&env, &lender), &7_200u64);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_negotiation_window_below_minimum_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv29");
    let offer_id = symbol_short!("noff29");
    let (_reg, fin, admin, _originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.set_negotiation_window(&one(&env, &admin), &3_599u64);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_negotiation_window_above_maximum_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv30");
    let offer_id = symbol_short!("noff30");
    let (_reg, fin, admin, _originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    fin.set_negotiation_window(&one(&env, &admin), &2_592_001u64);
}

// ── Events ───────────────────────────────────────────────────────────────────

/// How many published events carry `name` as their first topic.
fn count_events(env: &Env, name: Symbol) -> u32 {
    let mut count = 0;
    for (_contract, topics, _data) in env.events().all().iter() {
        if let Some(first) = topics.get(0) {
            if let Ok(topic) = Symbol::try_from_val(env, &first) {
                if topic == name {
                    count += 1;
                }
            }
        }
    }
    count
}

#[test]
fn test_negotiation_emits_amend_counter_and_close_events() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv31");
    let offer_id = symbol_short!("noff31");
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // The test harness exposes the events of the most recent invocation, so
    // each call is checked as it lands rather than all of them at the end.
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &400u32,
        &(1_296_000u64),
    );
    assert_eq!(
        count_events(&env, symbol_short!("off_amd")),
        1,
        "amend_offer must emit off_amd"
    );

    fin.counter_offer(
        &offer_id,
        &originator,
        &1u32,
        &1_000_000_000i128,
        &250u32,
        &(864_000u64),
    );
    assert_eq!(
        count_events(&env, symbol_short!("ctr_off")),
        1,
        "counter_offer must emit ctr_off"
    );

    fin.close_negotiation(&offer_id, &originator);
    assert_eq!(
        count_events(&env, symbol_short!("neg_clsd")),
        1,
        "close_negotiation must emit neg_clsd"
    );
}

#[test]
fn test_auto_accept_emits_negotiation_closed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv32");
    let offer_id = symbol_short!("noff32");
    let amount: i128 = 1_000_000_000;
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &(amount),
        &250u32,
        &(864_000u64),
    );
    fin.amend_offer(&offer_id, &lender, &1u32, &(amount), &250u32, &(864_000u64));

    assert_eq!(
        count_events(&env, symbol_short!("neg_clsd")),
        1,
        "auto-accept must emit neg_clsd exactly once"
    );
    assert_eq!(
        count_events(&env, symbol_short!("off_acc")),
        1,
        "auto-accept must run the ordinary off_acc settlement path"
    );
}

#[test]
fn test_plain_accept_closes_an_open_negotiation_as_accepted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv33");
    let offer_id = symbol_short!("noff33");
    let amount: i128 = 1_000_000_000;
    let (_reg, fin, _admin, originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    // A negotiation is under way when the originator decides to just take the
    // lender's standing terms through accept_offer instead of countering at
    // them. That route settles the offer, so it ends the negotiation too.
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &(amount),
        &400u32,
        &(1_296_000u64),
    );
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Open
    );

    fin.accept_offer(&offer_id, &originator);

    // Events are read before any getter runs: the harness exposes only the
    // most recent invocation's events, and a read call is an invocation.
    assert_eq!(
        count_events(&env, symbol_short!("neg_clsd")),
        1,
        "accept_offer must announce the end of an open negotiation"
    );
    assert_eq!(count_events(&env, symbol_short!("off_acc")), 1);

    // Accepted, not Closed: the negotiation ended because the offer was taken,
    // and an indexer must be able to tell that apart from a walk-away.
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::Accepted,
        "accept_offer must record the negotiation as Accepted"
    );
}

#[test]
fn test_plain_accept_without_a_negotiation_emits_no_closure() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("ninv34");
    let offer_id = symbol_short!("noff34");
    let amount: i128 = 1_000_000_000;
    let (_reg, fin, _admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, amount);

    // No negotiation was ever opened, so there is nothing to close and the
    // ordinary acceptance path must stay exactly as it was.
    fin.accept_offer(&offer_id, &originator);

    assert_eq!(count_events(&env, symbol_short!("off_acc")), 1);
    assert_eq!(
        count_events(&env, symbol_short!("neg_clsd")),
        0,
        "an offer with no negotiation must not emit neg_clsd"
    );
    assert_eq!(
        fin.get_negotiation_status(&offer_id),
        NegotiationStatus::None
    );
}

// ── Interest-rate cap enforcement across all term-setting paths ────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_amend_offer_interest_rate_above_cap_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("inv_rc");
    let offer_id = symbol_short!("off_rc");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // Amend with rate = MAX_INTEREST_BPS + 1 must be rejected.
    fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &(invofi_common::MAX_INTEREST_BPS + 1),
        &1_296_000u64,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_counter_offer_interest_rate_above_cap_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("inv_cc");
    let offer_id = symbol_short!("off_cc");
    let (_reg, fin, _admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // Counter-offer with rate = MAX_INTEREST_BPS + 1 must be rejected.
    fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &(invofi_common::MAX_INTEREST_BPS + 1),
        &1_296_000u64,
    );
}

#[test]
fn test_amend_offer_interest_rate_at_cap_passes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("inv_rk");
    let offer_id = symbol_short!("off_rk");
    let (_reg, fin, _admin, _originator, lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // Amend with rate = MAX_INTEREST_BPS must succeed.
    let amended = fin.amend_offer(
        &offer_id,
        &lender,
        &0u32,
        &1_000_000_000i128,
        &invofi_common::MAX_INTEREST_BPS,
        &1_296_000u64,
    );
    assert_eq!(amended.interest_rate, invofi_common::MAX_INTEREST_BPS);
}

#[test]
fn test_counter_offer_interest_rate_at_cap_records_in_history() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let invoice_id = symbol_short!("inv_ck");
    let offer_id = symbol_short!("off_ck");
    let (_reg, fin, _admin, originator, _lender, _token) =
        setup_negotiation(&env, &invoice_id, &offer_id, 1_000_000_000);

    // Counter-offer with rate = MAX_INTEREST_BPS must succeed.
    // counter_offer records in negotiation history — the offer's rate
    // is unchanged (it is the lender's standing position).
    let countered = fin.counter_offer(
        &offer_id,
        &originator,
        &0u32,
        &1_000_000_000i128,
        &invofi_common::MAX_INTEREST_BPS,
        &1_296_000u64,
    );
    // offer.status is still Pending (counter-offer doesn't auto-accept here)
    assert_eq!(countered.status, OfferStatus::Pending);
    // The rate is recorded in negotiation history
    let history = fin.get_negotiation(&offer_id);
    assert_eq!(history.len(), 1);
    assert_eq!(history.get(0).unwrap().interest_rate, invofi_common::MAX_INTEREST_BPS);
}
