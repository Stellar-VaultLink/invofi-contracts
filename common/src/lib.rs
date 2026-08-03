//! Shared types, constants, and currency registry for InvoFi Soroban contracts.
//!
//! `registry` and `financing` both depend on this crate. The currency → SEP-41
//! token registry lives here so adding a third currency means one
//! `register_currency` call — never a new branch in every money-touching
//! function.

#![no_std]

use soroban_sdk::{contractclient, contracttype, symbol_short, Address, Env, Map, Symbol};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Grace period after due_date before a lender can reclaim on an Overdue
/// invoice. 7 days, in seconds.
pub const GRACE_PERIOD_SECS: u64 = 604_800;

/// Minimum allowed financing duration in `create_offer`. 1 day, in seconds.
pub const MIN_OFFER_DURATION_SECS: u64 = 86_400;

/// Maximum allowed financing duration in `create_offer`. 365 days, in seconds.
pub const MAX_OFFER_DURATION_SECS: u64 = 31_536_000;

/// Minimum invoice amount in stroops (1 XLM = 10_000_000 stroops).
/// Prevents dust invoices that would cost more in fees than they're worth.
pub const MIN_INVOICE_AMOUNT: i128 = 10_000_000;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Risk tier for yield-rate lookups. A = low risk, C = high risk.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RiskTier {
    A = 0,
    B = 1,
    C = 2,
}

/// An invoice registered on-chain.
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

/// Lifecycle status of an invoice.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum InvoiceStatus {
    Pending = 0,
    Financed = 1,
    Repaid = 2,
    Overdue = 3,
    Cancelled = 4,
    Disputed = 5,
}

/// A financing offer submitted by a lender against an invoice.
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

/// Lifecycle status of a financing offer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OfferStatus {
    Pending = 0,
    Accepted = 1,
    Rejected = 2,
    Financed = 3,
    Repaid = 4,
    Defaulted = 5,
}

/// Aggregate protocol statistics.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolStats {
    pub total_invoices: u32,
    pub total_offers: u32,
    pub total_financed: i128,
    pub total_repaid: i128,
    pub total_fee_revenue: i128,
}

/// Per-lender activity statistics.
#[contracttype]
#[derive(Clone, Debug, Default)]
pub struct LenderStats {
    pub total_offered: i128,
    pub total_accepted: i128,
    pub offers_pending: u32,
    pub offers_repaid: u32,
}

// ─── Currency Registry ───────────────────────────────────────────────────────

/// Load the currency registry (an empty map if none has been configured).
pub fn load_currency_registry(env: &Env) -> Map<Symbol, Address> {
    env.storage()
        .instance()
        .get(&symbol_short!("curtok"))
        .unwrap_or_else(|| Map::new(env))
}

/// Persist the currency registry.
pub fn save_currency_registry(env: &Env, registry: &Map<Symbol, Address>) {
    env.storage()
        .instance()
        .set(&symbol_short!("curtok"), registry);
}

/// Register (or overwrite) the SEP-41 token contract that settles `currency`.
/// The admin authorization check lives in the contract entry point that calls
/// this — this module only owns registry state.
pub fn register_currency(env: &Env, currency: &Symbol, token: &Address) {
    let mut registry = load_currency_registry(env);
    registry.set(currency.clone(), token.clone());
    save_currency_registry(env, &registry);
}

/// Look up the token contract registered for `currency`, if any.
pub fn get_currency_token(env: &Env, currency: &Symbol) -> Option<Address> {
    load_currency_registry(env).get(currency.clone())
}

/// Resolve the token contract that moves funds for `currency`.
///
/// Prefers the currency registry; falls back to the legacy single token set
/// at `initialize()` so existing single-currency deployments keep working
/// unchanged. A multi-currency deployment registers each currency once via
/// `register_currency` — no per-function branches.
pub fn resolve_token(env: &Env, currency: &Symbol) -> Address {
    if let Some(addr) = get_currency_token(env, currency) {
        return addr;
    }
    env.storage()
        .instance()
        .get(&symbol_short!("token"))
        .unwrap_or_else(|| panic!("Not initialized"))
}

// ─── Pause Guard ─────────────────────────────────────────────────────────────

/// Panics if the contract is currently paused.
pub fn assert_not_paused(env: &Env) {
    let paused: bool = env
        .storage()
        .instance()
        .get(&symbol_short!("paused"))
        .unwrap_or(false);
    if paused {
        panic!("Contract is paused");
    }
}

// ─── Cross-Contract Interface ────────────────────────────────────────────────
// Financing calls these methods on the Registry contract.

/// Client trait for the Registry contract, used by Financing for
/// cross-contract calls. The `#[contractclient]` macro generates a
/// type-safe client from this trait.
#[contractclient(name = "RegistryClient")]
pub trait RegistryInterface {
    /// Read an invoice by ID.
    fn get_invoice(env: Env, id: Symbol) -> Invoice;

    /// Update the status of a Pending invoice (originator-only escape hatch).
    fn update_invoice_status(
        env: Env,
        id: Symbol,
        originator: Address,
        new_status: InvoiceStatus,
    ) -> Invoice;

    /// Mark a Financed invoice as Overdue. Callable by anyone after due_date.
    fn mark_invoice_overdue(env: Env, id: Symbol) -> Invoice;

    /// Transition a Financed invoice to Repaid or back to Financed (partial).
    /// Requires the repayer's auth. Only works on Financed invoices.
    fn set_invoice_repaid_status(
        env: Env,
        id: Symbol,
        repayer: Address,
        fully_repaid: bool,
    ) -> Invoice;

    /// Check if an address is blacklisted.
    fn is_blacklisted(env: Env, address: Address) -> bool;

    /// Read the admin address.
    fn get_admin(env: Env) -> Address;
}
