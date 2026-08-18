#![no_std]

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Symbol};

use invofi_common::{
    assert_not_paused, assert_schema_version, write_schema_version, resolve_token, ContractError,
    FinancingClient, FinancingOffer, InsuranceClient, Invoice, InvoiceStatus, OfferStatus,
    RegistryClient, ReputationClient, GRACE_PERIOD_SECS, MAX_OFFER_DURATION_SECS,
    MIN_OFFER_DURATION_SECS,
};

/// Current storage schema version for the repayment contract.
///
/// Increment this constant when a new release changes the layout of any
/// persistent or instance storage key. See docs/adr/0009-storage-schema-versioning.md.
pub const SCHEMA_VERSION: u32 = 1;

// ─── Overdue penalty (ADR-0007) ──────────────────────────────────────────────

/// Seconds in a day. Penalty accrues in whole elapsed days.
const SECS_PER_DAY: u64 = 86_400;

/// Upper bound on the configurable per-day penalty rate: 500 bps = 5%/day.
/// Guards against a mis-keyed admin call setting an absurd rate.
pub const MAX_PENALTY_BPS: u32 = 500;

/// Accrued overdue penalty on an obligation, in the offer's currency.
///
/// Per ADR-0007:
/// - accrual is anchored on `due_date`, not on the Overdue status transition
///   (which is permissionless and therefore gameable);
/// - the base is **frozen** at `total_due` (principal + yield) and does not
///   shrink as repayments land, so a late partial payment cannot retroactively
///   erase penalty that has already accrued;
/// - elapsed time truncates to whole days, so the partial day in progress is
///   not charged — rounding runs in the borrower's favour;
/// - the result is capped at `total_due * cap_bps / 10_000`.
///
/// Returns 0 when the feature is disabled (`penalty_bps == 0`), which is the
/// default for a freshly deployed contract.
fn accrued_penalty(
    env: &Env,
    total_due: i128,
    due_date: u64,
    penalty_bps: u32,
    cap_bps: u32,
) -> i128 {
    if penalty_bps == 0 || cap_bps == 0 || total_due <= 0 {
        return 0;
    }
    let now = env.ledger().timestamp();
    if now <= due_date {
        return 0;
    }
    let elapsed_days = ((now - due_date) / SECS_PER_DAY) as i128;
    if elapsed_days == 0 {
        return 0;
    }

    // Multiply in i128 with saturation, divide by 10_000 last so the two
    // multiplications do not compound truncation error.
    let raw = total_due
        .saturating_mul(penalty_bps as i128)
        .saturating_mul(elapsed_days)
        / 10_000;
    let cap = total_due.saturating_mul(cap_bps as i128) / 10_000;
    raw.min(cap)
}

/// Accrued penalty for an already-loaded offer. Reads the invoice (for
/// `due_date`) and the penalty config. Shared by `calculate_total_due` and
/// `calculate_penalty` so neither has to re-fetch the offer.
fn penalty_for_offer(env: &Env, offer: &FinancingOffer) -> i128 {
    let registry_addr: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("registry"))
        .unwrap_or_else(|| panic!("Not initialized"));
    let registry_client = RegistryClient::new(env, &registry_addr);
    let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);

    let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
    let total_due = offer.amount + yield_amount;
    let (penalty_bps, cap_bps) = load_penalty_config(env);
    accrued_penalty(env, total_due, invoice.due_date, penalty_bps, cap_bps)
}

