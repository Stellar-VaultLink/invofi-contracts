#![cfg(test)]
extern crate std;

use invofi_common::{InvoiceStatus, OfferStatus};
use invofi_financing::FinancingContract;
use invofi_insurance::InsuranceContract;
use invofi_registry::RegistryContract;
use invofi_repayment::RepaymentContract;
use invofi_reputation::ReputationContract;
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

// ═══════════════════════════════════════════════════════════════════════════════
// Shared Test Helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Wrap a single signer in the one-element `Vec<Address>` the threshold-gated
/// admin API expects (ADR-0010). Single-admin/bootstrap deployments pass
/// exactly this.
fn one(env: &Env, signer: &Address) -> soroban_sdk::Vec<Address> {
    let mut v = soroban_sdk::Vec::new(env);
    v.push_back(signer.clone());
    v
}

/// Deploy a fresh SEP-41 token and return its contract address.
pub(crate) fn create_token(env: &Env) -> Address {
    let token_admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    sac.address()
}

/// Mint `amount` of `token_id` to `who` and approve `spender` to move those
/// funds (the standard pre-accept flow).
pub(crate) fn mint_and_approve(
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

/// Full protocol deployment: registry → financing → repayment → insurance →
/// reputation. All five contracts are wired together with the same admin and
/// settlement token.
///
/// Returns a tuple of (clients, addresses) so tests can inspect state on any
/// contract after driving the lifecycle.
pub(crate) struct Protocol {
    #[allow(dead_code)]
    pub(crate) admin: Address,
    pub(crate) originator: Address,
    pub(crate) lender: Address,
    pub(crate) token_id: Address,
    #[allow(dead_code)]
    pub(crate) registry_id: Address,
    pub(crate) financing_id: Address,
    #[allow(dead_code)]
    pub(crate) repayment_id: Address,
    #[allow(dead_code)]
    pub(crate) insurance_id: Address,
    #[allow(dead_code)]
    pub(crate) reputation_id: Address,
    pub(crate) reg: invofi_registry::RegistryContractClient<'static>,
    pub(crate) fin: invofi_financing::FinancingContractClient<'static>,
    pub(crate) rep: invofi_repayment::RepaymentContractClient<'static>,
    pub(crate) ins: invofi_insurance::InsuranceContractClient<'static>,
    pub(crate) repu: invofi_reputation::ReputationContractClient<'static>,
}

/// Deploy and wire the entire five-contract protocol. The returned `Protocol`
/// struct borrows `env` via `'static` — safe because Soroban test envs are
/// leaked onto the stack and never deallocated during the test.
pub(crate) fn deploy_protocol(env: &Env) -> Protocol {
    let admin = Address::generate(env);
    let originator = Address::generate(env);
    let lender = Address::generate(env);
    let token_id = create_token(env);

    // 1. Registry
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let reg = invofi_registry::RegistryContractClient::new(env, &registry_id);

    // 2. Financing (wired to registry + token)
    let financing_id = env.register(
        FinancingContract,
        (admin.clone(), registry_id.clone(), token_id.clone()),
    );
    let fin = invofi_financing::FinancingContractClient::new(env, &financing_id);

    // 3. Repayment (wired to registry + financing + token)
    let repayment_id = env.register(
        RepaymentContract,
        (
            admin.clone(),
            registry_id.clone(),
            financing_id.clone(),
            token_id.clone(),
        ),
    );
    let rep = invofi_repayment::RepaymentContractClient::new(env, &repayment_id);

    // 4. Insurance (wired to token + registry)
    let insurance_id = env.register(InsuranceContract, (admin.clone(), token_id.clone()));
    let ins = invofi_insurance::InsuranceContractClient::new(env, &insurance_id);

    // 5. Reputation
    let reputation_id = env.register(ReputationContract, (admin.clone(),));
    let repu = invofi_reputation::ReputationContractClient::new(env, &reputation_id);

    // ── Wire cross-contract trust ──────────────────────────────────────────
    // Registry trusts financing and repayment for system status transitions.
    reg.set_financing_contract(&one(&env, &admin), &financing_id);
    reg.set_repayment_contract(&one(&env, &admin), &repayment_id);

    // Financing trusts repayment for callback methods.
    fin.set_repayment_contract(&one(&env, &admin), &repayment_id);

    // Repayment trusts insurance and reputation for default hooks.
    rep.set_insurance(&one(&env, &admin), &insurance_id);
    rep.set_reputation(&one(&env, &admin), &reputation_id);

    // Insurance: payout caller = repayment, registry for Defaulted check.
    ins.set_payout_caller(&one(&env, &admin), &repayment_id);
    ins.set_registry(&one(&env, &admin), &registry_id);

    // Reputation: recorder = repayment.
    repu.set_recorder(&one(&env, &admin), &repayment_id);

    Protocol {
        admin,
        originator,
        lender,
        token_id,
        registry_id,
        financing_id,
        repayment_id,
        insurance_id,
        reputation_id,
        reg,
        fin,
        rep,
        ins,
        repu,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// PHASE 3 — Cross-Crate Tests
// ═══════════════════════════════════════════════════════════════════════════════

// ─────────────────────────────────────────────────────────────────────────────
// Boundary 1: Registry ↔ Financing
//
// Covers: register_invoice → create_offer → accept_offer
//         (Pending → Financed via financing_marks_invoice_financed)
// ─────────────────────────────────────────────────────────────────────────────

/// Register an invoice, create an offer, and accept it. The invoice must
/// transition from Pending to Financed in the registry, and the lender's
/// principal must move to the originator.
#[test]
fn test_registry_financing_accept_offer_transitions_invoice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);
    let amount: i128 = 1_000_000_000;
    let offer_id = symbol_short!("off001");
    let invoice_id = symbol_short!("inv001");

    // Pre-fund: lender approves the financing contract to spend tokens.
    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);

    // Step 1: Originator registers an invoice.
    let inv = p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    assert_eq!(inv.status, InvoiceStatus::Pending);

    // Step 2: Lender creates a financing offer.
    let offer = p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
        &0u64,
    );
    assert_eq!(offer.status, OfferStatus::Pending);

    // Step 3: Originator accepts the offer.
    let accepted = p.fin.accept_offer(&offer_id, &p.originator);
    assert_eq!(accepted.status, OfferStatus::Accepted);

    // Assertion: Invoice is now Financed in the registry (cross-crate status sync).
    let inv_after = p.reg.get_invoice(&invoice_id);
    assert_eq!(inv_after.status, InvoiceStatus::Financed);

    // Assertion: Lender's principal moved to originator (token transfer).
    let tok = token::TokenClient::new(&env, &p.token_id);
    assert_eq!(tok.balance(&p.lender), 0);
    assert_eq!(tok.balance(&p.originator), amount);
}

