//! Shared types, constants, and currency registry for InvoFi Soroban contracts.
//!
//! `registry` and `financing` both depend on this crate. The currency → SEP-41
//! token registry lives here so adding a third currency means one
//! `register_currency` call — never a new branch in every money-touching
//! function.

#![no_std]

use soroban_sdk::{contractclient, contracterror, contracttype, symbol_short, Address, Env, Map, Symbol};

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

// ─── Shared Error Enum ────────────────────────────────────────────────────────

/// Structured error type shared across all InvoFi contracts.
///
/// Using `#[contracterror]` causes the Soroban host to encode these as a
/// typed `Error` value in the XDR result, not as an opaque string panic.
/// Clients (SDK, frontend, indexer) can match on the `u32` discriminant
/// without parsing panic messages — which breaks across contract versions.
///
/// Discriminants are **stable** and must never be re-numbered once deployed.
/// Add new variants at the end with a new, higher number.
///
/// Using `env.panic_with_error(&ContractError::X)` keeps all public
/// function signatures identical (`T`, not `Result<T, ContractError>`), so
/// no SDK binding changes are needed.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// Caller is not authorized to perform this action (wrong admin,
    /// wrong originator, wrong lender, etc.).
    Unauthorized = 1,

    /// Requested resource (invoice, offer, rate, etc.) does not exist.
    NotFound = 2,

    /// The operation is not permitted given the resource's current status
    /// (e.g., accepting an already-Financed offer, cancelling a non-Pending
    /// invoice, reclaiming before the grace period).
    InvalidTransition = 3,

    /// The contract is paused; all state-mutating operations are halted
    /// until an admin calls `unpause`.
    Paused = 4,

    /// The caller's balance is insufficient for the requested operation
    /// (e.g., unstaking more than staked, repaying more than is owed).
    InsufficientBalance = 5,

    /// A parameter value falls outside the allowed range or violates a
    /// protocol constraint (e.g., `fee_bps > 500`, `amount <= 0`,
    /// past-due `due_date`).
    InvalidInput = 6,

    /// An entity with the provided ID already exists (invoice, offer).
    AlreadyExists = 7,

    /// The caller's address is on the blacklist.
    Blacklisted = 8,
}

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum InvoiceStatus {
    Pending = 0,
    Financed = 1,
    Repaid = 2,
    Overdue = 3,
    Cancelled = 4,
    Disputed = 5,
    Defaulted = 6,
}

/// A record of a single invoice state transition.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionRecord {
    pub from_status: InvoiceStatus,
    pub to_status: InvoiceStatus,
    pub actor: Address,
    pub timestamp: u64,
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

/// Installment frequency for a fixed repayment schedule.
///
/// `Daily` = 86 400 s between installments, `Weekly` = 604 800 s,
/// `Monthly` = 2 592 000 s (30-day approximation).
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ScheduleFrequency {
    Daily = 0,
    Weekly = 1,
    Monthly = 2,
}

impl ScheduleFrequency {
    /// Returns the period in seconds that corresponds to this frequency.
    pub fn period_secs(self) -> u64 {
        match self {
            ScheduleFrequency::Daily => 86_400,
            ScheduleFrequency::Weekly => 604_800,
            ScheduleFrequency::Monthly => 2_592_000,
        }
    }
}

/// An advisory fixed-installment repayment schedule attached to a financing offer.
///
/// Each installment covers an equal slice of principal plus interest on the
/// remaining principal (flat-rate model):
///
///   installment_principal = offer.amount / count
///   installment_yield     = installment_principal * offer.interest_rate / 10_000
///   installment_amount    = installment_principal + installment_yield
///
/// The schedule is **advisory with enforcement**: ad-hoc partial repayments
/// remain permitted via `repay_invoice` and will never corrupt schedule state
/// — `amount_repaid` on the offer is always the source of truth for how much
/// has actually been cleared.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepaymentSchedule {
    pub offer_id: Symbol,
    /// Number of equal installments.
    pub count: u32,
    /// Seconds between installments.
    pub frequency: ScheduleFrequency,
    /// Amount due per installment (principal slice + yield on that slice).
    pub installment_amount: i128,
    /// Unix timestamp of the first installment due date.
    pub first_due: u64,
}

