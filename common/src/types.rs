//! Domain data types, errors, state records, and cross-contract interfaces for InvoFi.
//!
//! Soroban SDK macros (`#[contracttype]`, `#[contracterror]`, `#[contractclient]`) expand
//! into synthetic public statics (`__SPEC_XDR_*`) and associated functions (`spec_xdr()`)
//! without rustdoc comments. In order to allow crate-level `#![warn(missing_docs)]` without
//! triggering compiler warnings on Soroban SDK macro-generated internals, missing_docs is
//! allowed within this module. All domain items and fields are thoroughly documented below.

#![allow(missing_docs)]

use soroban_sdk::{
    contractclient, contracterror, contracttype, Address, BytesN, Env, String, Symbol, Vec,
};

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

// ─── Versioning Types ─────────────────────────────────────────────────────────

/// Numeric components of a strict `MAJOR.MINOR.PATCH` version.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct SemanticVersion {
    /// Major version number, incremented for incompatible breaking changes.
    pub major: u32,
    /// Minor version number, incremented for backwards-compatible features.
    pub minor: u32,
    /// Patch version number, incremented for backwards-compatible bug fixes.
    pub patch: u32,
}

/// Represents an executable upgrade proposal awaiting post-upgrade finalization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    /// Semantic version string of the pending upgrade.
    pub version: String,
    /// 32-byte hash of the newly uploaded WASM contract executable.
    pub wasm_hash: BytesN<32>,
}

// ─── Types ───────────────────────────────────────────────────────────────────

/// Risk tier for yield-rate lookups. A = low risk, C = high risk.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RiskTier {
    /// Tier A: Highest credit quality, lowest financing rates.
    A = 0,
    /// Tier B: Moderate credit quality, standard financing rates.
    B = 1,
    /// Tier C: Higher risk, highest financing rates.
    C = 2,
}

/// An invoice registered on-chain.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invoice {
    /// Unique alphanumeric identifier for this invoice.
    pub id: Symbol,
    /// Stellar address of the supplier or merchant issuing the invoice.
    pub originator: Address,
    /// Nominal invoice amount in stroops (1 XLM = 10_000_000 stroops).
    pub amount: i128,
    /// Symbol identifying the registered settlement token.
    pub currency: Symbol,
    /// Unix timestamp in seconds when repayment is due.
    pub due_date: u64,
    /// Current lifecycle status of the invoice.
    pub status: InvoiceStatus,
}

/// Metadata for an ERC-721-like non-fungible invoice token (issue #178).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvoiceTokenMetadata {
    /// Unique 32-byte identifier of the non-fungible position token.
    pub token_id: BytesN<32>,
    /// Identifier of the underlying registered invoice.
    pub invoice_id: Symbol,
    /// Original invoice issuer address.
    pub originator: Address,
    /// Current owner or holder of the tokenized invoice position.
    pub owner: Address,
    /// Nominal invoice face value in stroops.
    pub amount: i128,
    /// Settlement currency symbol.
    pub currency: Symbol,
    /// Unix timestamp when invoice repayment is due.
    pub due_date: u64,
    /// Current lifecycle status of the underlying invoice.
    pub status: InvoiceStatus,
}

/// Lifecycle status of an invoice.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum InvoiceStatus {
    /// Invoice registered on-chain and open for financing offers.
    Pending = 0,
    /// Financing offer accepted and principal funds disbursed to originator.
    Financed = 1,
    /// Principal and all accrued interest fully repaid.
    Repaid = 2,
    /// Repayment due date elapsed without complete repayment.
    Overdue = 3,
    /// Invoice cancelled by originator prior to financing.
    Cancelled = 4,
    /// Formal dispute raised, halting automated transitions.
    Disputed = 5,
    /// Unpaid past grace period, lender reclaimed and declared default.
    Defaulted = 6,
}

/// A record of a single invoice state transition.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionRecord {
    /// Lifecycle status before the transition occurred.
    pub from_status: InvoiceStatus,
    /// Lifecycle status after the transition completed.
    pub to_status: InvoiceStatus,
    /// Address or contract that initiated and authorized the transition.
    pub actor: Address,
    /// Ledger Unix timestamp at which the transition was committed.
    pub timestamp: u64,
}

/// The field being amended on an invoice.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AmendmentField {
    /// Amending the invoice face value amount.
    Amount = 0,
    /// Amending the invoice payment due date.
    DueDate = 1,
}

/// Status of an amendment request.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AmendmentStatus {
    /// Amendment requested and awaiting counterparty approval.
    Pending = 0,
    /// Amendment approved and committed to invoice state.
    Approved = 1,
    /// Amendment formally rejected.
    Rejected = 2,
}

