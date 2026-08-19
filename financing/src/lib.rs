#![no_std]
// create_offer takes 8 args — that's the contract's public ABI, and the lint
// also fires inside the #[contractimpl]-generated client where we can't allow it.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Map, Symbol, Vec};

use invofi_common::{
    assert_not_paused, check_invoice_version, resolve_token, ContractError, FinancingOffer,
    Invoice, InvoiceStatus, LenderStats, OfferStatus, ProtocolStats, RegistryClient,
    RepaymentSchedule, ScheduleFrequency, MAX_OFFER_DURATION_SECS, MIN_OFFER_DURATION_SECS,
};

// ─── Storage Helpers ─────────────────────────────────────────────────────────

fn load_offers(env: &Env) -> Map<Symbol, FinancingOffer> {
    env.storage()
        .persistent()
        .get(&symbol_short!("offers"))
        .unwrap_or(Map::new(env))
}

fn save_offers(env: &Env, map: &Map<Symbol, FinancingOffer>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("offers"), map);
}

fn load_lender_stats(env: &Env, lender: &Address) -> LenderStats {
    env.storage()
        .persistent()
        .get(&(symbol_short!("lstats"), lender.clone()))
        .unwrap_or_default()
}

fn save_lender_stats(env: &Env, lender: &Address, stats: &LenderStats) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("lstats"), lender.clone()), stats);
}

fn load_stats(env: &Env) -> ProtocolStats {
    env.storage()
        .instance()
        .get(&symbol_short!("stats"))
        .unwrap_or(ProtocolStats {
            total_invoices: 0,
            total_offers: 0,
            total_financed: 0,
            total_repaid: 0,
            total_fee_revenue: 0,
        })
}

fn save_stats(env: &Env, s: &ProtocolStats) {
    env.storage().instance().set(&symbol_short!("stats"), s);
}

// ─── Schedule Storage Helpers ─────────────────────────────────────────────────

fn load_schedules(env: &Env) -> Map<Symbol, RepaymentSchedule> {
    env.storage()
        .persistent()
        .get(&symbol_short!("scheds"))
        .unwrap_or(Map::new(env))
}

fn save_schedules(env: &Env, map: &Map<Symbol, RepaymentSchedule>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("scheds"), map);
}

fn assert_not_blacklisted(env: &Env, address: &Address) {
    // Cross-contract: check blacklist via the registry contract
    let registry_addr: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("registry"))
        .unwrap_or_else(|| panic!("Not initialized"));
    let registry_client = RegistryClient::new(env, &registry_addr);
    // CEI: Read-only cross-contract call, safe before state mutations.
    if registry_client.is_blacklisted(address) {
        env.panic_with_error(ContractError::Blacklisted);
    }
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct FinancingContract;