/// Negative test: Creating an offer on a non-Pending invoice must fail.
#[test]
#[should_panic(expected = "Invoice must be Pending to accept offers")]
fn test_registry_financing_offer_on_financed_invoice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);
    let invoice_id = symbol_short!("inv_blk");

    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    // Use the originator escape hatch to move it to Financed.
    p.reg
        .update_invoice_status(&invoice_id, &p.originator, &InvoiceStatus::Financed);

    // Try to create an offer on the already-Financed invoice — must panic.
    p.fin.create_offer(
        &symbol_short!("off_blk"),
        &invoice_id,
        &p.lender,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &86_400u64,
        &0u64,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Boundary 2: Financing ↔ Repayment
//
// Covers: repay_invoice via Repayment contract
//         (updates offer status, transfers funds, marks invoice repaid)
// ─────────────────────────────────────────────────────────────────────────────

/// Full repayment via the Repayment contract. Offer and invoice must both
/// transition to Repaid, and funds must reach the lender.
#[test]
fn test_financing_repayment_full_repay_syncs_state() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let p = deploy_protocol(&env);
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500;
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;
    let invoice_id = symbol_short!("inv_rp");
    let offer_id = symbol_short!("off_rp");

    // Fund lender and register invoice.
    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );

    // Create + accept offer.
    p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&offer_id, &p.originator);

    // Advance 365 days so pro-rata interest = flat yield.
    env.ledger().set_timestamp(funded_at + 365 * 86_400);

    // Fund originator for repayment.
    let asset = token::StellarAssetClient::new(&env, &p.token_id);
    asset.mint(&p.originator, &total_due);

    // Repay in full via the Repayment contract.
    let repaid = p
        .rep
        .repay_invoice(&invoice_id, &offer_id, &p.originator, &total_due);
    assert_eq!(repaid.status, InvoiceStatus::Repaid);

    // Offer must be Repaid in Financing (cross-crate state sync).
    let offer_after = p.fin.get_offer(&offer_id);
    assert_eq!(offer_after.status, OfferStatus::Repaid);

    // Lender received principal + yield.
    let tok = token::TokenClient::new(&env, &p.token_id);
    assert_eq!(tok.balance(&p.lender), total_due);
}

