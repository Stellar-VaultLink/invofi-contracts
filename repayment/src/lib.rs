#![no_std]

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Map, Symbol, Vec};

use invofi_common::{
    assert_not_paused, resolve_token, BatchItemResult, BatchRepayItem, BatchResult, ContractError,
    FinancingClient, FinancingOffer, InsuranceClient, Invoice, InvoiceStatus, OfferStatus,
    PaymentRecord, RegistryClient, ReputationClient, GRACE_PERIOD_SECS, MAX_BATCH_SIZE,
    MAX_OFFER_DURATION_SECS, MIN_OFFER_DURATION_SECS,
};

// ─── Overdue penalty (ADR-0007) ──────────────────────────────────────────────

/// Seconds in a day. Penalty accrues in whole elapsed days.
const SECS_PER_DAY: u64 = 86_400;

/// Upper bound on the configurable per-day penalty rate: 500 bps = 5%/day.
/// Guards against a mis-keyed admin call setting an absurd rate.
pub const MAX_PENALTY_BPS: u32 = 500;

/// Minimum partial payment threshold in basis points of the principal.
/// 100 bps = 1%. Prevents dust payments that waste gas.
const MIN_PARTIAL_PAYMENT_BPS: u32 = 100;

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

// ─── Payment History ────────────────────────────────────────────────────────

/// Load the payment history for an invoice. Returns an empty Vec if no
/// payments have been recorded yet.
fn load_payments(env: &Env, invoice_id: &Symbol) -> Vec<PaymentRecord> {
    env.storage()
        .persistent()
        .get(&(symbol_short!("pays"), invoice_id.clone()))
        .unwrap_or_else(|| Vec::new(env))
}

/// Persist the payment history for an invoice.
fn save_payments(env: &Env, invoice_id: &Symbol, payments: &Vec<PaymentRecord>) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("pays"), invoice_id.clone()), payments);
}

/// Calculate pro-rata interest on the remaining principal.
///
/// Formula: `remaining * rate_bps * days_elapsed / 3_650_000`
///
/// The denominator is `365 * 10_000` (days-in-year × bps divisor), which
/// converts the annual basis-point rate into a per-day fractional multiplier.
/// Rounding runs in the protocol's favour (toward zero) because integer
/// division truncates.
fn pro_rata_interest(remaining_principal: i128, rate_bps: u32, days_elapsed: i128) -> i128 {
    if remaining_principal <= 0 || rate_bps == 0 || days_elapsed <= 0 {
        return 0;
    }
    remaining_principal
        .saturating_mul(rate_bps as i128)
        .saturating_mul(days_elapsed)
        / 3_650_000
}

/// Sum the principal repaid across all stored payment records.
///
/// The iteration is bounded at `MAX_PAYMENTS_PER_INVOICE` to satisfy the
/// Soroban Scout `dos_unbounded_operation` detector. In practice an invoice
/// will never have more than a handful of payments.
const MAX_PAYMENTS_PER_INVOICE: u32 = 1_000;

