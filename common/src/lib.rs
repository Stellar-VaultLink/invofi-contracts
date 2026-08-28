//! Shared types, constants, and currency registry for InvoFi Soroban contracts.
//!
//! `registry` and `financing` both depend on this crate. The currency → SEP-41
//! token registry lives here so adding a third currency means one
//! `register_currency` call — never a new branch in every money-touching
//! function.

#![no_std]

use soroban_sdk::{
    contractclient, contracterror, contracttype, symbol_short, Address, BytesN, Env, Map, String,
    Symbol, Vec,
};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Grace period after due_date before a lender can reclaim on an Overdue
/// invoice. 7 days, in seconds.
pub const GRACE_PERIOD_SECS: u64 = 604_800;

/// Minimum allowed financing duration in `create_offer`. 1 day, in seconds.
pub const MIN_OFFER_DURATION_SECS: u64 = 86_400;

/// Maximum allowed financing duration in `create_offer`. 365 days, in seconds.
pub const MAX_OFFER_DURATION_SECS: u64 = 31_536_000;

/// Maximum allowed interest rate in `create_offer`, in basis points (100%).
/// Bounds `yield_amount = amount * rate / 10_000` so pathological rates cannot
/// grow the yield arbitrarily large.
pub const MAX_INTEREST_BPS: u32 = 10_000;

/// Minimum invoice amount in stroops (1 XLM = 10_000_000 stroops).
/// Prevents dust invoices that would cost more in fees than they're worth.
pub const MIN_INVOICE_AMOUNT: i128 = 10_000_000;

/// Default negotiation window for offer amendment / counter-offer. 72 hours,
/// in seconds. Admin-configurable per deployment via
/// `set_negotiation_window`.
pub const DEFAULT_NEGOTIATION_WINDOW_SECS: u64 = 259_200;

/// Lower bound an admin may configure the negotiation window to. 1 hour.
/// A window shorter than this makes a good-faith reply impractical.
pub const MIN_NEGOTIATION_WINDOW_SECS: u64 = 3_600;

/// Upper bound an admin may configure the negotiation window to. 30 days.
/// The window bounds how long a recorded counter-offer stays executable, so
/// it must not be settable to an effectively unbounded value.
pub const MAX_NEGOTIATION_WINDOW_SECS: u64 = 2_592_000;

/// Hard cap on negotiation rounds per offer. Every round appends a
/// `NegotiationRecord` to a single persistent `Vec`, so the cap is what keeps
/// that entry's size — and the cost of reading it — bounded.
pub const MAX_NEGOTIATION_ROUNDS: u32 = 20;
/// Default validity period for a verification attestation. 90 days, in
/// seconds. Admin-configurable per deployment via `set_attestation_validity`.
pub const DEFAULT_ATTESTATION_VALIDITY_SECS: u64 = 7_776_000;

/// Lower bound an admin may configure attestation validity to. 1 day.
pub const MIN_ATTESTATION_VALIDITY_SECS: u64 = 86_400;

/// Upper bound an admin may configure attestation validity to. 365 days.
/// An attestation is a snapshot of an off-chain fact that can change without
/// anyone telling the chain, so it must not be settable to never expire.
pub const MAX_ATTESTATION_VALIDITY_SECS: u64 = 31_536_000;

/// Maximum verification fee an admin may configure, in basis points (5%) —
/// the same ceiling `set_fee` applies to the protocol fee.
pub const MAX_VERIFICATION_FEE_BPS: u32 = 500;

/// Maximum size of the trusted verifier set.
pub const MAX_VERIFIERS: u32 = 20;

/// Maximum number of signers an `AdminConfig` may hold. Every threshold
/// check walks the signer set, so this bounds that loop the same way
/// `MAX_VERIFIERS` bounds the verifier one.
pub const MAX_ADMIN_SIGNERS: u32 = 20;

/// Maximum attestations retained per invoice. One per (verifier, type) keeps
/// this at `MAX_VERIFIERS x 3` for the current verifier set; the cap also
/// bounds the residue left behind by verifiers who have since been removed.
pub const MAX_ATTESTATIONS_PER_INVOICE: u32 = 60;

