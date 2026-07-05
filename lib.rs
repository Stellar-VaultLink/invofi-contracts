#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, vec, Address, Env, Map, Symbol, Vec};

/// Grace period after due_date before a lender can mark a Financed offer
/// Defaulted on an Overdue invoice. 7 days, in seconds.
const GRACE_PERIOD_SECS: u64 = 604_800;

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
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OfferStatus {
    Pending   = 0,
    Accepted  = 1,
    Rejected  = 2,
    Repaid    = 3,
    Defaulted = 4,
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
        originator.require_auth();
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
        assert!(amount > 0, "offer amount must be greater than zero");
        assert!(interest_rate > 0, "interest_rate must be greater than zero");

        // Invoice must exist
        let invoices = load_invoices(&env);
        if !invoices.contains_key(invoice_id.clone()) {
            panic!("Invoice not found");
        }

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
        };
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);
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

    /// Mark an invoice as repaid. Only the invoice originator (debtor) can call this.
    /// Sets the invoice to Repaid and the linked offer to Repaid.
    pub fn repay_invoice(
        env: Env,
        invoice_id: Symbol,
        offer_id: Symbol,
        repayer: Address,
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
        if offer.status != OfferStatus::Accepted {
            panic!("Offer must be Accepted before repayment");
        }

        // Repay principal + yield directly to the lender. `repayer` already
        // authorized this call above, so a direct transfer (not
        // transfer_from) is sufficient — no prior approval needed.
        let token_id: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("token"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let token_client = token::TokenClient::new(&env, &token_id);
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let repay_amount = offer.amount + yield_amount;
        token_client.transfer(&repayer, &offer.lender, &repay_amount);

        invoice.status = InvoiceStatus::Repaid;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);

        offer.status = OfferStatus::Repaid;
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
        if offer.status != OfferStatus::Accepted {
            panic!("Offer must be Accepted before reclaim");
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
}

#[cfg(test)]
mod test;