/// Partial repayment keeps both invoice and offer in Financed state.
#[test]
fn test_financing_repayment_partial_keeps_financed() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let p = deploy_protocol(&env);
    let amount: i128 = 1_000_000_000;
    let invoice_id = symbol_short!("inv_pr2");
    let offer_id = symbol_short!("off_pr2");

    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&offer_id, &p.originator);

    // Advance 1 day to accrue some interest.
    env.ledger().set_timestamp(funded_at + 86_400);

    // Partial repayment (10% of principal — above the 1% minimum).
    let partial = amount / 10;
    let asset = token::StellarAssetClient::new(&env, &p.token_id);
    asset.mint(&p.originator, &partial);
    let repaid = p
        .rep
        .repay_invoice(&invoice_id, &offer_id, &p.originator, &partial);
    assert_eq!(repaid.status, InvoiceStatus::Financed);

    let offer_after = p.fin.get_offer(&offer_id);
    assert_eq!(offer_after.status, OfferStatus::Financed);
    assert_eq!(offer_after.amount_repaid, partial);
}

/// Negative test: Repaying a Pending (unfinanced) invoice must fail.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_financing_repayment_on_pending_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);

    p.reg.register_invoice(
        &symbol_short!("inv_np"),
        &p.originator,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    p.fin.create_offer(
        &symbol_short!("off_np"),
        &symbol_short!("inv_np"),
        &p.lender,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &500u32,
        &86_400u64,
        &0u64,
    );
    // Offer NOT accepted — invoice is still Pending.
    p.rep.repay_invoice(
        &symbol_short!("inv_np"),
        &symbol_short!("off_np"),
        &p.originator,
        &1,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Boundary 3: Repayment ↔ Insurance
//
// Covers: reclaim_invoice → repayment_marks_defaulted → pay_out
//         (Overdue → Defaulted → insurance payout to lender)
// ─────────────────────────────────────────────────────────────────────────────

/// Full default flow: overdue → reclaim → insurance pays out to the lender.
/// The insurance pool must be drained pro-rata and the lender receives the
/// payout.
#[test]
fn test_repayment_insurance_default_triggers_payout() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);
    let staker = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let invoice_id = symbol_short!("inv_df");
    let offer_id = symbol_short!("off_df");

    // Fund lender + register invoice.
    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );

    // Create + accept offer.
    p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&offer_id, &p.originator);

    // Fund the insurance pool.
    let coverage: i128 = 300_000_000;
    mint_and_approve(&env, &p.token_id, &p.insurance_id, &staker, coverage);
    p.ins.stake(&staker, &coverage);

    // Past due + grace period → mark overdue → reclaim.
    env.ledger()
        .set_timestamp(due_date + invofi_common::GRACE_PERIOD_SECS + 1);
    p.rep.mark_overdue(&invoice_id);

    let reclaimed = p.rep.reclaim_invoice(&invoice_id, &offer_id, &p.lender);
    assert_eq!(reclaimed.status, OfferStatus::Defaulted);

    // Invoice must be Defaulted in the registry (cross-crate status sync).
    let inv = p.reg.get_invoice(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Defaulted);

    // Lender received insurance payout (capped at pool).
    // Insurance pays principal + flat yield (frozen base for penalty).
    let total_due = amount + amount * 500 / 10_000;
    assert!(total_due > coverage);
    let tok = token::TokenClient::new(&env, &p.token_id);
    assert_eq!(tok.balance(&p.lender), coverage);

    // Pool drained.
    assert_eq!(p.ins.get_pool_total(), 0);
    assert_eq!(p.ins.get_stake(&staker), 0);
}

/// Negative test: Reclaiming before the grace period must fail.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_repayment_insurance_reclaim_before_grace_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let invoice_id = symbol_short!("inv_gp");
    let offer_id = symbol_short!("off_gp");

    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&offer_id, &p.originator);

    // Past due_date but NOT past the grace period.
    env.ledger().set_timestamp(due_date + 1);
    p.rep.mark_overdue(&invoice_id);
    p.rep.reclaim_invoice(&invoice_id, &offer_id, &p.lender);
}

// ─────────────────────────────────────────────────────────────────────────────
// Boundary 4: Repayment ↔ Reputation
//
// Covers: full repay → record_outcome(0) → score increases
//         reclaim → record_outcome(1) → score decreases
// ─────────────────────────────────────────────────────────────────────────────

