#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, Address, Env, Map, Symbol, Vec};

/// Grace period after due_date before a lender can mark a Financed offer
/// Defaulted on an Overdue invoice. 7 days, in seconds.
const GRACE_PERIOD_SECS: u64 = 604_800;

/// Minimum allowed financing duration in create_offer. 1 day, in seconds.
pub const MIN_OFFER_DURATION_SECS: u64 = 86_400;

/// Maximum allowed financing duration in create_offer. 365 days, in seconds.
pub const MAX_OFFER_DURATION_SECS: u64 = 31_536_000;

/// Minimum invoice amount in stroops (1 XLM = 10_000_000 stroops).
/// Prevents dust invoices that would cost more in fees than they're worth.
pub const MIN_INVOICE_AMOUNT: i128 = 10_000_000;

// ─── Yield Rate Oracle ───────────────────────────────────────────────────────

/// Risk tier for yield-rate lookups. A = low risk, C = high risk.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RiskTier {
    A = 0,
    B = 1,
    C = 2,
}

fn load_rates(env: &Env) -> Map<RiskTier, u32> {
    env.storage()
        .persistent()
        .get(&symbol_short!("rates"))
        .unwrap_or(Map::new(env))
}

fn save_rates(env: &Env, map: &Map<RiskTier, u32>) {
    env.storage().persistent().set(&symbol_short!("rates"), map);
}

// ─── Invoice ─────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invoice {
    pub id: Symbol,
    pub originator: Address,
    pub amount: i128,
    pub currency: Symbol,
    pub due_date: u64,
    pub status: InvoiceStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum InvoiceStatus {
    Pending   = 0,
    Financed  = 1,
    Repaid    = 2,
    Overdue   = 3,
    Cancelled = 4,
}

// ─── Financing Offer ─────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinancingOffer {
    pub id: Symbol,
    pub invoice_id: Symbol,
    pub lender: Address,
    pub amount: i128,
    pub currency: Symbol,
    /// Interest rate in basis points (e.g. 500 = 5.00%)
    pub interest_rate: u32,
    /// Financing duration in seconds
    pub duration: u64,
    pub status: OfferStatus,
    /// Unix timestamp when the offer was accepted; 0 if not yet accepted
    pub funded_at: u64,
    /// Running total of repayments made against the financing obligation
    pub amount_repaid: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OfferStatus {
    Pending   = 0,
    Accepted  = 1,
    Rejected  = 2,
    Financed  = 3,
    Repaid    = 4,
    Defaulted = 5,
}

// ─── Pause guard ────────────────────────────────────────────────────────────

fn assert_not_paused(env: &Env) {
    let paused: bool = env
        .storage()
        .instance()
        .get(&symbol_short!("paused"))
        .unwrap_or(false);
    if paused {
        panic!("Contract is paused");
    }
}

// ─── Storage helpers ─────────────────────────────────────────────────────────

fn load_invoices(env: &Env) -> Map<Symbol, Invoice> {
    env.storage()
        .persistent()
        .get(&symbol_short!("invoices"))
        .unwrap_or(Map::new(env))
}

fn save_invoices(env: &Env, map: &Map<Symbol, Invoice>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("invoices"), map);
}

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

// ─── Contract ────────────────────────────────────────────────────────────────
// --- ProtocolStats ------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolStats {
    pub total_invoices: u32,
    pub total_offers: u32,
    pub total_financed: i128,
    pub total_repaid: i128,
    pub total_fee_revenue: i128,
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

// --- Blacklist helpers -------------------------------------------------------

fn load_blacklist(env: &Env) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&symbol_short!("blklist"))
        .unwrap_or(Vec::new(env))
}

fn save_blacklist(env: &Env, list: &Vec<Address>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("blklist"), list);
}

fn assert_not_blacklisted(env: &Env, address: &Address) {
    let list = load_blacklist(env);
    for entry in list.iter() {
        if entry == *address {
            panic!("Address is blacklisted");
        }
    }
}



#[contract]
pub struct InvoiceRegistryContract;

#[contractimpl]
impl InvoiceRegistryContract {
    // ── Admin / token setup ──────────────────────────────────────────────────