fn total_principal_repaid(payments: &Vec<PaymentRecord>) -> i128 {
    let mut total: i128 = 0;
    let limit = payments.len().min(MAX_PAYMENTS_PER_INVOICE);
    let mut i: u32 = 0;
    while i < limit {
        if let Some(record) = payments.get(i) {
            total += record.principal_paid;
        }
        i += 1;
    }
    total
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

    /// Repay a financed invoice. Originator transfers payment to lender.
    /// Deducts protocol fee (if configured in financing) and forwards net.
    /// Updates remaining balance. If balance reaches 0, marks invoice Repaid
    /// and offer Repaid in financing contract.
    ///
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
        repayer.require_auth();
        Self::repay_invoice_internal(&env, invoice_id, offer_id, &repayer, amount)
            .unwrap_or_else(|err| env.panic_with_error(err))
    }

    fn repay_invoice_internal(
        env: &Env,
        invoice_id: Symbol,
        offer_id: Symbol,
        repayer: &Address,
        amount: i128,
    ) -> Result<Invoice, ContractError> {
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
.ok_or(ContractError::NotFound)?;
        let registry_client = RegistryClient::new(env, &registry_addr);
        let invoice: Invoice = match registry_client.try_get_invoice(&invoice_id) {
            Ok(Ok(inv)) => inv,
            _ => return Err(ContractError::NotFound),
        };

        if invoice.originator != *repayer {
            return Err(ContractError::Unauthorized);
        }
        if invoice.status != InvoiceStatus::Financed {
            return Err(ContractError::InvalidTransition);
        }

        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
.ok_or(ContractError::NotFound)?;
        let financing_client = FinancingClient::new(env, &financing_addr);
        let mut offer: FinancingOffer = match financing_client.try_get_offer(&offer_id) {
            Ok(Ok(off)) => off,
            _ => return Err(ContractError::NotFound),
        };

        if offer.invoice_id != invoice_id {
            return Err(ContractError::InvalidInput);
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            return Err(ContractError::InvalidTransition);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidInput);
        }

        let token_id = resolve_token(env, &offer.currency);
        let token_client = token::TokenClient::new(env, &token_id);

        let payments = load_payments(env, &invoice_id);
        let principal_repaid_so_far = total_principal_repaid(&payments);
        let remaining_principal = (offer.amount - principal_repaid_so_far).max(0);

        let now = env.ledger().timestamp();
        let days_since_funded = ((now - offer.funded_at) / SECS_PER_DAY) as i128;
        let accrued_interest =
            pro_rata_interest(remaining_principal, offer.interest_rate, days_since_funded);

        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let frozen_base = offer.amount + yield_amount;
        let (penalty_bps, cap_bps) = load_penalty_config(env);
        let penalty = accrued_penalty(env, frozen_base, invoice.due_date, penalty_bps, cap_bps);

        let total_owed = remaining_principal + accrued_interest + penalty;

        let min_payment = offer.amount * (MIN_PARTIAL_PAYMENT_BPS as i128) / 10_000;
        if amount < min_payment && amount < total_owed {
            return Err(ContractError::InvalidInput);
        }

        if amount > total_owed {
            return Err(ContractError::InsufficientBalance);
        }

        // Split the payment: interest first, then principal.
        let interest_portion = amount.min(accrued_interest);
        let principal_portion = amount - interest_portion;

        // Protocol fee deduction (applied to the total payment amount)
        let fee_bps: u32 = financing_client.get_fee_bps();
        let fee_amount = amount * (fee_bps as i128) / 10_000;
        let lender_amount = amount - fee_amount;
        // CEI: External interaction. Safe because this contract has no local state to protect.
        token_client.transfer(repayer, &offer.lender, &lender_amount);
        if fee_amount > 0 {
            let admin: Address = env
                .storage()
                .instance()
                .get(&symbol_short!("admin"))
                .ok_or(ContractError::NotFound)?;
            // CEI: External interaction. Safe because this contract has no local state to protect.
            token_client.transfer(repayer, &admin, &fee_amount);
        }

        // ── Store payment record ───────────────────────────────────────────
        let payment_id = payments.len() + 1;
        let record = PaymentRecord {
            payment_id,
            amount,
            interest_paid: interest_portion,
            principal_paid: principal_portion,
            timestamp: now,
            payer: repayer.clone(),
        };
        let mut updated_payments = payments;
        updated_payments.push_back(record);
        save_payments(env, &invoice_id, &updated_payments);

        // Determine if fully repaid: remaining principal is zero after this payment.
        let new_remaining = remaining_principal - principal_portion;
        let fully_repaid = new_remaining <= 0;

        // Update offer.amount_repaid for backward compatibility with financing contract.
        offer.amount_repaid += amount;
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
                let reputation_client = ReputationClient::new(env, &reputation_addr);
                // CEI: External interaction. Safe because this contract has no local state to protect.
                reputation_client.record_outcome(&invoice.originator, &0);
            }
        }

        // Emit the appropriate event.
        if fully_repaid {
            env.events().publish(
                (symbol_short!("inv_frp"), invoice_id.clone()),
                (
                    offer_id.clone(),
                    amount,
                    principal_portion,
                    interest_portion,
                ),
            );
        } else {
            env.events().publish(
                (symbol_short!("parpay"), invoice_id.clone()),
                (
                    offer_id.clone(),
                    amount,
                    principal_portion,
                    interest_portion,
                    new_remaining,
                ),
            );
        }

        // Legacy event for backward compatibility with indexers.
        env.events().publish(
            (symbol_short!("inv_rep"), invoice_id),
            (offer_id, amount, fully_repaid),
        );
        Ok(updated_invoice)
    }

    /// Repay multiple invoices in a single transaction (1-100 items).
    pub fn batch_repay_invoices(
        env: Env,
        repayer: Address,
        items: Vec<BatchRepayItem>,
        allow_partial: bool,
    ) -> BatchResult {
        assert_not_paused(&env);
        repayer.require_auth();

        let total_count = items.len();
        if total_count == 0 || total_count > MAX_BATCH_SIZE {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let mut results = Vec::new(&env);
        let mut success_count: u32 = 0;
        let mut failure_count: u32 = 0;
        let mut processed_ids = Map::<Symbol, bool>::new(&env);

        let limit = items.len().min(MAX_BATCH_SIZE);
        if !allow_partial {
            let mut i: u32 = 0;
            while i < limit {
                let item = items
                    .get(i)
                    .unwrap_or_else(|| env.panic_with_error(ContractError::InvalidInput));

                if processed_ids.contains_key(item.invoice_id.clone()) {
                    env.panic_with_error(ContractError::AlreadyExists);
                }
                processed_ids.set(item.invoice_id.clone(), true);

                Self::repay_invoice_internal(
                    &env,
                    item.invoice_id.clone(),
                    item.offer_id.clone(),
                    &repayer,
                    item.amount,
                )
                .unwrap_or_else(|err| env.panic_with_error(err));

                results.push_back(BatchItemResult {
                    id: item.invoice_id.clone(),
                    success: true,
                    error_code: 0,
                });
                i += 1;
            }
            success_count = total_count;
            env.events().publish(
                (symbol_short!("btch_rpy"), repayer.clone()),
                (success_count, 0u32),
            );
        } else {
            let mut i: u32 = 0;
            while i < limit {
                let item = items
                    .get(i)
                    .unwrap_or_else(|| env.panic_with_error(ContractError::InvalidInput));
                let mut err_code: u32 = 0;

                if processed_ids.contains_key(item.invoice_id.clone()) {
                    err_code = ContractError::AlreadyExists as u32;
                } else {
                    processed_ids.set(item.invoice_id.clone(), true);
                    match Self::repay_invoice_internal(
                        &env,
                        item.invoice_id.clone(),
                        item.offer_id.clone(),
                        &repayer,
                        item.amount,
                    ) {
                        Ok(_) => {
                            success_count += 1;
                            results.push_back(BatchItemResult {
                                id: item.invoice_id.clone(),
                                success: true,
                                error_code: 0,
                            });
                        }
                        Err(err) => {
                            err_code = err as u32;
                        }
                    }
                }

                if err_code != 0 {
                    failure_count += 1;
                    results.push_back(BatchItemResult {
                        id: item.invoice_id.clone(),
                        success: false,
                        error_code: err_code,
                    });
                }
                i += 1;
            }
            env.events().publish(
                (symbol_short!("btch_rpy"), repayer.clone()),
                (success_count, failure_count),
            );
        }

        BatchResult {
            total_processed: total_count,
            success_count,
            failure_count,
            results,
        }
    }

    // ── Overdue / Reclaim ────────────────────────────────────────────────────

    /// Mark an invoice as overdue. Can be called by anyone after due_date.
    /// Cross-contract: delegates to registry's mark_invoice_overdue which
    /// handles the status transition and event emission.
    pub fn mark_overdue(env: Env, invoice_id: Symbol) -> Invoice {
        assert_not_paused(&env);

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
        // Pro-rata: remaining principal + accrued interest + penalty.
        let payments = load_payments(&env, &offer.invoice_id);
        let principal_repaid = total_principal_repaid(&payments);
        let remaining = (offer.amount - principal_repaid).max(0);
        let now = env.ledger().timestamp();
        let days = ((now - offer.funded_at) / SECS_PER_DAY) as i128;
        let interest = pro_rata_interest(remaining, offer.interest_rate, days);
        let penalty = penalty_for_offer(&env, &offer);
        (remaining + interest + penalty).max(0)
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

    // ── Partial repayment queries (issue #176) ─────────────────────────────

    /// Return the full payment history for an invoice as a Vec of
    /// `PaymentRecord`. Empty if no payments have been made.
    pub fn get_payment_history(env: Env, invoice_id: Symbol) -> Vec<PaymentRecord> {
        load_payments(&env, &invoice_id)
    }

    /// Return the remaining principal on an offer (original principal minus
    /// the sum of all principal portions recorded in payment history).
    pub fn get_remaining_principal(env: Env, offer_id: Symbol) -> i128 {
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
        let payments = load_payments(&env, &offer.invoice_id);
        let principal_repaid = total_principal_repaid(&payments);
        (offer.amount - principal_repaid).max(0)
    }

    /// Calculate the pro-rata accrued interest on an offer's remaining
    /// principal. Returns 0 for terminal-status offers.
    pub fn calculate_accrued_interest(env: Env, offer_id: Symbol) -> i128 {
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
        let payments = load_payments(&env, &offer.invoice_id);
        let principal_repaid = total_principal_repaid(&payments);
        let remaining = (offer.amount - principal_repaid).max(0);
        let now = env.ledger().timestamp();
        let days = ((now - offer.funded_at) / SECS_PER_DAY) as i128;
        pro_rata_interest(remaining, offer.interest_rate, days)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