#[contractimpl]
impl FinancingContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// One-time setup. Stores the admin, the registry address (for
    /// cross-contract calls) and the default settlement token.
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run (issue #75) — a
    /// fresh deployment can never be hijacked by a third party setting
    /// themselves as admin.
    pub fn __constructor(env: Env, admin: Address, registry: Address, token: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("Already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("registry"), &registry);
        env.storage()
            .instance()
            .set(&symbol_short!("token"), &token);
    }

    /// Register the repayment contract address. Only admin.
    /// The repayment contract is the only caller authorized to invoke
    /// callback methods (update_offer_status, update_offer_amount_repaid, etc.).
    pub fn set_repayment_contract(env: Env, admin: Address, repayment: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("repayment"), &repayment);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    pub fn get_registry(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Transfers admin rights. Only current admin.
    pub fn transfer_admin(env: Env, admin: Address, new_admin: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &new_admin);
    }

    // ── Currency registry ────────────────────────────────────────────────────

    /// Register a currency → token mapping. Admin only.
    pub fn register_currency(env: Env, admin: Address, currency: Symbol, token_addr: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        invofi_common::register_currency(&env, &currency, &token_addr);
    }

    pub fn get_currency_token(env: Env, currency: Symbol) -> Option<Address> {
        invofi_common::get_currency_token(&env, &currency)
    }

    // ── Position tokens (Task 7) ─────────────────────────────────────────────

    /// Configure the SEP-41 position-token contract that represents lenders'
    /// claims on financed invoices. Admin only.
    ///
    /// The token contract MUST be initialized with this financing contract as
    /// its admin, so accept_offer can mint claim tokens on the lender's
    /// behalf (the token's admin.require_auth() resolves via implicit
    /// contract-invoker auth — see ADR-0002).
    pub fn set_position_token(env: Env, admin: Address, token: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("postok"), &token);
    }

    /// Returns the configured position-token contract, if any.
    pub fn get_position_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("postok"))
    }

    // ── Pause / unpause ──────────────────────────────────────────────────────

    pub fn pause(env: Env, admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &true);
    }

    pub fn unpause(env: Env, admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &false);
    }

    pub fn contract_is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("paused"))
            .unwrap_or(false)
    }

    // ── Offer CRUD ───────────────────────────────────────────────────────────

    /// Create a financing offer on an invoice. Only the lender can call this.
    pub fn create_offer(
        env: Env,
        offer_id: Symbol,
        invoice_id: Symbol,
        lender: Address,
        amount: i128,
        currency: Symbol,
        interest_rate: u32,
        duration: u64,
    ) -> FinancingOffer {
        assert_not_paused(&env);
        lender.require_auth();
        assert_not_blacklisted(&env, &lender);
        assert!(amount > 0, "offer amount must be greater than zero");
        assert!(interest_rate > 0, "interest_rate must be greater than zero");
        assert!(
            interest_rate <= 10_000,
            "interest_rate must be at most 10000 bps"
        );
        assert!(
            duration >= MIN_OFFER_DURATION_SECS,
            "duration must be at least 1 day (86400 seconds)"
        );
        assert!(
            duration <= MAX_OFFER_DURATION_SECS,
            "duration must be at most 365 days"
        );

        // Cross-contract call: verify invoice exists, is Pending, and lender != originator
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&invoice_id);
        assert!(
            invoice.status == InvoiceStatus::Pending,
            "Invoice must be Pending to accept offers"
        );
        assert!(
            lender != invoice.originator,
            "lender cannot finance their own invoice"
        );

        let mut offers = load_offers(&env);
        if offers.contains_key(offer_id.clone()) {
            env.panic_with_error(ContractError::AlreadyExists);
        }

        let offer = FinancingOffer {
            id: offer_id.clone(),
            invoice_id,
            lender,
            amount,
            currency,
            interest_rate,
            duration,
            status: OfferStatus::Pending,
            funded_at: 0,
            amount_repaid: 0,
        };
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        let mut s = load_stats(&env);
        s.total_offers += 1;
        save_stats(&env, &s);

        let mut lstats = load_lender_stats(&env, &offer.lender);
        lstats.total_offered += offer.amount;
        lstats.offers_pending += 1;
        save_lender_stats(&env, &offer.lender, &lstats);

        env.events().publish(
            (symbol_short!("off_new"), offer.id.clone()),
            (
                offer.invoice_id.clone(),
                offer.lender.clone(),
                amount,
                interest_rate,
            ),
        );
        offer
    }

    /// Get a financing offer by ID.
    pub fn get_offer(env: Env, id: Symbol) -> FinancingOffer {
        load_offers(&env)
            .get(id)
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound))
    }

    /// Withdraw a pending offer. Only the lender.
    pub fn withdraw_offer(env: Env, offer_id: Symbol, lender: Address) -> FinancingOffer {
        assert_not_paused(&env);
        lender.require_auth();
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.lender != lender {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        offer.status = OfferStatus::Rejected;
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);
        env.events().publish(
            (symbol_short!("off_wdr"), offer.id.clone()),
            offer.lender.clone(),
        );
        offer
    }

    // ── Accept / Reject ──────────────────────────────────────────────────────

    /// Accept a financing offer. Only the invoice originator.
    /// Cross-contract: reads + updates invoice status in the registry contract.
    pub fn accept_offer(env: Env, offer_id: Symbol, invoice_originator: Address, expected_version: u64) -> FinancingOffer {
        assert_not_paused(&env);
        invoice_originator.require_auth();

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: read invoice from registry
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);

        if invoice.originator != invoice_originator {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Optimistic-concurrency guard (issue #110): reject if another
        // transaction has already mutated the invoice since the caller read it.
        // The version supplied by the caller must match the stored version;
        // if not, the registry will have already bumped it and this fires.
        check_invoice_version(&env, invoice.version, expected_version);

        // Pull the lender's principal and pay it straight to the business.
        let token_id = resolve_token(&env, &offer.currency);
        let token_client = token::TokenClient::new(&env, &token_id);
        // CEI: External interaction before state writes. Safe because token is a standard Soroban token without reentrant hooks.
        token_client.transfer_from(
            &env.current_contract_address(),
            &offer.lender,
            &invoice.originator,
            &offer.amount,
        );

        offer.status = OfferStatus::Accepted;
        offer.funded_at = env.ledger().timestamp();
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        // Cross-contract: mark the invoice Financed in the registry via the
        // system transition (the financing contract is the authorized caller;
        // user auth does not propagate across contract boundaries in Soroban).
        // CEI: External interaction before local state writes (stats). Safe because registry is a trusted protocol contract.
        registry_client.financing_marks_invoice_financed(&offer.invoice_id);

        // Mint the lender's position token representing their claim on this
        // financed invoice (1:1 with the offer amount — see ADR-0002). The
        // token's admin is this financing contract, so the mint resolves via
        // implicit contract-invoker auth. If no position token is configured
        // (legacy deployments), financing still works unchanged.
        if let Some(pos_token) = env.storage().instance().get(&symbol_short!("postok")) {
            // SDK 22's StellarAssetInterface exposes mint(to, amount); the
            // token contract authorizes its admin (this financing contract)
            // internally via require_auth, which resolves through implicit
            // contract-invoker auth when we call it cross-contract.
            let pos_client = token::StellarAssetClient::new(&env, &pos_token);
            // CEI: External interaction before local state writes. Safe because pos_token is a trusted admin-configured token.
            pos_client.mint(&offer.lender, &offer.amount);
            env.events().publish(
                (symbol_short!("pos_mint"), offer.id.clone()),
                (offer.lender.clone(), offer.amount),
            );
        }

        let mut s = load_stats(&env);
        s.total_financed += offer.amount;
        save_stats(&env, &s);

        let mut lstats = load_lender_stats(&env, &offer.lender);
        lstats.total_accepted += offer.amount;
        save_lender_stats(&env, &offer.lender, &lstats);

        env.events().publish(
            (symbol_short!("off_acc"), offer.id.clone()),
            (offer.invoice_id.clone(), offer.lender.clone(), offer.amount),
        );
        offer
    }

    /// Reject a financing offer. Only the invoice originator.
    pub fn reject_offer(env: Env, offer_id: Symbol, invoice_originator: Address) -> FinancingOffer {
        assert_not_paused(&env);
        invoice_originator.require_auth();

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: verify invoice originator
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);
        if invoice.originator != invoice_originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        offer.status = OfferStatus::Rejected;
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        env.events().publish(
            (symbol_short!("off_rej"), offer.id.clone()),
            offer.invoice_id.clone(),
        );
        offer
    }

    // ── Cross-contract callback methods (called by Repayment) ───────────

    fn assert_only_repayment(env: &Env) {
        let repayment_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("repayment"))
            .unwrap_or_else(|| panic!("Repayment contract not configured"));
        repayment_addr.require_auth();
    }

    /// Update the status of an offer. Called by the Repayment contract
    /// after accept/reject/repay/reclaim to keep offer state in sync.
    pub fn update_offer_status(env: Env, id: Symbol, new_status: OfferStatus) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        offer.status = new_status;
        offers.set(id, offer);
        save_offers(&env, &offers);
    }

    /// Update the running amount_repaid on an offer. Called by Repayment.
    pub fn update_offer_amount_repaid(env: Env, id: Symbol, amount_repaid: i128) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        offer.amount_repaid = amount_repaid;
        offers.set(id, offer);
        save_offers(&env, &offers);
    }

    /// Update lender stats after a repayment. Called by Repayment.
    pub fn update_lender_stats_repaid(env: Env, lender: Address, fully_repaid: bool) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut lstats = load_lender_stats(&env, &lender);
        if fully_repaid {
            lstats.offers_repaid += 1;
        }
        save_lender_stats(&env, &lender, &lstats);
    }

    /// Update protocol-level stats after a repayment. Called by Repayment.
    pub fn update_stats_repaid(env: Env, amount: i128, fee_amount: i128) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut s = load_stats(&env);
        s.total_repaid += amount;
        s.total_fee_revenue += fee_amount;
        save_stats(&env, &s);
    }

    /// Read the protocol fee in basis points. Called by Repayment.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("feebps"))
            .unwrap_or(0)
    }

    // ── Query helpers ────────────────────────────────────────────────────────

    pub fn get_offers_by_invoice(env: Env, invoice_id: Symbol) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.invoice_id == invoice_id {
                result.push_back(offer);
            }
        }
        result
    }

    pub fn get_offers_by_lender(env: Env, lender: Address) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.lender == lender {
                result.push_back(offer);
            }
        }
        result
    }

    /// Return all financing offers. Admin-only analytics function.
    /// At scale, prefer paginated queries — this returns an unbounded Vec.
    pub fn get_all_offers(env: Env) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            result.push_back(offer);
        }
        result
    }

    pub fn get_offers_by_status(env: Env, status: OfferStatus) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.status == status {
                result.push_back(offer);
            }
        }
        result
    }

    pub fn get_pending_offers_by_invoice(env: Env, invoice_id: Symbol) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.invoice_id == invoice_id && offer.status == OfferStatus::Pending {
                result.push_back(offer);
            }
        }
        result
    }

    pub fn get_offers_count(env: Env) -> u32 {
        load_offers(&env).len()
    }

    pub fn get_offers_paginated(env: Env, offset: u32, limit: u32) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (idx, (_id, offer)) in offers.iter().enumerate() {
            if idx as u32 >= offset && result.len() < limit {
                result.push_back(offer);
            }
            if result.len() >= limit {
                break;
            }
        }
        result
    }

    pub fn get_lender_stats(env: Env, lender: Address) -> LenderStats {
        load_lender_stats(&env, &lender)
    }

    pub fn get_lender_active_total(env: Env, lender: Address) -> i128 {
        let offers = load_offers(&env);
        let mut total: i128 = 0;
        for (_id, offer) in offers.iter() {
            if offer.lender == lender
                && (offer.status == OfferStatus::Accepted || offer.status == OfferStatus::Financed)
            {
                total += offer.amount;
            }
        }
        total
    }

    pub fn get_stats(env: Env) -> ProtocolStats {
        load_stats(&env)
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    pub fn get_offer_duration_limits(_env: Env) -> (u64, u64) {
        (MIN_OFFER_DURATION_SECS, MAX_OFFER_DURATION_SECS)
    }

    // ── Repayment schedules (issue #133) ─────────────────────────────────────

    /// Attach a fixed installment repayment schedule to a financing offer.
    ///
    /// Can be called by the **lender** on a Pending offer (before acceptance)
    /// or by the **originator** on a Financed/Accepted offer (after acceptance,
    /// to formalise a payment plan). Both parties must provide their auth;
    /// the contract checks that the caller is the lender or the invoice
    /// originator.
    ///
    /// Installment math (flat-rate, equal principal slices):
    ///
    ///   installment_principal = offer.amount / count
    ///   installment_yield     = installment_principal × interest_rate / 10_000
    ///   installment_amount    = installment_principal + installment_yield
    ///
    /// `first_due` must be in the future. `count` must be ≥ 1 and ≤ 1 200
    /// (100 years of daily installments — a hard cap that prevents overflow).
    ///
    /// An existing schedule is overwritten — either party can reschedule while
    /// the offer is still active.
    pub fn schedule_repayment(
        env: Env,
        offer_id: Symbol,
        caller: Address,
        frequency: ScheduleFrequency,
        count: u32,
        first_due: u64,
    ) -> RepaymentSchedule {
        assert_not_paused(&env);
        caller.require_auth();

        assert!(count >= 1, "count must be at least 1");
        assert!(count <= 1_200, "count must be at most 1200 installments");
        assert!(
            first_due > env.ledger().timestamp(),
            "first_due must be in the future"
        );

        let offers = load_offers(&env);
        let offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        // Only allow scheduling on live (non-terminal) offers.
        if offer.status == OfferStatus::Rejected
            || offer.status == OfferStatus::Repaid
            || offer.status == OfferStatus::Defaulted
        {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // The caller must be the lender or the invoice originator.
        // Retrieve the originator via the registry cross-contract call.
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);

        if caller != offer.lender && caller != invoice.originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        // installment_principal = floor(amount / count)
        // We use floor division; any leftover cent is borne by the last
        // installment but the helper only reports whole installments — that
        // is explicitly in-scope per the issue.
        let installment_principal = offer.amount / (count as i128);
        let installment_yield =
            installment_principal * (offer.interest_rate as i128) / 10_000;
        let installment_amount = installment_principal + installment_yield;

        assert!(
            installment_amount > 0,
            "installment amount must be greater than zero"
        );

        let schedule = RepaymentSchedule {
            offer_id: offer_id.clone(),
            count,
            frequency,
            installment_amount,
            first_due,
        };

        let mut schedules = load_schedules(&env);
        schedules.set(offer_id, schedule.clone());
        save_schedules(&env, &schedules);

        schedule
    }

    /// Read the repayment schedule attached to an offer, if any.
    pub fn get_schedule(env: Env, offer_id: Symbol) -> Option<RepaymentSchedule> {
        load_schedules(&env).get(offer_id)
    }

    /// Return the 1-based index of the installment that is **currently due**
    /// (its due timestamp ≤ `now`) and whose principal has not yet been
    /// covered by `amount_repaid` on the offer.
    ///
    /// Returns `0` when:
    /// - No schedule exists for this offer, or
    /// - All installments have been paid, or
    /// - No installment is due yet (`now` < `first_due`).
    ///
    /// This is the keeper/CLI-friendly read helper described in issue #133.
    pub fn get_installment_due(env: Env, offer_id: Symbol) -> u32 {
        let schedules = load_schedules(&env);
        let schedule = match schedules.get(offer_id.clone()) {
            Some(s) => s,
            None => return 0,
        };

        let offers = load_offers(&env);
        let offer = match offers.get(offer_id) {
            Some(o) => o,
            None => return 0,
        };

        // Terminal offer — nothing due.
        if offer.status == OfferStatus::Repaid || offer.status == OfferStatus::Defaulted {
            return 0;
        }

        let now = env.ledger().timestamp();
        if now < schedule.first_due {
            return 0;
        }

        let period = schedule.frequency.period_secs();
        // How many installments have elapsed (1-based)?
        let elapsed = ((now - schedule.first_due) / period + 1).min(schedule.count as u64) as u32;

        // How many installments are already covered by amount_repaid?
        let paid_count = if schedule.installment_amount == 0 {
            schedule.count
        } else {
            (offer.amount_repaid / schedule.installment_amount) as u32
        };

        if paid_count >= elapsed {
            // All elapsed installments already paid.
            0
        } else {
            // The next unpaid installment that is already due.
            paid_count + 1
        }
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