/// An on-chain audit record for an invoice amendment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmendmentRecord {
    /// Field being modified by the amendment.
    pub field: AmendmentField,
    /// Previous invoice face value prior to amendment.
    pub old_amount: i128,
    /// Proposed or approved new invoice face value.
    pub new_amount: i128,
    /// Previous due date timestamp prior to amendment.
    pub old_due_date: u64,
    /// Proposed or approved new due date timestamp.
    pub new_due_date: u64,
    /// Symbol reason code provided for the amendment.
    pub reason: Symbol,
    /// Ledger Unix timestamp when the amendment was recorded.
    pub timestamp: u64,
    /// Resolution status of the amendment.
    pub status: AmendmentStatus,
}

/// A financing offer submitted by a lender against an invoice.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinancingOffer {
    /// Unique identifier for this financing offer.
    pub id: Symbol,
    /// Identifier of the target invoice being financed.
    pub invoice_id: Symbol,
    /// Stellar address of the lender providing liquidity.
    pub lender: Address,
    /// Principal financing amount offered.
    pub amount: i128,
    /// Symbol of the currency used for disbursement and repayment.
    pub currency: Symbol,
    /// Interest rate in basis points (e.g. 500 = 5.00%).
    pub interest_rate: u32,
    /// Financing duration in seconds.
    pub duration: u64,
    /// Optional Unix timestamp after which the offer can no longer be accepted (0 = no expiry).
    pub expires_at: u64,
    /// Current lifecycle status of the offer.
    pub status: OfferStatus,
    /// Unix timestamp when the offer was accepted; 0 if not yet accepted.
    pub funded_at: u64,
    /// Running total of repayments made against the financing obligation.
    pub amount_repaid: i128,
}

/// Lifecycle status of a financing offer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OfferStatus {
    /// Offer created by lender and awaiting originator decision.
    Pending = 0,
    /// Offer accepted by originator, pending fund disbursement.
    Accepted = 1,
    /// Offer explicitly rejected by originator.
    Rejected = 2,
    /// Principal disbursed to originator, financing is active.
    Financed = 3,
    /// Principal and interest fully satisfied.
    Repaid = 4,
    /// Underlying invoice defaulted and lender reclaimed.
    Defaulted = 5,
}

/// Aggregate protocol statistics.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolStats {
    /// Total count of invoices registered across the protocol.
    pub total_invoices: u32,
    /// Total count of financing offers created.
    pub total_offers: u32,
    /// Total cumulative principal amount financed in stroops.
    pub total_financed: i128,
    /// Total cumulative amount repaid by originators in stroops.
    pub total_repaid: i128,
    /// Total cumulative protocol fee revenue earned in stroops.
    pub total_fee_revenue: i128,
}

/// Per-lender activity statistics.
#[contracttype]
#[derive(Clone, Debug, Default)]
pub struct LenderStats {
    /// Total cumulative liquidity offered across all proposals.
    pub total_offered: i128,
    /// Total cumulative liquidity accepted and funded.
    pub total_accepted: i128,
    /// Count of offers currently pending originator action.
    pub offers_pending: u32,
    /// Count of financed offers successfully repaid in full.
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
    /// Daily installments (every 86,400 seconds).
    Daily = 0,
    /// Weekly installments (every 604,800 seconds).
    Weekly = 1,
    /// Monthly installments (every 2,592,000 seconds / 30 days).
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
    /// Identifier of the financing offer this schedule belongs to.
    pub offer_id: Symbol,
    /// Number of equal installments.
    pub count: u32,
    /// Interval between installments.
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
    /// The two sides converged on identical terms and the offer executed. Terminal.
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
    /// Attestation evaluation status.
    pub status: VerificationStatus,
}

/// An event record stored in the on-chain event index.
///
/// Lightweight summary that mirrors a Soroban event log entry, enabling
/// efficient querying by event type, time range, and actor address.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRecord {
    /// Monotonically increasing sequential event identifier.
    pub event_id: u64,
    /// Symbolic category name of the event.
    pub event_type: Symbol,
    /// Ledger timestamp when the event occurred.
    pub timestamp: u64,
    /// Initiating actor address.
    pub actor: Address,
    /// Originating contract address.
    pub contract_id: Address,
    /// Primary storage key or entity identifier referenced.
    pub data_key: Symbol,
}

/// M-of-N admin governance config: `threshold` distinct, authorized addresses
/// out of `signers` are required to authorize any admin-gated call.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminConfig {
    /// Authorized administrator signer addresses.
    pub signers: Vec<Address>,
    /// Minimum threshold of valid signatures required.
    pub threshold: u32,
}

// ─── Cross-Contract Interface Traits ─────────────────────────────────────────

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

    /// Read the protocol-fee recipient address.
    fn get_fee_recipient(env: Env) -> Address;
}

/// Client trait for the Financing contract, used by Repayment for
/// cross-contract calls. The `#[contractclient]` macro generates a
/// type-safe client from this trait.
#[contractclient(name = "FinancingClient")]
pub trait FinancingInterface {
    /// Read a financing offer by ID.
    fn get_offer(env: Env, id: Symbol) -> FinancingOffer;

    /// Read all offers associated with an invoice.
    fn get_offers_by_invoice(env: Env, invoice_id: Symbol) -> Vec<FinancingOffer>;

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
