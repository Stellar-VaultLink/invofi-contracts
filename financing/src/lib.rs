#![no_std]
// create_offer takes 8 args — that's the contract's public ABI, and the lint
// also fires inside the #[contractimpl]-generated client where we can't allow it.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Map, Symbol, Vec};

use invofi_common::{
    assert_not_paused, resolve_token, FinancingOffer, Invoice, InvoiceStatus, LenderStats,
    OfferStatus, ProtocolStats, RegistryClient, MAX_OFFER_DURATION_SECS, MIN_OFFER_DURATION_SECS,
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

fn assert_not_blacklisted(env: &Env, address: &Address) {
    // Cross-contract: check blacklist via the registry contract
    let registry_addr: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("registry"))
        .unwrap_or_else(|| panic!("Not initialized"));
    let registry_client = RegistryClient::new(env, &registry_addr);
    if registry_client.is_blacklisted(address) {
        panic!("Address is blacklisted");
    }
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct FinancingContract;

#[contractimpl]
impl FinancingContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// Initialize the financing contract. Stores the registry address for
    /// cross-contract calls and the token address for direct token resolution.
    /// Admin is the same as the registry admin.
    pub fn initialize(env: Env, admin: Address, registry: Address, token: Address) {
        admin.require_auth();
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
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can set the repayment contract");
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
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can transfer admin rights");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &new_admin);
    }

    // ── Currency registry ────────────────────────────────────────────────────

    /// Register a currency → token mapping. Admin only.
    pub fn register_currency(env: Env, admin: Address, currency: Symbol, token_addr: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can register currencies");
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
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can set the position token");
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
            panic!("Only admin can pause");
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
            panic!("Only admin can unpause");
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
            panic!("Offer with this ID already exists");
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
            .unwrap_or_else(|| panic!("Offer not found"))
    }

    /// Withdraw a pending offer. Only the lender.
    pub fn withdraw_offer(env: Env, offer_id: Symbol, lender: Address) -> FinancingOffer {
        assert_not_paused(&env);
        lender.require_auth();
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));
        if offer.lender != lender {
            panic!("Only the offer lender can withdraw");
        }
        if offer.status != OfferStatus::Pending {
            panic!("Only Pending offers can be withdrawn");
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
    pub fn accept_offer(env: Env, offer_id: Symbol, invoice_originator: Address) -> FinancingOffer {
        assert_not_paused(&env);
        invoice_originator.require_auth();

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));
        if offer.status != OfferStatus::Pending {
            panic!("Offer is not in Pending status");
        }

        // Cross-contract: read invoice from registry
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);

        if invoice.originator != invoice_originator {
            panic!("Only the invoice originator can accept offers");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Invoice is not in Pending status");
        }

        // Pull the lender's principal and pay it straight to the business.
        let token_id = resolve_token(&env, &offer.currency);
        let token_client = token::TokenClient::new(&env, &token_id);
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
            .unwrap_or_else(|| panic!("Offer not found"));
        if offer.status != OfferStatus::Pending {
            panic!("Offer is not in Pending status");
        }

        // Cross-contract: verify invoice originator
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);
        if invoice.originator != invoice_originator {
            panic!("Only the invoice originator can reject offers");
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
        Self::assert_only_repayment(&env);
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));
        offer.status = new_status;
        offers.set(id, offer);
        save_offers(&env, &offers);
    }

    /// Update the running amount_repaid on an offer. Called by Repayment.
    pub fn update_offer_amount_repaid(env: Env, id: Symbol, amount_repaid: i128) {
        Self::assert_only_repayment(&env);
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));
        offer.amount_repaid = amount_repaid;
        offers.set(id, offer);
        save_offers(&env, &offers);
    }

    /// Update lender stats after a repayment. Called by Repayment.
    pub fn update_lender_stats_repaid(env: Env, lender: Address, fully_repaid: bool) {
        Self::assert_only_repayment(&env);
        let mut lstats = load_lender_stats(&env, &lender);
        if fully_repaid {
            lstats.offers_repaid += 1;
        }
        save_lender_stats(&env, &lender, &lstats);
    }

    /// Update protocol-level stats after a repayment. Called by Repayment.
    pub fn update_stats_repaid(env: Env, amount: i128, fee_amount: i128) {
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
}

#[cfg(test)]
mod test;