/// The off-chain facts the verification oracle attests to. Used to decide
/// whether an invoice is fully verified — every type must clear the threshold.
pub const VERIFICATION_TYPES: [VerificationType; 3] = [
    VerificationType::DocumentHash,
    VerificationType::BusinessRegistration,
    VerificationType::TaxCompliance,
];

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

    /// A version is not in strict `MAJOR.MINOR.PATCH` form.
    InvalidVersion = 9,

    /// An executable update is awaiting post-upgrade finalization.
    UpgradePending = 10,

    /// No executable update is awaiting post-upgrade finalization.
    UpgradeNotPending = 11,

    /// No retained previous executable is available for rollback.
    RollbackUnavailable = 12,

    /// The offer has passed its expiration deadline and can no longer be accepted.
    OfferExpired = 13,
}

/// Numeric components of a strict `MAJOR.MINOR.PATCH` version.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct SemanticVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    pub version: String,
    pub wasm_hash: BytesN<32>,
}

const VERSION_KEY: Symbol = symbol_short!("__version");
const PREVIOUS_VERSION_KEY: Symbol = symbol_short!("__prevver");
const PREVIOUS_WASM_KEY: Symbol = symbol_short!("__prevwsm");
const PENDING_UPGRADE_KEY: Symbol = symbol_short!("__upgrade");

/// Parses a numeric semantic version in exactly `MAJOR.MINOR.PATCH` form.
pub fn parse_semantic_version(env: &Env, value: &String) -> SemanticVersion {
    let len = value.len() as usize;
    if !(5..=32).contains(&len) {
        env.panic_with_error(ContractError::InvalidVersion);
    }

    let mut bytes = [0u8; 32];
    value.copy_into_slice(&mut bytes[..len]);
    let mut parts = [0u32; 3];
    let mut part = 0usize;
    let mut digits = 0usize;

    for byte in &bytes[..len] {
        if *byte == b'.' {
            if part == 2 || digits == 0 {
                env.panic_with_error(ContractError::InvalidVersion);
            }
            part += 1;
            digits = 0;
        } else {
            if !byte.is_ascii_digit() || (digits > 0 && parts[part] == 0) {
                env.panic_with_error(ContractError::InvalidVersion);
            }
            parts[part] = parts[part]
                .checked_mul(10)
                .and_then(|v| v.checked_add((byte - b'0') as u32))
                .unwrap_or_else(|| env.panic_with_error(ContractError::InvalidVersion));
            digits += 1;
        }
    }
    if part != 2 || digits == 0 {
        env.panic_with_error(ContractError::InvalidVersion);
    }
    SemanticVersion {
        major: parts[0],
        minor: parts[1],
        patch: parts[2],
    }
}

/// Stores the initial version and emits the initialization event. Constructors
/// call this only after their one-time deploy-time setup has succeeded.
pub fn initialize_contract_version(env: &Env, version: &str) {
    let version = String::from_str(env, version);
    parse_semantic_version(env, &version);
    env.storage().instance().set(&VERSION_KEY, &version);
    env.events()
        .publish((Symbol::new(env, "contract_initialized"),), version);
}

/// Returns the version committed by the constructor or a completed upgrade.
pub fn contract_version(env: &Env) -> String {
    env.storage()
        .instance()
        .get(&VERSION_KEY)
        .unwrap_or_else(|| panic!("Not initialized"))
}

