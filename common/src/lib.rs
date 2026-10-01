//! Shared types, constants, and currency registry for InvoFi Soroban contracts.
//!
//! `registry` and `financing` both depend on this crate. The currency → SEP-41
//! token registry lives here so adding a third currency means one
//! `register_currency` call — never a new branch in every money-touching
//! function.

#![no_std]
#![warn(missing_docs)]

use soroban_sdk::{symbol_short, Address, BytesN, Env, Map, String, Symbol, Vec};

/// Domain types, errors, state records, and cross-contract interfaces.
pub mod types;
pub use types::*;

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

// ─── Contract Versioning ──────────────────────────────────────────────────────

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

// ─── Multisig Admin Governance (ADR-0010) ─────────────────────────────────────

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
pub fn assert_threshold(env: &Env, cfg: &AdminConfig, provided: &Vec<Address>) {
    let mut counted: Vec<Address> = Vec::new(env);
    for who in provided.iter() {
        if !is_signer(cfg, &who) {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if counted.iter().any(|c| c == who) {
            env.panic_with_error(ContractError::Unauthorized);
        }
        who.require_auth();
        counted.push_back(who);
    }
    if counted.len() < cfg.threshold {
        env.panic_with_error(ContractError::Unauthorized);
    }
}

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
    validate_transition(env, from_status, to_status);

    env.events().publish(
        (symbol_short!("inv_trx"), invoice_id.clone()),
        (from_status, to_status, actor.clone()),
    );

    record_transition(env, invoice_id, from_status, to_status, actor);
}