/// Full repayment records a success on the reputation contract. The
/// originator's score must increase by 1.
#[test]
fn test_repayment_reputation_success_on_full_repay() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let p = deploy_protocol(&env);
    let amount: i128 = 1_000_000_000;
    let interest_rate: u32 = 500;
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;
    let invoice_id = symbol_short!("inv_rs");
    let offer_id = symbol_short!("off_rs");

    // Verify starting score is 0.
    assert_eq!(p.repu.get_score(&p.originator), 0);

    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&offer_id, &p.originator);

    // Advance 365 days so pro-rata interest = flat yield.
    env.ledger().set_timestamp(funded_at + 365 * 86_400);

    // Fund originator + full repayment.
    let asset = token::StellarAssetClient::new(&env, &p.token_id);
    asset.mint(&p.originator, &total_due);
    p.rep
        .repay_invoice(&invoice_id, &offer_id, &p.originator, &total_due);

    // Reputation: one success → score 1.
    assert_eq!(p.repu.get_score(&p.originator), 1);
    let rec = p.repu.get_record(&p.originator);
    assert_eq!(rec.repayments, 1);
    assert_eq!(rec.defaults, 0);
}

/// Default records a failure on the reputation contract. The originator's
/// score must be floored at 0 after a default.
#[test]
fn test_repayment_reputation_default_on_reclaim() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let p = deploy_protocol(&env);
    let staker = Address::generate(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let invoice_id = symbol_short!("inv_rd");
    let offer_id = symbol_short!("off_rd");

    // Seed originator with 2 successes first.
    assert_eq!(p.repu.get_score(&p.originator), 0);

    // We'll do two quick repay cycles to build score, then one default.
    for i in 0u32..2 {
        let inv_id = soroban_sdk::Symbol::new(
            &env,
            match i {
                0 => "inv_s1",
                _ => "inv_s2",
            },
        );
        let off_id = soroban_sdk::Symbol::new(
            &env,
            match i {
                0 => "off_s1",
                _ => "off_s2",
            },
        );
        let due: u64 = 3_000_000 + i as u64;

        let asset = token::StellarAssetClient::new(&env, &p.token_id);
        asset.mint(&p.lender, &amount);
        mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);

        p.reg.register_invoice(
            &inv_id,
            &p.originator,
            &amount,
            &symbol_short!("USDC"),
            &due,
        );
        p.fin.create_offer(
            &off_id,
            &inv_id,
            &p.lender,
            &amount,
            &symbol_short!("USDC"),
            &500u32,
            &2_592_000u64,
        );
        p.fin.accept_offer(&off_id, &p.originator);

        // Advance 365 days so pro-rata interest = flat yield.
        env.ledger().set_timestamp(funded_at + 365 * 86_400);

        let total = amount + amount * 500 / 10_000;
        let asset2 = token::StellarAssetClient::new(&env, &p.token_id);
        asset2.mint(&p.originator, &total);
        p.rep.repay_invoice(&inv_id, &off_id, &p.originator, &total);

        // Reset timestamp for next iteration.
        env.ledger().set_timestamp(funded_at);
    }
    assert_eq!(p.repu.get_score(&p.originator), 2);

    // Now create a default scenario.
    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    let coverage: i128 = 1_000_000_000;
    mint_and_approve(&env, &p.token_id, &p.insurance_id, &staker, coverage);
    p.ins.stake(&staker, &coverage);

    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&offer_id, &p.originator);

    env.ledger()
        .set_timestamp(due_date + invofi_common::GRACE_PERIOD_SECS + 1);
    p.rep.mark_overdue(&invoice_id);
    p.rep.reclaim_invoice(&invoice_id, &offer_id, &p.lender);

    // Default penalises 2 points: 2 - 2 = 0.
    assert_eq!(p.repu.get_score(&p.originator), 0);
    let rec = p.repu.get_record(&p.originator);
    assert_eq!(rec.defaults, 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Boundary 5: Registry ↔ Repayment
//
// Covers: mark_overdue via Repayment contract → registry status sync
//         repayment_marks_defaulted via reclaim
// ─────────────────────────────────────────────────────────────────────────────

/// Marking overdue through the Repayment contract delegates to the registry
/// and transitions Financed → Overdue.
#[test]
fn test_registry_repayment_overdue_delegates_to_registry() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);
    let amount: i128 = 1_000_000_000;
    let due_date: u64 = 1_735_689_600;
    let invoice_id = symbol_short!("inv_ov");

    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &due_date,
    );
    p.fin.create_offer(
        &symbol_short!("off_ov"),
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &500u32,
        &2_592_000u64,
        &0u64,
    );
    p.fin.accept_offer(&symbol_short!("off_ov"), &p.originator);

    // After due_date, anyone can mark overdue through Repayment.
    env.ledger().set_timestamp(due_date + 1);
    p.rep.mark_overdue(&invoice_id);

    let inv = p.reg.get_invoice(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Overdue);
}