/// Validates and records an upgrade before its executable is replaced.
///
/// SDK 22 does not expose the currently-running executable hash. The release
/// transaction therefore supplies it, under the same threshold authorization
/// that is required to schedule the replacement, so it can be retained as the
/// rollback target.
pub fn begin_upgrade(
    env: &Env,
    current_wasm_hash: &BytesN<32>,
    new_wasm_hash: &BytesN<32>,
    new_version: &String,
) {
    if env.storage().instance().has(&PENDING_UPGRADE_KEY) {
        env.panic_with_error(ContractError::UpgradePending);
    }
    let current = contract_version(env);
    if parse_semantic_version(env, new_version) <= parse_semantic_version(env, &current) {
        env.panic_with_error(ContractError::InvalidVersion);
    }
    env.storage()
        .instance()
        .set(&PREVIOUS_VERSION_KEY, &current);
    env.storage()
        .instance()
        .set(&PREVIOUS_WASM_KEY, current_wasm_hash);
    env.storage().instance().set(
        &PENDING_UPGRADE_KEY,
        &PendingUpgrade {
            version: new_version.clone(),
            wasm_hash: new_wasm_hash.clone(),
        },
    );
}

/// Commits an upgrade after the new executable's post-upgrade hook succeeds.
pub fn complete_upgrade(env: &Env) {
    let pending: PendingUpgrade = env
        .storage()
        .instance()
        .get(&PENDING_UPGRADE_KEY)
        .unwrap_or_else(|| env.panic_with_error(ContractError::UpgradeNotPending));
    env.storage().instance().set(&VERSION_KEY, &pending.version);
    env.storage().instance().remove(&PENDING_UPGRADE_KEY);
    env.events()
        .publish((Symbol::new(env, "contract_upgraded"),), pending.version);
}

/// Returns the immediately previous executable and its version. Soroban
/// preserves storage across executable replacement but offers no historical
/// storage snapshot API, so rollback can only restore code and metadata.
pub fn rollback_target(env: &Env) -> (BytesN<32>, String) {
    if env.storage().instance().has(&PENDING_UPGRADE_KEY) {
        env.panic_with_error(ContractError::UpgradePending);
    }
    let wasm_hash = env
        .storage()
        .instance()
        .get(&PREVIOUS_WASM_KEY)
        .unwrap_or_else(|| env.panic_with_error(ContractError::RollbackUnavailable));
    let version = env
        .storage()
        .instance()
        .get(&PREVIOUS_VERSION_KEY)
        .unwrap_or_else(|| env.panic_with_error(ContractError::RollbackUnavailable));
    (wasm_hash, version)
}

/// Commits rollback metadata in the same call that schedules the prior Wasm.
pub fn commit_rollback(env: &Env, version: &String) {
    env.storage().instance().set(&VERSION_KEY, version);
    env.storage().instance().remove(&PREVIOUS_VERSION_KEY);
    env.storage().instance().remove(&PREVIOUS_WASM_KEY);
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
    /// Optional Unix timestamp after which the offer can no longer be accepted.
    /// `0` means the offer never expires (backward compatible).
    pub expires_at: u64,
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

/// Which side of a financing negotiation proposed a set of terms.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NegotiationParty {
    /// The lender who created the offer, via `amend_offer`.
    Lender = 0,
    /// The invoice originator, via `counter_offer`.
    Originator = 1,
}

/// One round of an offer negotiation: the terms one party put on the table,
/// and when. The full `Vec<NegotiationRecord>` for an offer is the on-chain
/// negotiation history.
///
/// `(amount, interest_rate, duration)` is the **canonical term tuple**.
/// Agreement is exact equality of that tuple — there is no rounding or
/// tolerance, so "these are the same terms" is never a judgement call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiationRecord {
    /// Which side proposed these terms.
    pub party: NegotiationParty,
    /// Proposed principal.
    pub amount: i128,
    /// Proposed interest rate in basis points.
    pub interest_rate: u32,
    /// Proposed financing duration in seconds.
    pub duration: u64,
    /// Ledger timestamp at which the round was recorded.
    pub timestamp: u64,
}

/// Lifecycle status of an offer negotiation.
///
/// `Expired` is **derived on read** from the window deadline: Soroban has no
/// scheduler, so nothing flips a negotiation to expired on its own. `Closed`
/// and `Accepted` are persisted, because both are the result of an actual
/// call.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NegotiationStatus {
    /// No negotiation has been opened on this offer.
    None = 0,
    /// Open: within the window, still accepting rounds.
    Open = 1,
    /// The window elapsed without agreement. Terminal.
    Expired = 2,
    /// A party ended the negotiation early. Terminal.
    Closed = 3,
    /// The two sides converged on identical terms and the offer executed.
    /// Terminal.
    Accepted = 4,
}