/// Read the configured penalty parameters, defaulting to disabled.
fn load_penalty_config(env: &Env) -> (u32, u32) {
    let penalty_bps: u32 = env
        .storage()
        .instance()
        .get(&symbol_short!("penbps"))
        .unwrap_or(0);
    let cap_bps: u32 = env
        .storage()
        .instance()
        .get(&symbol_short!("pencap"))
        .unwrap_or(0);
    (penalty_bps, cap_bps)
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct RepaymentContract;

#[contractimpl]
impl RepaymentContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// One-time setup. Stores the registry, financing, and token addresses for
    /// cross-contract calls. Admin is the same as the registry/financing admin.
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run (issue #75).
    pub fn __constructor(
        env: Env,
        admin: Address,
        registry: Address,
        financing: Address,
        token: Address,
    ) {
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
            .set(&symbol_short!("financing"), &financing);
        env.storage()
            .instance()
            .set(&symbol_short!("token"), &token);
        write_schema_version(&env, SCHEMA_VERSION);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Register the insurance contract address. Admin only. When configured,
    /// reclaim (default) triggers a pool payout to the lender from the
    /// insurance pool (Task 10).
    pub fn set_insurance(env: Env, admin: Address, insurance: Address) {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);
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
            .set(&symbol_short!("insadd"), &insurance);
    }

    pub fn get_insurance(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("insadd"))
    }

    /// Register the reputation contract address. Admin only. When configured,
    /// full repayments and defaults update the originator's reputation score
    /// (Task 11).
    pub fn set_reputation(env: Env, admin: Address, reputation: Address) {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);
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
            .set(&symbol_short!("repadd"), &reputation);
    }

    pub fn get_reputation(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("repadd"))
    }

    /// Configure overdue penalty accrual (ADR-0007). Admin only.
    ///
    /// `penalty_bps` is the **per-day** rate applied to the frozen base
    /// (principal + yield); `cap_bps` bounds total accrued penalty as a
    /// fraction of that same base. Both default to 0, which disables accrual
    /// entirely — a freshly deployed contract behaves exactly as before until
    /// an admin calls this.
    pub fn set_penalty(env: Env, admin: Address, penalty_bps: u32, cap_bps: u32) {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if penalty_bps > MAX_PENALTY_BPS {
            env.panic_with_error(ContractError::InvalidInput);
        }
        if cap_bps > 10_000 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("penbps"), &penalty_bps);
        env.storage()
            .instance()
            .set(&symbol_short!("pencap"), &cap_bps);
    }

    /// The configured per-day penalty rate in basis points (0 = disabled).
    pub fn get_penalty_bps(env: Env) -> u32 {
        load_penalty_config(&env).0
    }

    /// The configured penalty ceiling in basis points of the frozen base.
    pub fn get_penalty_cap_bps(env: Env) -> u32 {
        load_penalty_config(&env).1
    }

    /// Transfers admin rights. Only current admin.
    pub fn transfer_admin(env: Env, admin: Address, new_admin: Address) {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);
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

    // ── Repayment ────────────────────────────────────────────────────────────

    /// Mark part or all of an invoice as repaid. Only the originator.
    /// Cross-contract: reads + updates invoice status in registry, reads
    /// + updates offer state in financing.
    pub fn repay_invoice(
        env: Env,
        invoice_id: Symbol,
        offer_id: Symbol,
        repayer: Address,
        amount: i128,
    ) -> Invoice {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);
        repayer.require_auth();

        // Cross-contract: read invoice from registry
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call. Repayment has no local state to mutate, so CEI is trivially satisfied.
        let invoice: Invoice = registry_client.get_invoice(&invoice_id);

        if invoice.originator != repayer {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if invoice.status != InvoiceStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: read offer from financing
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        // CEI: Read-only cross-contract call.
        let mut offer: FinancingOffer = financing_client.get_offer(&offer_id);

        if offer.invoice_id != invoice_id {
            env.panic_with_error(ContractError::InvalidInput);
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        assert!(amount > 0, "repayment amount must be greater than zero");

        let token_id = resolve_token(&env, &offer.currency);
        let token_client = token::TokenClient::new(&env, &token_id);
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;

        // Overdue penalty (ADR-0007). Accrues from the invoice due date, on a
        // base frozen at principal + yield. Zero unless an admin has enabled
        // it, and zero while the invoice is not yet past due.
        let (penalty_bps, cap_bps) = load_penalty_config(&env);
        let penalty = accrued_penalty(&env, total_due, invoice.due_date, penalty_bps, cap_bps);
        let total_owed = total_due + penalty;

        let remaining_balance = total_owed - offer.amount_repaid;
        if amount > remaining_balance {
            env.panic_with_error(ContractError::InsufficientBalance);
        }

        // Protocol fee deduction
        let fee_bps: u32 = financing_client.get_fee_bps();
        let fee_amount = amount * (fee_bps as i128) / 10_000;
        let lender_amount = amount - fee_amount;
        // CEI: External interaction. Safe because this contract has no local state to protect.
        token_client.transfer(&repayer, &offer.lender, &lender_amount);
        if fee_amount > 0 {
            let admin: Address = env
                .storage()
                .instance()
                .get(&symbol_short!("admin"))
                .unwrap_or_else(|| panic!("Not initialized"));
            // CEI: External interaction. Safe because this contract has no local state to protect.
            token_client.transfer(&repayer, &admin, &fee_amount);
        }

        offer.amount_repaid += amount;
        let fully_repaid = offer.amount_repaid >= total_owed;
        let new_status = if fully_repaid {
            OfferStatus::Repaid
        } else {
            OfferStatus::Financed
        };

        // Cross-contract: update offer in financing
        // CEI: External interactions. Safe because this contract has no local state to protect.
        financing_client.update_offer_status(&offer_id, &new_status);
        financing_client.update_offer_amount_repaid(&offer_id, &offer.amount_repaid);
        financing_client.update_lender_stats_repaid(&offer.lender, &fully_repaid);
        financing_client.update_stats_repaid(&amount, &fee_amount);

        // Cross-contract: mark the invoice repaid in the registry via the
        // system transition (the repayment contract is the authorized caller).
        // CEI: External interaction. Safe because this contract has no local state to protect.
        let updated_invoice =
            registry_client.repayment_marks_invoice_repaid(&invoice_id, &fully_repaid);

        // Reputation (Task 11): record a successful outcome on full repayment.
        // Only when a reputation contract is configured — deployments without
        // one behave exactly as before.
        if fully_repaid {
            let reputation_opt: Option<Address> =
                env.storage().instance().get(&symbol_short!("repadd"));
            if let Some(reputation_addr) = reputation_opt {
                let reputation_client = ReputationClient::new(&env, &reputation_addr);
                // CEI: External interaction. Safe because this contract has no local state to protect.
                reputation_client.record_outcome(&invoice.originator, &0);
            }
        }

        env.events().publish(
            (symbol_short!("inv_rep"), invoice_id),
            (offer_id, amount, fully_repaid),
        );
        updated_invoice
    }

    // ── Overdue / Reclaim ────────────────────────────────────────────────────

    /// Mark an invoice as overdue. Can be called by anyone after due_date.
    /// Cross-contract: delegates to registry's mark_invoice_overdue which
    /// handles the status transition and event emission.
    pub fn mark_overdue(env: Env, invoice_id: Symbol) -> Invoice {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);

        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: External interaction. Safe because this contract has no local state to protect.
        registry_client.mark_invoice_overdue(&invoice_id)
    }

    /// After grace period, lender can mark their offer Defaulted.
    /// Cross-contract: reads invoice from registry, updates offer in financing.
    pub fn reclaim_invoice(
        env: Env,
        invoice_id: Symbol,
        offer_id: Symbol,
        lender: Address,
    ) -> FinancingOffer {
        assert_not_paused(&env);
        assert_schema_version(&env, SCHEMA_VERSION);
        lender.require_auth();

        // Cross-contract: read invoice from registry
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call. Repayment has no local state to protect.
        let invoice: Invoice = registry_client.get_invoice(&invoice_id);

        if invoice.status != InvoiceStatus::Overdue {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        if env.ledger().timestamp() < invoice.due_date + GRACE_PERIOD_SECS {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: read offer from financing
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        // CEI: Read-only cross-contract call.
        let mut offer: FinancingOffer = financing_client.get_offer(&offer_id);

        if offer.invoice_id != invoice_id {
            env.panic_with_error(ContractError::InvalidInput);
        }
        if offer.lender != lender {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: update offer status in financing
        // CEI: External interaction. Safe because this contract has no local state to protect.
        financing_client.update_offer_status(&offer_id, &OfferStatus::Defaulted);

        offer.status = OfferStatus::Defaulted;

        // Task 10: transition the invoice Overdue -> Defaulted in the registry
        // (the repayment contract is the authorized caller). This is the
        // protocol's realized-credit-loss signal.
        // CEI: External interaction. Safe because this contract has no local state to protect.
        registry_client.repayment_marks_defaulted(&invoice_id);

        // Task 10: insurance payout hook. The lender's outstanding exposure is
        // principal + yield - already repaid. The pool pays up to its
        // available balance; pay_out returns what was actually paid (0 when
        // the pool is empty). Skipped entirely when no insurance contract is
        // configured.
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;
        let remaining_due = (total_due - offer.amount_repaid).max(0);

        // Overdue penalty (ADR-0007). Deliberately **excluded** from the
        // insured amount: the pool covers realized credit loss (principal +
        // yield, per ADR-0003), not the punitive charge owed by the
        // originator. Including it would make staker losses grow with how
        // long a defaulted invoice went unreclaimed. It is reported on the
        // event so indexers can track the lender's uncovered claim.
        let (penalty_bps, cap_bps) = load_penalty_config(&env);
        let penalty = accrued_penalty(&env, total_due, invoice.due_date, penalty_bps, cap_bps);

        let mut payout: i128 = 0;
        let insurance_opt: Option<Address> = env.storage().instance().get(&symbol_short!("insadd"));
        if let Some(insurance_addr) = insurance_opt {
            let insurance_client = InsuranceClient::new(&env, &insurance_addr);
            // CEI: External interaction. Safe because this contract has no local state to protect.
            payout = insurance_client.pay_out(&invoice_id, &offer.lender, &remaining_due);
        }

        // Task 11: reputation hook — record the originator's default.
        let reputation_opt: Option<Address> =
            env.storage().instance().get(&symbol_short!("repadd"));
        if let Some(reputation_addr) = reputation_opt {
            let reputation_client = ReputationClient::new(&env, &reputation_addr);
            // CEI: External interaction. Safe because this contract has no local state to protect.
            reputation_client.record_outcome(&invoice.originator, &1);
        }

        env.events().publish(
            (symbol_short!("off_def"), offer.id.clone()),
            (
                offer.invoice_id.clone(),
                offer.lender.clone(),
                payout,
                penalty,
            ),
        );
        offer
    }

    // ── Query helpers ────────────────────────────────────────────────────────

    /// Calculate the remaining total due on an offer
    /// (principal + yield + accrued overdue penalty - repaid).
    ///
    /// Cross-contract: reads the offer from financing and — since ADR-0007 —
    /// the invoice from the registry, because penalty accrual is anchored on
    /// `invoice.due_date`.
    pub fn calculate_total_due(env: Env, offer_id: Symbol) -> i128 {
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        // CEI: Read-only cross-contract call.
        let offer: FinancingOffer = financing_client.get_offer(&offer_id);

        if offer.status == OfferStatus::Repaid || offer.status == OfferStatus::Defaulted {
            return 0;
        }
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;
        let penalty = penalty_for_offer(&env, &offer);
        (total_due + penalty - offer.amount_repaid).max(0)
    }

    /// The overdue penalty accrued on an offer so far (ADR-0007), before
    /// subtracting anything already repaid. Returns 0 when accrual is
    /// disabled, when the invoice is not yet past due, or when the offer has
    /// reached a terminal status.
    ///
    /// Exposed separately so a UI can show the penalty component rather than
    /// only the combined figure from `calculate_total_due`.
    pub fn calculate_penalty(env: Env, offer_id: Symbol) -> i128 {
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        let offer: FinancingOffer = financing_client.get_offer(&offer_id);

        if offer.status == OfferStatus::Repaid || offer.status == OfferStatus::Defaulted {
            return 0;
        }
        penalty_for_offer(&env, &offer)
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    pub fn get_duration_limits(_env: Env) -> (u64, u64) {
        (MIN_OFFER_DURATION_SECS, MAX_OFFER_DURATION_SECS)
    }

    // ── Schedule helpers (issue #133) ────────────────────────────────────────

    /// Return the 1-based index of the installment that is currently due for
    /// the given offer, or 0 if none.  Delegates to the financing contract's
    /// `get_installment_due` so callers don't need to hold the financing
    /// address directly.
    pub fn get_installment_due(env: Env, offer_id: Symbol) -> u32 {
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        // CEI: Read-only cross-contract call.
        financing_client.get_installment_due(&offer_id)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