/// Validates that a transition from `from_status` to `to_status` is allowed.
pub(crate) fn validate_transition(env: &Env, from_status: InvoiceStatus, to_status: InvoiceStatus) {
    let valid = matches!(
        (from_status, to_status),
        (InvoiceStatus::Pending, InvoiceStatus::Cancelled)
            | (InvoiceStatus::Pending, InvoiceStatus::Financed)
            | (InvoiceStatus::Financed, InvoiceStatus::Repaid)
            | (InvoiceStatus::Financed, InvoiceStatus::Financed)
            | (InvoiceStatus::Financed, InvoiceStatus::Overdue)
            | (InvoiceStatus::Financed, InvoiceStatus::Disputed)
            | (InvoiceStatus::Overdue, InvoiceStatus::Defaulted)
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod transition_tests {
    extern crate std;

    use super::{validate_transition, InvoiceStatus};
    use soroban_sdk::Env;

    macro_rules! legal {
        ($name:ident, $from:expr, $to:expr) => {
            #[test]
            fn $name() {
                let env = Env::default();
                validate_transition(&env, $from, $to);
            }
        };
    }

    macro_rules! illegal {
        ($name:ident, $from:expr, $to:expr) => {
            #[test]
            fn $name() {
                let env = Env::default();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    validate_transition(&env, $from, $to);
                }));
                assert!(result.is_err(), "expected transition to panic");
            }
        };
    }

    legal!(pending_to_cancelled, InvoiceStatus::Pending, InvoiceStatus::Cancelled);
    legal!(pending_to_financed, InvoiceStatus::Pending, InvoiceStatus::Financed);
    legal!(financed_to_repaid, InvoiceStatus::Financed, InvoiceStatus::Repaid);
    legal!(financed_to_financed, InvoiceStatus::Financed, InvoiceStatus::Financed);
    legal!(financed_to_overdue, InvoiceStatus::Financed, InvoiceStatus::Overdue);
    legal!(financed_to_disputed, InvoiceStatus::Financed, InvoiceStatus::Disputed);
    legal!(overdue_to_defaulted, InvoiceStatus::Overdue, InvoiceStatus::Defaulted);
    legal!(disputed_to_pending, InvoiceStatus::Disputed, InvoiceStatus::Pending);
    legal!(disputed_to_financed, InvoiceStatus::Disputed, InvoiceStatus::Financed);
    legal!(disputed_to_repaid, InvoiceStatus::Disputed, InvoiceStatus::Repaid);
    legal!(disputed_to_cancelled, InvoiceStatus::Disputed, InvoiceStatus::Cancelled);
    legal!(disputed_to_defaulted, InvoiceStatus::Disputed, InvoiceStatus::Defaulted);

    illegal!(pending_to_pending, InvoiceStatus::Pending, InvoiceStatus::Pending);
    illegal!(pending_to_repaid, InvoiceStatus::Pending, InvoiceStatus::Repaid);
    illegal!(pending_to_overdue, InvoiceStatus::Pending, InvoiceStatus::Overdue);
    illegal!(pending_to_disputed, InvoiceStatus::Pending, InvoiceStatus::Disputed);
    illegal!(pending_to_defaulted, InvoiceStatus::Pending, InvoiceStatus::Defaulted);

    illegal!(financed_to_pending, InvoiceStatus::Financed, InvoiceStatus::Pending);
    illegal!(financed_to_cancelled, InvoiceStatus::Financed, InvoiceStatus::Cancelled);
    illegal!(financed_to_defaulted, InvoiceStatus::Financed, InvoiceStatus::Defaulted);

    illegal!(repaid_to_pending, InvoiceStatus::Repaid, InvoiceStatus::Pending);
    illegal!(repaid_to_financed, InvoiceStatus::Repaid, InvoiceStatus::Financed);
    illegal!(repaid_to_repaid, InvoiceStatus::Repaid, InvoiceStatus::Repaid);
    illegal!(repaid_to_overdue, InvoiceStatus::Repaid, InvoiceStatus::Overdue);
    illegal!(repaid_to_cancelled, InvoiceStatus::Repaid, InvoiceStatus::Cancelled);
    illegal!(repaid_to_disputed, InvoiceStatus::Repaid, InvoiceStatus::Disputed);
    illegal!(repaid_to_defaulted, InvoiceStatus::Repaid, InvoiceStatus::Defaulted);

    illegal!(overdue_to_pending, InvoiceStatus::Overdue, InvoiceStatus::Pending);
    illegal!(overdue_to_financed, InvoiceStatus::Overdue, InvoiceStatus::Financed);
    illegal!(overdue_to_repaid, InvoiceStatus::Overdue, InvoiceStatus::Repaid);
    illegal!(overdue_to_overdue, InvoiceStatus::Overdue, InvoiceStatus::Overdue);
    illegal!(overdue_to_cancelled, InvoiceStatus::Overdue, InvoiceStatus::Cancelled);
    illegal!(overdue_to_disputed, InvoiceStatus::Overdue, InvoiceStatus::Disputed);

    illegal!(cancelled_to_pending, InvoiceStatus::Cancelled, InvoiceStatus::Pending);
    illegal!(cancelled_to_financed, InvoiceStatus::Cancelled, InvoiceStatus::Financed);
    illegal!(cancelled_to_repaid, InvoiceStatus::Cancelled, InvoiceStatus::Repaid);
    illegal!(cancelled_to_overdue, InvoiceStatus::Cancelled, InvoiceStatus::Overdue);
    illegal!(cancelled_to_cancelled, InvoiceStatus::Cancelled, InvoiceStatus::Cancelled);
    illegal!(cancelled_to_disputed, InvoiceStatus::Cancelled, InvoiceStatus::Disputed);
    illegal!(cancelled_to_defaulted, InvoiceStatus::Cancelled, InvoiceStatus::Defaulted);

    illegal!(disputed_to_disputed, InvoiceStatus::Disputed, InvoiceStatus::Disputed);
    illegal!(disputed_to_overdue, InvoiceStatus::Disputed, InvoiceStatus::Overdue);

    illegal!(defaulted_to_pending, InvoiceStatus::Defaulted, InvoiceStatus::Pending);
    illegal!(defaulted_to_financed, InvoiceStatus::Defaulted, InvoiceStatus::Financed);
    illegal!(defaulted_to_repaid, InvoiceStatus::Defaulted, InvoiceStatus::Repaid);
    illegal!(defaulted_to_overdue, InvoiceStatus::Defaulted, InvoiceStatus::Overdue);
    illegal!(defaulted_to_cancelled, InvoiceStatus::Defaulted, InvoiceStatus::Cancelled);
    illegal!(defaulted_to_disputed, InvoiceStatus::Defaulted, InvoiceStatus::Disputed);
    illegal!(defaulted_to_defaulted, InvoiceStatus::Defaulted, InvoiceStatus::Defaulted);
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