/// The class of off-chain fact an attestation speaks to.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum VerificationType {
    /// The hash of the invoice document itself matches what the originator
    /// registered off-chain.
    DocumentHash = 0,
    /// The originator is a registered business in good standing.
    BusinessRegistration = 1,
    /// The originator is current on its tax obligations.
    TaxCompliance = 2,
}

/// Verification state of an invoice, or of one verification type on it.
///
/// `Expired` is **derived on read** from `valid_until`: Soroban has no
/// scheduler, so nothing flips an attestation to expired on its own.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum VerificationStatus {
    /// No attestation yet, or not enough of them to clear the threshold.
    Pending = 0,
    /// Enough distinct verifiers attested affirmatively, none of them
    /// expired, and no verifier rejected.
    Verified = 1,
    /// A verifier attested negatively. A live rejection outranks any number
    /// of approvals.
    Rejected = 2,
    /// Every attestation that existed has passed its `valid_until`.
    Expired = 3,
}

/// A verifier's signed statement about one off-chain fact, stored on-chain.
///
/// The contract cannot check that `hash` corresponds to a real invoice
/// document, or that a business registration is genuine. What it does is
/// authenticate that a *trusted verifier* said so and keep the statement
/// tamper-evident and timestamped — see ADR-0009 for that trust boundary.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attestation {
    /// The verifier who submitted it.
    pub verifier: Address,
    /// Which off-chain fact it speaks to.
    pub v_type: VerificationType,
    /// Hash of the off-chain evidence (document, registration record, filing).
    pub hash: BytesN<32>,
    /// Ledger timestamp at which it was submitted.
    pub timestamp: u64,
    /// Ledger timestamp after which it no longer counts.
    pub valid_until: u64,
    /// `Verified` or `Rejected` as submitted; flipped to `Expired` once
    /// `expire_verifications` observes the lapse. Reads derive expiry from
    /// `valid_until` regardless, so this field lagging never makes a read
    /// wrong.
    pub status: VerificationStatus,
}

/// An event record stored in the on-chain event index.
///
/// Lightweight summary that mirrors a Soroban event log entry, enabling
/// efficient querying by event type, time range, and actor address.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRecord {
    pub event_id: u64,
    pub event_type: Symbol,
    pub timestamp: u64,
    pub actor: Address,
    pub contract_id: Address,
    pub data_key: Symbol,
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
///     amend_offer, counter_offer, close_negotiation, set_negotiation_window,
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
///   - state-changing: record_outcome, resolve_dispute, set_recorder.
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

// ─── Invoice State Machine ────────────────────────────────────────────────────

// ─── Multisig Admin Governance (ADR-0010) ─────────────────────────────────────
//
// Every `assert_admin`-style check across the five contracts used to compare
// the caller against one stored `Address`. This section replaces that with an
// M-of-N threshold over a configurable set of signer addresses, following the
// same "same-block, no timelock" philosophy as the pause mechanism
// (ADR-0001): a call that already carries `threshold` valid signer
// authorizations executes immediately, in the same transaction, with no
// on-chain proposal queue to track.
//
// `AdminConfig { signers: [admin], threshold: 1 }` — one signer, threshold
// one — is single-admin bootstrap mode, and is what every constructor sets up
// by default. Under it, a threshold-gated call takes a one-element
// `Vec<Address>` and behaves exactly like the legacy single-admin check: the
// sole signer's authorization is both necessary and sufficient. Moving to a
// true M-of-N is a post-deploy admin action (`set_signers`, threshold-gated
// like everything else here), never a redeploy.

/// M-of-N admin governance config: `threshold` distinct, authorized addresses
/// out of `signers` are required to authorize any admin-gated call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminConfig {
    pub signers: Vec<Address>,
    pub threshold: u32,
}