/// Negative test: Marking overdue on a Pending invoice must fail.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_registry_repayment_overdue_on_pending_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let p = deploy_protocol(&env);

    p.reg.register_invoice(
        &symbol_short!("inv_no"),
        &p.originator,
        &10_000_000i128,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    // Invoice is still Pending — mark_overdue must panic.
    p.rep.mark_overdue(&symbol_short!("inv_no"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Full Lifecycle End-to-End Test
// ═══════════════════════════════════════════════════════════════════════════════

/// Complete happy-path lifecycle touching all five contracts:
/// register → offer → accept → repay → repaid.
/// Verifies every cross-crate state transition and fund flow.
#[test]
fn test_full_lifecycle_register_offer_accept_repay() {
    let env = Env::default();
    env.mock_all_auths();
    let funded_at: u64 = 1_000_000;
    env.ledger().set_timestamp(funded_at);

    let p = deploy_protocol(&env);
    let amount: i128 = 2_000_000_000;
    let interest_rate: u32 = 300;
    let yield_amount = amount * (interest_rate as i128) / 10_000;
    let total_due = amount + yield_amount;
    let invoice_id = symbol_short!("inv_lc");
    let offer_id = symbol_short!("off_lc");

    // ── Step 1: Register invoice (Registry) ────────────────────────────────
    let inv = p.reg.register_invoice(
        &invoice_id,
        &p.originator,
        &amount,
        &symbol_short!("USDC"),
        &3_000_000u64,
    );
    assert_eq!(inv.status, InvoiceStatus::Pending);
    assert_eq!(inv.amount, amount);

    // ── Step 2: Create offer (Financing) ───────────────────────────────────
    let offer = p.fin.create_offer(
        &offer_id,
        &invoice_id,
        &p.lender,
        &amount,
        &symbol_short!("USDC"),
        &interest_rate,
        &1_296_000u64,
        &0u64,
    );
    assert_eq!(offer.status, OfferStatus::Pending);
    assert_eq!(offer.interest_rate, interest_rate);

    // ── Step 3: Accept offer (Financing → Registry) ────────────────────────
    mint_and_approve(&env, &p.token_id, &p.financing_id, &p.lender, amount);
    let accepted = p.fin.accept_offer(&offer_id, &p.originator);
    assert_eq!(accepted.status, OfferStatus::Accepted);

    // Invoice is now Financed.
    let inv_fin = p.reg.get_invoice(&invoice_id);
    assert_eq!(inv_fin.status, InvoiceStatus::Financed);

    // Lender's funds moved to originator.
    let tok = token::TokenClient::new(&env, &p.token_id);
    assert_eq!(tok.balance(&p.lender), 0);
    assert_eq!(tok.balance(&p.originator), amount);

    // ── Step 4: Advance 365 days so pro-rata interest = flat yield ──────────
    env.ledger().set_timestamp(funded_at + 365 * 86_400);

    // ── Step 5: Repay in full (Repayment → Financing → Registry → Reputation)
    let asset = token::StellarAssetClient::new(&env, &p.token_id);
    asset.mint(&p.originator, &total_due);
    let repaid = p
        .rep
        .repay_invoice(&invoice_id, &offer_id, &p.originator, &total_due);
    assert_eq!(repaid.status, InvoiceStatus::Repaid);

    // Offer Repaid in Financing.
    let offer_final = p.fin.get_offer(&offer_id);
    assert_eq!(offer_final.status, OfferStatus::Repaid);

    // Lender received principal + yield.
    assert_eq!(tok.balance(&p.lender), total_due);

    // Reputation: success recorded.
    assert_eq!(p.repu.get_score(&p.originator), 1);

    // Protocol stats (from Financing).
    let stats = p.fin.get_stats();
    assert_eq!(stats.total_financed, amount);
    assert_eq!(stats.total_repaid, total_due);
}