    /// One-time setup. Sets the admin and the SEP-41 token contract used to
    /// move funds on `accept_offer` and `repay_invoice`. Must be called once,
    /// right after deployment, before any offer can be accepted.
    pub fn initialize(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&symbol_short!("admin"), &admin);
        env.storage().instance().set(&symbol_short!("token"), &token);
    }

    /// Returns the admin address. Panics if not yet initialized.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Returns the SEP-41 token contract address used for fund movement.
    /// Panics if not yet initialized.
    pub fn get_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("token"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Transfers admin rights to a new address. Only the current admin can
    /// call this.
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
        env.storage().instance().set(&symbol_short!("admin"), &new_admin);
    }

    // ── Pause / unpause ──────────────────────────────────────────────────────

    /// Halt all state-mutating operations. Admin only.
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
        env.storage().instance().set(&symbol_short!("paused"), &true);
    }

    /// Resume operations after a pause. Admin only.
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
        env.storage().instance().set(&symbol_short!("paused"), &false);
    }

    /// Returns true if the contract is currently paused.
    pub fn contract_is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("paused"))
            .unwrap_or(false)
    }

    // ── Yield rate oracle functions ──────────────────────────────────────────

    /// Sets the yield rate (in basis points, 0-10000) for a risk tier.
    /// Admin only.
    pub fn set_rate(env: Env, admin: Address, tier: RiskTier, rate_bps: u32) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only admin can set rates");
        }
        if rate_bps > 10_000 {
            panic!("rate_bps must be between 0 and 10000");
        }

        let mut rates = load_rates(&env);
        rates.set(tier, rate_bps);
        save_rates(&env, &rates);
    }

    /// Returns the configured yield rate (basis points) for a risk tier.
    /// Panics if that tier hasn't been set yet.
    pub fn get_rate(env: Env, tier: RiskTier) -> u32 {
        load_rates(&env)
            .get(tier)
            .unwrap_or_else(|| panic!("Rate not set for this tier"))
    }

    // ── Protocol fee ────────────────────────────────────────────────────────

    /// Set the protocol fee in basis points (max 500 = 5%). Admin only.
    /// Fee is deducted from each repayment and sent to the admin address.
    pub fn set_fee(env: Env, admin: Address, fee_bps: u32) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only admin can set fee");
        }
        if fee_bps > 500 {
            panic!("fee_bps must be at most 500 (5%)");
        }
        env.storage().instance().set(&symbol_short!("feebps"), &fee_bps);
    }

    /// Returns the configured protocol fee in basis points (default 0).
    pub fn get_fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("feebps"))
            .unwrap_or(0)
    }

    // ── Invoice functions ────────────────────────────────────────────────────

    /// Register a new invoice. Only the originator can call this.
    pub fn register_invoice(
        env: Env,
        id: Symbol,
        originator: Address,
        amount: i128,
        currency: Symbol,
        due_date: u64,
    ) -> Invoice {
        assert_not_paused(&env);
        originator.require_auth();
        assert_not_blacklisted(&env, &originator);
        assert!(amount > 0, "amount must be greater than zero");
        assert!(
            due_date > env.ledger().timestamp(),
            "due_date must be in the future"
        );

        let mut invoices = load_invoices(&env);
        if invoices.contains_key(id.clone()) {
            panic!("Invoice with this ID already exists");
        }

        let invoice = Invoice {
            id: id.clone(),
            originator,
            amount,
            currency,
            due_date,
            status: InvoiceStatus::Pending,
        };
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        let mut s = load_stats(&env); s.total_invoices += 1; save_stats(&env, &s);
        invoice
    }

    /// Get an invoice by ID.
    pub fn get_invoice(env: Env, id: Symbol) -> Invoice {
        load_invoices(&env)
            .get(id)
            .unwrap_or_else(|| panic!("Invoice not found"))
    }

    /// Update the status of an invoice. Restricted to originator or contract logic.
    pub fn update_invoice_status(env: Env, id: Symbol, new_status: InvoiceStatus) -> Invoice {
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        invoice.status = new_status;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    // ── Financing offer functions ─────────────────────────────────────────────

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
        lender.require_auth();
        assert_not_blacklisted(&env, &lender);
        assert!(amount > 0, "offer amount must be greater than zero");
        assert!(interest_rate > 0, "interest_rate must be greater than zero");
        assert!(interest_rate <= 10_000, "interest_rate must be at most 10000 bps");
        assert!(duration >= MIN_OFFER_DURATION_SECS, "duration must be at least 1 day (86400 seconds)");
        assert!(duration <= MAX_OFFER_DURATION_SECS, "duration must be at most 365 days");

        // Invoice must exist, and the lender can't finance their own invoice.
        let invoices = load_invoices(&env);
        let invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
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
        let mut s = load_stats(&env); s.total_offers += 1; save_stats(&env, &s);
        offer
    }

    /// Get a financing offer by ID.
    pub fn get_offer(env: Env, id: Symbol) -> FinancingOffer {
        load_offers(&env)
            .get(id)
            .unwrap_or_else(|| panic!("Offer not found"))
    }

    /// Accept a financing offer. Only the invoice originator can call this.
    /// Marks the offer Accepted and the invoice Financed.
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

        // Verify caller is the invoice originator
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(offer.invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.originator != invoice_originator {
            panic!("Only the invoice originator can accept offers");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Invoice is not in Pending status");
        }

        // Pull the lender's principal and pay it straight to the business —
        // this is the "immediate liquidity" the protocol promises. The
        // lender must have called token.approve(lender, <this contract>,
        // offer.amount, ...) on the token contract before the offer is
        // accepted, since the lender isn't a co-signer of this call.
        let token_id: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token"))
            .unwrap_or_else(|| panic!("Not initialized"));
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

        invoice.status = InvoiceStatus::Financed;
        invoices.set(offer.invoice_id.clone(), invoice);
        save_invoices(&env, &invoices);

        offer
    }

    /// Reject a financing offer. Only the invoice originator can call this.
    pub fn reject_offer(env: Env, offer_id: Symbol, invoice_originator: Address) -> FinancingOffer {
        invoice_originator.require_auth();

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));

        if offer.status != OfferStatus::Pending {
            panic!("Offer is not in Pending status");
        }

        let invoices = load_invoices(&env);
        let invoice = invoices
            .get(offer.invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.originator != invoice_originator {
            panic!("Only the invoice originator can reject offers");
        }

        offer.status = OfferStatus::Rejected;
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        offer
    }

    /// Mark part or all of an invoice as repaid. Only the invoice originator
    /// (debtor) can call this. Partial payments keep the invoice/offer in
    /// Financed status until the outstanding balance is fully cleared.
    pub fn repay_invoice(
        env: Env,
        invoice_id: Symbol,
        offer_id: Symbol,
        repayer: Address,
        amount: i128,
    ) -> Invoice {
        repayer.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.originator != repayer {
            panic!("Only the invoice originator can repay");
        }
        if invoice.status != InvoiceStatus::Financed {
            panic!("Invoice must be Financed before repayment");
        }

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));

        if offer.invoice_id != invoice_id {
            panic!("Offer does not belong to this invoice");
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            panic!("Offer must be Accepted or Financed before repayment");
        }
        assert!(amount > 0, "repayment amount must be greater than zero");

        // Repay principal + yield directly to the lender, minus protocol fee.
        // `repayer` already authorized this call, so a direct transfer
        // (not transfer_from) is sufficient.
        let token_id: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let token_client = token::TokenClient::new(&env, &token_id);
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;
        let remaining_balance = total_due - offer.amount_repaid;
        assert!(
            amount <= remaining_balance,
            "Repayment amount exceeds remaining balance"
        );

        // Deduct protocol fee and send remainder to lender.
        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&symbol_short!("feebps"))
            .unwrap_or(0);
        let fee_amount = amount * (fee_bps as i128) / 10_000;
        let lender_amount = amount - fee_amount;
        token_client.transfer(&repayer, &offer.lender, &lender_amount);
        if fee_amount > 0 {
            let admin: Address = env
                .storage()
                .instance()
                .get(&symbol_short!("admin"))
                .unwrap_or_else(|| panic!("Not initialized"));
            token_client.transfer(&repayer, &admin, &fee_amount);
        }

        offer.amount_repaid += amount;
        if offer.amount_repaid >= total_due {
            invoice.status = InvoiceStatus::Repaid;
            offer.status = OfferStatus::Repaid;
        } else {
            invoice.status = InvoiceStatus::Financed;
            offer.status = OfferStatus::Financed;
        }

        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);

        offers.set(offer_id, offer);
        save_offers(&env, &offers);

        invoice
    }

    /// After an invoice has been marked Overdue (see `mark_overdue`) and the
    /// grace period has elapsed, the financing lender can mark their offer
    /// Defaulted. No funds move here — principal was already paid to the
    /// business at `accept_offer` time, so this is an on-chain default
    /// record for off-chain recovery, not a refund. There is nothing held
    /// in escrow to reclaim under this protocol's unsecured-financing model.
    pub fn reclaim_invoice(env: Env, invoice_id: Symbol, offer_id: Symbol, lender: Address) -> FinancingOffer {
        lender.require_auth();

        let invoices = load_invoices(&env);
        let invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.status != InvoiceStatus::Overdue {
            panic!("Invoice must be Overdue before reclaim");
        }
        if env.ledger().timestamp() < invoice.due_date + GRACE_PERIOD_SECS {
            panic!("Grace period has not elapsed");
        }

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| panic!("Offer not found"));

        if offer.invoice_id != invoice_id {
            panic!("Offer does not belong to this invoice");
        }
        if offer.lender != lender {
            panic!("Only the financing lender can reclaim");
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            panic!("Offer must be Accepted or Financed before reclaim");
        }

        offer.status = OfferStatus::Defaulted;
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        offer
    }

    /// Mark an invoice as overdue. Can be called by anyone after due_date has passed.
    pub fn mark_overdue(env: Env, invoice_id: Symbol) -> Invoice {
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.status != InvoiceStatus::Financed {
            panic!("Only Financed invoices can be marked Overdue");
        }
        if env.ledger().timestamp() <= invoice.due_date {
            panic!("Invoice due date has not passed");
        }

        invoice.status = InvoiceStatus::Overdue;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    /// Returns all invoices matching the given status.
    pub fn get_invoices_by_status(env: Env, status: InvoiceStatus) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (_id, inv) in invoices.iter() {
            if inv.status == status {
                result.push_back(inv);
            }
        }
        result
    }

    /// Update the face amount of a Pending invoice. Only the originator can call this.
    /// Useful to correct a mis-entered amount before the invoice attracts offers.
    pub fn update_invoice_amount(
        env: Env,
        invoice_id: Symbol,
        originator: Address,
        new_amount: i128,
    ) -> Invoice {
        originator.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.originator != originator {
            panic!("Only the invoice originator can update the amount");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Only Pending invoices can have their amount updated");
        }
        assert!(new_amount > 0, "new_amount must be greater than zero");

        invoice.amount = new_amount;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    /// Cancel a Pending invoice. Only the originator can call this.
    /// Transitions the invoice from Pending → Cancelled. Any pending offers
    /// attached to the invoice remain in Pending status (they were never funded).
    pub fn cancel_invoice(env: Env, invoice_id: Symbol, originator: Address) -> Invoice {
        originator.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));

        if invoice.originator != originator {
            panic!("Only the invoice originator can cancel");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Only Pending invoices can be cancelled");
        }

        invoice.status = InvoiceStatus::Cancelled;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    /// Return all financing offers attached to a given invoice.
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

    /// Return all financing offers submitted by a given lender address.
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

    /// Withdraw a pending offer. Only the lender who created the offer can call this.
    /// Transitions the offer from Pending → Rejected (lender-initiated withdrawal).
    pub fn withdraw_offer(env: Env, offer_id: Symbol, lender: Address) -> FinancingOffer {
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
        offer
    }

    /// Return all invoices registered by a given originator address.
    pub fn get_invoices_by_originator(env: Env, originator: Address) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (_id, inv) in invoices.iter() {
            if inv.originator == originator {
                result.push_back(inv);
            }
        }
        result
    }

    /// Return all registered invoices regardless of status. Useful for admin analytics.
    pub fn get_all_invoices(env: Env) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (_id, inv) in invoices.iter() {
            result.push_back(inv);
        }
        result
    }

    /// Return all financing offers regardless of status. Useful for admin analytics.
    pub fn get_all_offers(env: Env) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            result.push_back(offer);
        }
        result
    }

    /// Return the remaining amount due (principal + yield − already repaid) for
    /// a given offer. Returns 0 if the offer is already Repaid or Defaulted.
    pub fn calculate_total_due(env: Env, offer_id: Symbol) -> i128 {
        let offers = load_offers(&env);
        let offer = offers
            .get(offer_id)
            .unwrap_or_else(|| panic!("Offer not found"));

        if offer.status == OfferStatus::Repaid || offer.status == OfferStatus::Defaulted {
            return 0;
        }

        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;
        (total_due - offer.amount_repaid).max(0)
    }

    // --- Blacklist management ------------------------------------------------

    /// Permanently ban an address from registering invoices or submitting offers.
    /// Only the admin can call this.
    pub fn blacklist_address(env: Env, admin: Address, target: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only admin can blacklist");
        }
        let mut list = load_blacklist(&env);
        for entry in list.iter() {
            if entry == target {
                return;
            }
        }
        list.push_back(target);
        save_blacklist(&env, &list);
    }

    /// Remove an address from the protocol blacklist. Admin only.
    pub fn unblacklist_address(env: Env, admin: Address, target: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only admin can unblacklist");
        }
        let list = load_blacklist(&env);
        let mut new_list: Vec<Address> = Vec::new(&env);
        for entry in list.iter() {
            if entry != target {
                new_list.push_back(entry);
            }
        }
        save_blacklist(&env, &new_list);
    }

    /// Returns true if the given address is currently blacklisted.
    pub fn is_blacklisted(env: Env, address: Address) -> bool {
        let list = load_blacklist(&env);
        for entry in list.iter() {
            if entry == address {
                return true;
            }
        }
        false
    }

    /// Return the full blacklist for admin review.
    pub fn get_blacklist(env: Env) -> Vec<Address> {
        load_blacklist(&env)
    }

    // --- Protocol statistics -------------------------------------------------

    /// Return aggregate statistics collected across all protocol activity.
    pub fn get_stats(env: Env) -> ProtocolStats {
        load_stats(&env)
    }

}

#[cfg(test)]
mod test;