/// One-time setup: writes single-admin bootstrap config (`[admin]`,
/// threshold 1). Called from each contract's constructor in place of the old
/// `storage().instance().set(&symbol_short!("admin"), &admin)`. Panics if a
/// config already exists — constructors run at most once (see ADR-0005).
pub fn init_admin_config(env: &Env, admin: &Address) {
    if env.storage().instance().has(&symbol_short!("admcfg")) {
        panic!("Already initialized");
    }
    let mut signers = Vec::new(env);
    signers.push_back(admin.clone());
    save_admin_config(
        env,
        &AdminConfig {
            signers,
            threshold: 1,
        },
    );
}

/// Load the current admin config. Panics if the contract was never
/// initialized (pre-ADR-0010 instances that have not run `migrate_admin` —
/// see the migration note in `docs/adr/0010-multisig-admin-governance.md`).
pub fn load_admin_config(env: &Env) -> AdminConfig {
    env.storage()
        .instance()
        .get(&symbol_short!("admcfg"))
        .unwrap_or_else(|| panic!("Not initialized"))
}

/// Persist an admin config. `pub` so `set_signers` (and the one-time
/// `migrate_admin` path) can write it from each contract; every other
/// mutation goes through `assert_threshold` first.
pub fn save_admin_config(env: &Env, cfg: &AdminConfig) {
    env.storage().instance().set(&symbol_short!("admcfg"), cfg);
}

/// True if `who` is a member of `cfg.signers`.
pub fn is_signer(cfg: &AdminConfig, who: &Address) -> bool {
    cfg.signers.iter().any(|s| s == *who)
}

/// Validates a candidate `(signers, threshold)` pair before it is written by
/// `set_signers`: non-empty, threshold in `[1, signers.len()]`, no duplicate
/// address, and bounded by `MAX_ADMIN_SIGNERS`. A threshold above the signer
/// count would make the config permanently unsatisfiable; a duplicate would
/// let one key count twice toward it.
pub fn validate_signers(env: &Env, signers: &Vec<Address>, threshold: u32) {
    if signers.is_empty() || signers.len() > MAX_ADMIN_SIGNERS {
        env.panic_with_error(ContractError::InvalidInput);
    }
    if threshold == 0 || threshold > signers.len() {
        env.panic_with_error(ContractError::InvalidInput);
    }
    for (i, a) in signers.iter().enumerate() {
        for (j, b) in signers.iter().enumerate() {
            if i != j && a == b {
                env.panic_with_error(ContractError::InvalidInput);
            }
        }
    }
}