/// A single payment record stored on-chain as part of the payment history
/// for an invoice. Each partial or full repayment creates one record.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentRecord {
    /// Sequential payment identifier (1-based).
    pub payment_id: u32,
    /// Total payment amount (principal + interest combined).
    pub amount: i128,
    /// Portion of the payment applied to accrued interest.
    pub interest_paid: i128,
    /// Portion of the payment applied to outstanding principal.
    pub principal_paid: i128,
    /// Unix timestamp of the payment.
    pub timestamp: u64,
    /// Address that made the payment.
    pub payer: Address,
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
///
/// Coverage matrix for the five audited contracts: every public
/// write/state-changing entrypoint must call this guard before mutating
/// persistent storage or transferring funds. The explicit exceptions are the
/// pause/unpause setters themselves and read-only getter/query functions.
///
/// - Registry:
///   - state-changing: register_invoice, update_invoice_status, update_invoice_amount,
///     cancel_invoice, set_invoice_repaid_status, financing_marks_invoice_financed,
///     repayment_marks_invoice_repaid, repayment_marks_defaulted, mark_invoice_overdue,
///     raise_dispute, resolve_dispute, blacklist_address, unblacklist_address,
///     transfer_admin, set_financing_contract, set_repayment_contract, set_rate,
///     set_fee.
///   - exceptions: pause, unpause, contract_is_paused, getters.
/// - Financing:
///   - state-changing: create_offer, withdraw_offer, accept_offer, reject_offer,
///     update_offer_status, update_offer_amount_repaid, update_lender_stats_repaid,
///     update_stats_repaid, register_currency, set_position_token, set_repayment_contract,
///     transfer_admin.
///   - exceptions: pause, unpause, contract_is_paused, getters.
/// - Repayment:
///   - state-changing: repay_invoice, mark_overdue, reclaim_invoice, set_insurance,
///     set_reputation, set_penalty, transfer_admin.
///   - exceptions: pause, unpause, contract_is_paused, getters.
/// - Insurance:
///   - state-changing: stake, unstake, pay_out, set_staking_token, set_payout_caller,
///     transfer_admin.
///   - exceptions: pause, unpause, contract_is_paused, getters.
/// - Reputation:
///   - state-changing: record_outcome, set_recorder.
///   - exceptions: pause, unpause, contract_is_paused, getters.
pub fn assert_not_paused(env: &Env) {
    let paused: bool = env
        .storage()
        .instance()
        .get(&symbol_short!("paused"))
        .unwrap_or(false);
    if paused {
        env.panic_with_error(ContractError::Paused);
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

    /// System transition: Pending -> Financed, called by the financing
    /// contract on offer acceptance. Authorized via implicit contract-invoker
    /// auth on the registered financing address.
    fn financing_marks_invoice_financed(env: Env, id: Symbol) -> Invoice;

    /// System transition: Financed -> Financed (partial) / Repaid (full),
    /// called by the repayment contract. Authorized via implicit
    /// contract-invoker auth on the registered repayment address.
    fn repayment_marks_invoice_repaid(env: Env, id: Symbol, fully_repaid: bool) -> Invoice;

    /// System transition: Overdue -> Defaulted, called by the repayment
    /// contract when a lender reclaims (declares a default). Authorized via
    /// implicit contract-invoker auth on the registered repayment address.
    fn repayment_marks_defaulted(env: Env, id: Symbol) -> Invoice;

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

// ─── Financing Cross-Contract Interface ──────────────────────────────────────
// Repayment calls these methods on the Financing contract.

/// Client trait for the Financing contract, used by Repayment for
/// cross-contract calls. The `#[contractclient]` macro generates a
/// type-safe client from this trait.
#[contractclient(name = "FinancingClient")]
pub trait FinancingInterface {
    /// Read a financing offer by ID.
    fn get_offer(env: Env, id: Symbol) -> FinancingOffer;

    /// Update the status of an offer. Called by Repayment after accept/reject/
    /// repay/reclaim to keep offer state in sync.
    fn update_offer_status(env: Env, id: Symbol, new_status: OfferStatus);

    /// Update the running amount_repaid on an offer.
    fn update_offer_amount_repaid(env: Env, id: Symbol, amount_repaid: i128);

    /// Update lender stats after a repayment. `fully_repaid` increments
    /// `offers_repaid`.
    fn update_lender_stats_repaid(env: Env, lender: Address, fully_repaid: bool);

    /// Update protocol-level stats after a repayment. Adds to
    /// `total_repaid` and `total_fee_revenue`.
    fn update_stats_repaid(env: Env, amount: i128, fee_amount: i128);

    /// Read the admin address.
    fn get_admin(env: Env) -> Address;

    /// Read the protocol fee in basis points.
    fn get_fee_bps(env: Env) -> u32;

    /// Create a fixed installment repayment schedule for an offer.
    /// `first_due` is the Unix timestamp of the first installment.
    fn schedule_repayment(
        env: Env,
        offer_id: Symbol,
        frequency: ScheduleFrequency,
        count: u32,
        first_due: u64,
    ) -> RepaymentSchedule;

    /// Read the repayment schedule attached to an offer, if any.
    fn get_schedule(env: Env, offer_id: Symbol) -> Option<RepaymentSchedule>;

    /// Return the installment number (1-based) that is currently due (its
    /// due timestamp ≤ now) and has not yet been covered by `amount_repaid`.
    /// Returns 0 when all installments are paid or no schedule exists.
    fn get_installment_due(env: Env, offer_id: Symbol) -> u32;
}

// ─── Insurance Cross-Contract Interface ──────────────────────────────────────
// Repayment calls this method on the Insurance contract when an invoice
// defaults. The insurance contract stores the trusted payout caller (the
// repayment contract) and requires its auth via implicit contract-invoker
// auth, so pay_out can never be invoked by an arbitrary address.

/// Client trait for the Insurance contract, used by Repayment for
/// cross-contract payout calls. `#[contractclient]` generates a type-safe
/// client from this trait.
#[contractclient(name = "InsuranceClient")]
pub trait InsuranceInterface {
    /// Pay `amount` to `beneficiary` from the insurance pool, capped at the
    /// pool's available balance. Only callable by the configured payout
    /// caller (the repayment contract). Verifies on-chain that `invoice_id`
    /// is in `Defaulted` status before moving any funds. Returns the amount
    /// actually paid.
    fn pay_out(env: Env, invoice_id: Symbol, beneficiary: Address, amount: i128) -> i128;

    /// Claim a partial payout from the insurance pool for a specific offer.
    /// The claim amount is bounded by the reserved amount for this offer and
    /// the pool's available balance. Returns (paid, remaining_reserved).
    fn claim_payout(
        env: Env,
        offer_id: Symbol,
        lender: Address,
        amount: i128,
    ) -> (i128, i128);
}

// ─── Reputation Cross-Contract Interface ─────────────────────────────────────
// Repayment records an originator's outcome on the Reputation contract after
// every fully-repaid invoice (success) and every default. The reputation
// contract stores the trusted recorder (the repayment contract) and requires
// its auth via implicit contract-invoker auth.

/// Client trait for the Reputation contract, used by Repayment for
/// cross-contract outcome recording. `#[contractclient]` generates a
/// type-safe client from this trait.
#[contractclient(name = "ReputationClient")]
pub trait ReputationInterface {
    /// Record an outcome for an originator. Only callable by the configured
    /// recorder (the repayment contract). `outcome` is 0 = successful full
    /// repayment, 1 = default.
    fn record_outcome(env: Env, originator: Address, outcome: u32);

    /// Read an originator's current reputation score (public, read-only).
    fn get_score(env: Env, originator: Address) -> i128;
}

// ─── State Machine State Validation and Enforcement ──────────────────────────

use soroban_sdk::Vec;

/// Validates and executes an invoice state transition. Emits a structured 
/// transition event and records the transition in the history log.
/// 
/// This is the single point of authority for all invoice status changes across
/// the protocol. All entry points (registry, financing, repayment) must route
/// through this function.
/// 
/// # Panics
/// - If the transition is invalid for the current state
/// - If the transition would violate business rules (e.g., due_date check for Overdue)
pub fn assert_transition(
    env: &Env,
    invoice_id: Symbol,
    from_status: InvoiceStatus,
    to_status: InvoiceStatus,
    actor: Address,
) {
    // Validate that this transition is in the allowed table
    validate_transition(from_status, to_status);

    // Emit structured transition event
    env.events().publish(
        (symbol_short!("inv_trx"), invoice_id.clone()),
        (from_status, to_status, actor.clone()),
    );

    // Record transition in bounded history (max 20 entries)
    record_transition(env, invoice_id, from_status, to_status, actor);
}

/// Validates that a transition from `from_status` to `to_status` is allowed.
/// 
/// Valid transitions:
/// - Pending -> Cancelled (originator cancellation)
/// - Pending -> Financed (offer acceptance)
/// - Financed -> Repaid (full repayment)
/// - Financed -> Financed (partial repayment, no-op state-wise)
/// - Financed -> Overdue (time-based, after due_date)
/// - Financed -> Disputed (originator dispute)
/// - Overdue -> Defaulted (lender reclaim after grace period)
/// - Disputed -> Pending (admin resolution to Pending)
/// - Disputed -> Financed (admin resolution to Financed)
/// - Disputed -> Repaid (admin resolution to Repaid)
/// - Disputed -> Cancelled (admin resolution to Cancelled)
/// - Disputed -> Defaulted (admin resolution to Defaulted)
fn validate_transition(from_status: InvoiceStatus, to_status: InvoiceStatus) {
    let valid = matches!(
        (from_status, to_status),
        // Pending exits
        (InvoiceStatus::Pending, InvoiceStatus::Cancelled)
        | (InvoiceStatus::Pending, InvoiceStatus::Financed)
        // Financed exits
        | (InvoiceStatus::Financed, InvoiceStatus::Repaid)
        | (InvoiceStatus::Financed, InvoiceStatus::Financed)
        | (InvoiceStatus::Financed, InvoiceStatus::Overdue)
        | (InvoiceStatus::Financed, InvoiceStatus::Disputed)
        // Overdue exits
        | (InvoiceStatus::Overdue, InvoiceStatus::Defaulted)
        // Disputed exits (admin-controlled)
        | (InvoiceStatus::Disputed, InvoiceStatus::Pending)
        | (InvoiceStatus::Disputed, InvoiceStatus::Financed)
        | (InvoiceStatus::Disputed, InvoiceStatus::Repaid)
        | (InvoiceStatus::Disputed, InvoiceStatus::Cancelled)
        | (InvoiceStatus::Disputed, InvoiceStatus::Defaulted)
    );

    if !valid {
        panic!("Invalid state transition");
    }
}

/// Records a transition in the bounded history log (max 20 entries, FIFO eviction).
fn record_transition(
    env: &Env,
    invoice_id: Symbol,
    from_status: InvoiceStatus,
    to_status: InvoiceStatus,
    actor: Address,
) {
    let storage_key = symbol_short!("trn_log");
    let mut history: Vec<TransitionRecord> = env
        .storage()
        .persistent()
        .get(&(storage_key.clone(), invoice_id.clone()))
        .unwrap_or_else(|| Vec::new(env));

    let record = TransitionRecord {
        from_status,
        to_status,
        actor,
        timestamp: env.ledger().timestamp(),
    };

    history.push_back(record);

    // Evict oldest entry if we exceed max 20
    if history.len() > 20 {
        history.pop_front();
    }

    env.storage()
        .persistent()
        .set(&(storage_key, invoice_id), &history);
}

/// Query the full transition history for an invoice.
pub fn get_transition_history(env: &Env, invoice_id: Symbol) -> Vec<TransitionRecord> {
    env.storage()
        .persistent()
        .get(&(symbol_short!("trn_log"), invoice_id))
        .unwrap_or_else(|| Vec::new(env))
}