/// The core threshold check. Requires every address in `provided` to be a
/// distinct member of `cfg.signers` and to authorize this invocation
/// (`require_auth`), and requires at least `cfg.threshold` of them. Panics
/// with `Unauthorized` otherwise.
///
/// Soroban lets a single transaction carry a separate signed authorization
/// entry per address, so `provided` being fully authorized means every one
/// of those signers actually co-signed *this* call, in this same
/// transaction — there is no separate on-chain approval step to replay or to
/// front-run.
pub fn assert_threshold(env: &Env, cfg: &AdminConfig, provided: &Vec<Address>) {
    let mut counted: Vec<Address> = Vec::new(env);
    for who in provided.iter() {
        if !is_signer(cfg, &who) {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if counted.iter().any(|c| c == who) {
            // Duplicate entry — would otherwise let one signature count
            // twice toward the threshold.
            env.panic_with_error(ContractError::Unauthorized);
        }
        who.require_auth();
        counted.push_back(who);
    }
    if counted.len() < cfg.threshold {
        env.panic_with_error(ContractError::Unauthorized);
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
    fn claim_payout(env: Env, offer_id: Symbol, lender: Address, amount: i128) -> (i128, i128);
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

// ─── assert_transition unit tests ────────────────────────────────────────────

#[cfg(test)]
mod transition_tests {
    extern crate std;

    use super::{validate_transition, InvoiceStatus};
    use soroban_sdk::Env;

    // ── Legal transitions ────────────────────────────────────────────────────
    //
    // Each row: (from, to) — must succeed without panic.
    //
    // Adding a new status to the state machine later means adding rows here;
    // all illegal combinations are covered by the matrix below.

    macro_rules! legal {
        ($name:ident, $from:expr, $to:expr) => {
            #[test]
            fn $name() {
                let env = Env::default();
                validate_transition(&env, $from, $to); // must not panic
            }
        };
    }

    // Pending exits
    legal!(
        pending_to_financed,
        InvoiceStatus::Pending,
        InvoiceStatus::Financed
    );
    legal!(
        pending_to_cancelled,
        InvoiceStatus::Pending,
        InvoiceStatus::Cancelled
    );

    // Financed exits
    legal!(
        financed_to_financed,
        InvoiceStatus::Financed,
        InvoiceStatus::Financed
    ); // partial repayment
    legal!(
        financed_to_repaid,
        InvoiceStatus::Financed,
        InvoiceStatus::Repaid
    );
    legal!(
        financed_to_overdue,
        InvoiceStatus::Financed,
        InvoiceStatus::Overdue
    );
    legal!(
        financed_to_disputed,
        InvoiceStatus::Financed,
        InvoiceStatus::Disputed
    );

    // Overdue exits
    legal!(
        overdue_to_defaulted,
        InvoiceStatus::Overdue,
        InvoiceStatus::Defaulted
    );

    // Disputed exits (resolve_dispute)
    legal!(
        disputed_to_pending,
        InvoiceStatus::Disputed,
        InvoiceStatus::Pending
    );
    legal!(
        disputed_to_financed,
        InvoiceStatus::Disputed,
        InvoiceStatus::Financed
    );
    legal!(
        disputed_to_repaid,
        InvoiceStatus::Disputed,
        InvoiceStatus::Repaid
    );
    legal!(
        disputed_to_cancelled,
        InvoiceStatus::Disputed,
        InvoiceStatus::Cancelled
    );
    legal!(
        disputed_to_defaulted,
        InvoiceStatus::Disputed,
        InvoiceStatus::Defaulted
    );

    // ── Illegal transitions ───────────────────────────────────────────────────
    //
    // Every (from, to) pair NOT in the legal set must panic with InvalidTransition
    // (ContractError discriminant 3 → "Error(Contract, #3)").

    macro_rules! illegal {
        ($name:ident, $from:expr, $to:expr) => {
            #[test]
            #[should_panic(expected = "Error(Contract, #3)")]
            fn $name() {
                let env = Env::default();
                validate_transition(&env, $from, $to);
            }
        };
    }

    // Pending — illegal targets
    illegal!(
        pending_to_pending,
        InvoiceStatus::Pending,
        InvoiceStatus::Pending
    );
    illegal!(
        pending_to_repaid,
        InvoiceStatus::Pending,
        InvoiceStatus::Repaid
    ); // the motivating example
    illegal!(
        pending_to_overdue,
        InvoiceStatus::Pending,
        InvoiceStatus::Overdue
    );
    illegal!(
        pending_to_disputed,
        InvoiceStatus::Pending,
        InvoiceStatus::Disputed
    );
    illegal!(
        pending_to_defaulted,
        InvoiceStatus::Pending,
        InvoiceStatus::Defaulted
    );

    // Financed — illegal targets
    illegal!(
        financed_to_pending,
        InvoiceStatus::Financed,
        InvoiceStatus::Pending
    );
    illegal!(
        financed_to_cancelled,
        InvoiceStatus::Financed,
        InvoiceStatus::Cancelled
    );
    illegal!(
        financed_to_defaulted,
        InvoiceStatus::Financed,
        InvoiceStatus::Defaulted
    );

    // Overdue — illegal targets
    illegal!(
        overdue_to_pending,
        InvoiceStatus::Overdue,
        InvoiceStatus::Pending
    );
    illegal!(
        overdue_to_financed,
        InvoiceStatus::Overdue,
        InvoiceStatus::Financed
    );
    illegal!(
        overdue_to_repaid,
        InvoiceStatus::Overdue,
        InvoiceStatus::Repaid
    );
    illegal!(
        overdue_to_overdue,
        InvoiceStatus::Overdue,
        InvoiceStatus::Overdue
    );
    illegal!(
        overdue_to_cancelled,
        InvoiceStatus::Overdue,
        InvoiceStatus::Cancelled
    );
    illegal!(
        overdue_to_disputed,
        InvoiceStatus::Overdue,
        InvoiceStatus::Disputed
    );

    // Disputed — illegal targets (anything not in the five allowed)
    illegal!(
        disputed_to_overdue,
        InvoiceStatus::Disputed,
        InvoiceStatus::Overdue
    );
    illegal!(
        disputed_to_disputed,
        InvoiceStatus::Disputed,
        InvoiceStatus::Disputed
    );

    // Terminal states — nothing may leave them
    illegal!(
        repaid_to_pending,
        InvoiceStatus::Repaid,
        InvoiceStatus::Pending
    );
    illegal!(
        repaid_to_financed,
        InvoiceStatus::Repaid,
        InvoiceStatus::Financed
    );
    illegal!(
        repaid_to_repaid,
        InvoiceStatus::Repaid,
        InvoiceStatus::Repaid
    );
    illegal!(
        repaid_to_overdue,
        InvoiceStatus::Repaid,
        InvoiceStatus::Overdue
    );
    illegal!(
        repaid_to_cancelled,
        InvoiceStatus::Repaid,
        InvoiceStatus::Cancelled
    );
    illegal!(
        repaid_to_disputed,
        InvoiceStatus::Repaid,
        InvoiceStatus::Disputed
    );
    illegal!(
        repaid_to_defaulted,
        InvoiceStatus::Repaid,
        InvoiceStatus::Defaulted
    );

    illegal!(
        cancelled_to_pending,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Pending
    );
    illegal!(
        cancelled_to_financed,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Financed
    );
    illegal!(
        cancelled_to_repaid,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Repaid
    );
    illegal!(
        cancelled_to_overdue,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Overdue
    );
    illegal!(
        cancelled_to_cancelled,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Cancelled
    );
    illegal!(
        cancelled_to_disputed,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Disputed
    );
    illegal!(
        cancelled_to_defaulted,
        InvoiceStatus::Cancelled,
        InvoiceStatus::Defaulted
    );

    illegal!(
        defaulted_to_pending,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Pending
    );
    illegal!(
        defaulted_to_financed,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Financed
    );
    illegal!(
        defaulted_to_repaid,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Repaid
    );
    illegal!(
        defaulted_to_overdue,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Overdue
    );
    illegal!(
        defaulted_to_cancelled,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Cancelled
    );
    illegal!(
        defaulted_to_disputed,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Disputed
    );
    illegal!(
        defaulted_to_defaulted,
        InvoiceStatus::Defaulted,
        InvoiceStatus::Defaulted
    );
} // end mod transition_tests

// ─── State Machine State Validation and Enforcement ──────────────────────────

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
    validate_transition(env, from_status, to_status);

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
pub(crate) fn validate_transition(env: &Env, from_status: InvoiceStatus, to_status: InvoiceStatus) {
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
        env.panic_with_error(ContractError::InvalidTransition);
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

#[cfg(test)]
mod semantic_version_tests {
    extern crate std;

    use super::{parse_semantic_version, SemanticVersion};
    use soroban_sdk::{Env, String};

    #[test]
    fn compares_numeric_components_not_lexicographic_text() {
        let env = Env::default();
        let one_nine = parse_semantic_version(&env, &String::from_str(&env, "1.9.0"));
        let one_ten = parse_semantic_version(&env, &String::from_str(&env, "1.10.0"));
        assert_eq!(
            one_ten,
            SemanticVersion {
                major: 1,
                minor: 10,
                patch: 0
            }
        );
        assert!(one_nine < one_ten);
    }

    #[test]
    fn rejects_malformed_versions() {
        let env = Env::default();
        for value in ["1.0", "1.0.0.0", "01.0.0", "1.a.0", "1..0"] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                parse_semantic_version(&env, &String::from_str(&env, value));
            }));
            assert!(result.is_err(), "{value} must be rejected");
        }
    }
}
