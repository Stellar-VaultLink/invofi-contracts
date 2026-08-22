#![no_std]

use soroban_sdk::{
    contract, contractimpl, symbol_short, token, Address, BytesN, Env, Map, Symbol, Vec,
};

use invofi_common::{
    assert_not_paused, assert_transition, get_transition_history, Attestation, ContractError,
    Invoice, InvoiceStatus, ProtocolStats, RiskTier, TransitionRecord, VerificationStatus,
    VerificationType, DEFAULT_ATTESTATION_VALIDITY_SECS, MAX_ATTESTATIONS_PER_INVOICE,
    MAX_ATTESTATION_VALIDITY_SECS, MAX_VERIFICATION_FEE_BPS, MAX_VERIFIERS,
    MIN_ATTESTATION_VALIDITY_SECS, MIN_INVOICE_AMOUNT, VERIFICATION_TYPES,
};

// ─── Storage Helpers ─────────────────────────────────────────────────────────

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

fn load_rates(env: &Env) -> Map<RiskTier, u32> {
    env.storage()
        .persistent()
        .get(&symbol_short!("rates"))
        .unwrap_or(Map::new(env))
}

fn save_rates(env: &Env, map: &Map<RiskTier, u32>) {
    env.storage().persistent().set(&symbol_short!("rates"), map);
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

// ─── Verification Oracle Storage Helpers (issue #181) ────────────────────────
//
//   ("verifs", invoice_id) -> Vec<Attestation>   attestations for one invoice
//   ("verifrs")            -> Vec<Address>       the trusted verifier set

fn load_verifications(env: &Env, invoice_id: &Symbol) -> Vec<Attestation> {
    env.storage()
        .persistent()
        .get(&(symbol_short!("verifs"), invoice_id.clone()))
        .unwrap_or(Vec::new(env))
}

fn save_verifications(env: &Env, invoice_id: &Symbol, list: &Vec<Attestation>) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("verifs"), invoice_id.clone()), list);
}

fn load_verifiers(env: &Env) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&symbol_short!("verifrs"))
        .unwrap_or(Vec::new(env))
}

fn save_verifiers(env: &Env, list: &Vec<Address>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("verifrs"), list);
}

/// An attestation's status as of `now`.
///
/// Expiry is derived here rather than stored, so a read is correct whether or
/// not anyone has called `expire_verifications` yet. A lapsed *rejection*
/// expires too: a stale "this invoice is bad" is no more informative than a
/// stale "this invoice is good".
fn effective_status(attestation: &Attestation, now: u64) -> VerificationStatus {
    if attestation.status != VerificationStatus::Expired && now > attestation.valid_until {
        VerificationStatus::Expired
    } else {
        attestation.status
    }
}

/// Status of one verification type on one invoice.
///
/// `attest` keeps at most one attestation per (verifier, type), so counting
/// affirmative attestations *is* counting distinct verifiers — no verifier can
/// reach the threshold alone by attesting repeatedly.
fn type_status(
    env: &Env,
    attestations: &Vec<Attestation>,
    v_type: VerificationType,
    threshold: u32,
    now: u64,
) -> VerificationStatus {
    let mut approvals: u32 = 0;
    let mut rejected = false;
    let mut expired = false;
    let mut seen = false;

    // Only the current verifier set speaks to status. Removing a verifier is
    // how the admin withdraws trust -- usually because the key is compromised
    // or the verifier was found negligent -- so their statements must stop
    // counting the moment they leave, or removal would not actually revoke
    // anything. The records stay readable through `get_verifications` -- the
    // history of who said what is preserved, it just no longer votes -- until
    // the invoice hits its cap, where departed verifiers' records are the
    // first thing `evict_one_slot` reclaims.
    let trusted = load_verifiers(env);

    for attestation in attestations.iter() {
        if attestation.v_type != v_type
            || !trusted.iter().any(|entry| entry == attestation.verifier)
        {
            continue;
        }
        seen = true;
        match effective_status(&attestation, now) {
            VerificationStatus::Verified => approvals += 1,
            VerificationStatus::Rejected => rejected = true,
            VerificationStatus::Expired => expired = true,
            VerificationStatus::Pending => {}
        }
    }

    if !seen {
        return VerificationStatus::Pending;
    }
    // A live rejection outranks approvals: one verifier saying the document is
    // forged is not outvoted by two saying it looks fine.
    if rejected {
        return VerificationStatus::Rejected;
    }
    if approvals >= threshold {
        return VerificationStatus::Verified;
    }
    // `Expired` means the type has no live statement left. A type that still
    // holds a live approval but has not reached the threshold is
    // under-attested, not expired -- it needs another verifier, not a refresh
    // of one that already spoke.
    if expired && approvals == 0 {
        return VerificationStatus::Expired;
    }
    VerificationStatus::Pending
}

/// Free one slot in a full attestation list so a trusted verifier is never
/// locked out of an invoice.
///
/// The cap is `MAX_VERIFIERS x 3`, which is exactly enough for the whole
/// active set to speak on all three types — but records from verifiers that
/// have since been removed keep occupying slots, so a rotated-through set can
/// fill the list and permanently block admission. History yields to liveness
/// in a defined order: the oldest record from a verifier that is no longer
/// trusted goes first, then the oldest lapsed record.
///
/// Under the current constants only the first pass can ever run, and that
/// makes eviction **fully status-preserving**. The arithmetic: eviction is
/// reached only when the list still holds `MAX_ATTESTATIONS_PER_INVOICE`
/// records after excluding the incoming verifier's own record for this type.
/// Were every one of those 60 from a current verifier, then with at most
/// `MAX_VERIFIERS` (20) of them, one record per (verifier, type) and three
/// types, the list would have to be exactly 20 x 3 -- meaning the incoming
/// verifier already holds this type, its record is the one excluded, and the
/// count is 59, so eviction is never reached at all. So whenever eviction
/// *does* run, at least one record belongs to a departed verifier, and pass
/// one finds it. Departed records are filtered out of `type_status`, so
/// dropping one cannot move any status.
///
/// The lapsed pass and the refusal below are therefore unreachable today.
/// They are kept because they are the correct behaviour if
/// `MAX_VERIFIERS x 3` and `MAX_ATTESTATIONS_PER_INVOICE` are ever moved out
/// of that equality, and because the ordering states the intent: a lapsed
/// record is the next-least-valuable thing to drop. If the lapsed pass ever
/// did run it could take a type whose only remaining record is a lapsed one
/// from `Expired` to `Pending` -- both non-verified states that gate
/// financing identically -- but it can never touch `Verified` or `Rejected`,
/// which rest on live records from current verifiers.
///
/// The final `None` arm is unreachable for the same reason, and is likewise
/// kept as the correct behaviour: refuse the attestation rather than drop a
/// live statement from an active verifier.
fn evict_one_slot(env: &Env, list: &Vec<Attestation>, now: u64) -> Vec<Attestation> {
    let trusted = load_verifiers(env);
    let is_trusted = |who: &Address| trusted.iter().any(|entry| entry == *who);

    let mut victim: Option<u32> = None;
    // Pass 1: the oldest record whose verifier has been removed from the set.
    for (idx, attestation) in list.iter().enumerate() {
        if !is_trusted(&attestation.verifier) {
            victim = Some(idx as u32);
            break;
        }
    }
    // Pass 2: failing that, the oldest record that has lapsed.
    if victim.is_none() {
        for (idx, attestation) in list.iter().enumerate() {
            if effective_status(&attestation, now) == VerificationStatus::Expired {
                victim = Some(idx as u32);
                break;
            }
        }
    }

    let victim = match victim {
        Some(idx) => idx,
        None => env.panic_with_error(ContractError::InvalidTransition),
    };

    let mut kept: Vec<Attestation> = Vec::new(env);
    for (idx, attestation) in list.iter().enumerate() {
        if idx as u32 != victim {
            kept.push_back(attestation);
        }
    }
    kept
}

fn assert_not_blacklisted(env: &Env, address: &Address) {
    let list = load_blacklist(env);
    for entry in list.iter() {
        if entry == *address {
            env.panic_with_error(ContractError::Blacklisted);
        }
    }
}

fn assert_admin(env: &Env, caller: &Address) {
    caller.require_auth();
    let current: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("admin"))
        .unwrap_or_else(|| panic!("Not initialized"));
    if current != *caller {
        env.panic_with_error(ContractError::Unauthorized);
    }
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct RegistryContract;

#[contractimpl]
impl RegistryContract {
    // ── Admin / initialization ───────────────────────────────────────────────

    /// One-time setup. Sets the admin address.
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run — a fresh
    /// deployment can never be hijacked by a third party setting themselves
    /// as admin (issue #75).
    pub fn __constructor(env: Env, admin: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("Already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
    }

    /// Returns the admin address. Panics if not yet initialized.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Transfers admin rights to a new address. Only the current admin can
    /// call this.
    pub fn transfer_admin(env: Env, admin: Address, new_admin: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &new_admin);
    }

    /// Register the financing contract address. Admin only. The financing
    /// contract is the only caller allowed to transition a Pending invoice to
    /// Financed via `transition_invoice_status`.
    pub fn set_financing_contract(env: Env, admin: Address, financing: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("financing"), &financing);
    }

    /// Register the repayment contract address. Admin only. The repayment
    /// contract is the only caller allowed to transition a Financed invoice
    /// to Financed (partial) or Repaid (full) via `transition_invoice_status`.
    pub fn set_repayment_contract(env: Env, admin: Address, repayment: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("repayment"), &repayment);
    }

    // ── Pause / unpause ──────────────────────────────────────────────────────

    /// Halt all state-mutating operations. Admin only.
    pub fn pause(env: Env, admin: Address) {
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &true);
    }

    /// Resume operations after a pause. Admin only.
    pub fn unpause(env: Env, admin: Address) {
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &false);
    }

    /// Returns true if the contract is currently paused.
    pub fn contract_is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("paused"))
            .unwrap_or(false)
    }

    // ── Yield rate oracle ────────────────────────────────────────────────────

    /// Sets the yield rate (in basis points, 0-10000) for a risk tier.
    /// Admin only.
    pub fn set_rate(env: Env, admin: Address, tier: RiskTier, rate_bps: u32) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        if rate_bps > 10_000 {
            env.panic_with_error(ContractError::InvalidInput);
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
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound))
    }

    // ── Protocol fee ─────────────────────────────────────────────────────────

    /// Set the protocol fee in basis points (max 500 = 5%). Admin only.
    pub fn set_fee(env: Env, admin: Address, fee_bps: u32) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        if fee_bps > 500 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("feebps"), &fee_bps);
    }

    /// Returns the configured protocol fee in basis points (default 0).
    pub fn get_fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("feebps"))
            .unwrap_or(0)
    }

    // ── Invoice CRUD ─────────────────────────────────────────────────────────

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
        if amount < MIN_INVOICE_AMOUNT {
            env.panic_with_error(ContractError::InvalidInput);
        }
        if due_date <= env.ledger().timestamp() {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let mut invoices = load_invoices(&env);
        if invoices.contains_key(id.clone()) {
            env.panic_with_error(ContractError::AlreadyExists);
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

        let mut s = load_stats(&env);
        s.total_invoices += 1;
        save_stats(&env, &s);

        env.events().publish(
            (symbol_short!("inv_reg"), invoice.id.clone()),
            (invoice.originator.clone(), amount, due_date),
        );
        invoice
    }

    /// Get an invoice by ID.
    pub fn get_invoice(env: Env, id: Symbol) -> Invoice {
        load_invoices(&env)
            .get(id)
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound))
    }

    /// Manually cancel a Pending invoice. Only the invoice originator can call
    /// this. Restricted to `Pending → Cancelled`; for all other lifecycle
    /// transitions use the dedicated entry points.
    pub fn update_invoice_status(
        env: Env,
        id: Symbol,
        originator: Address,
        new_status: InvoiceStatus,
    ) -> Invoice {
        assert_not_paused(&env);
        originator.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if invoice.originator != originator {
            env.panic_with_error(ContractError::Unauthorized);
        }
        let old_status = invoice.status;
        assert_transition(&env, id.clone(), old_status, new_status, originator.clone());

        invoice.status = new_status;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    /// Update the face amount of a Pending invoice. Only the originator.
    pub fn update_invoice_amount(
        env: Env,
        invoice_id: Symbol,
        originator: Address,
        new_amount: i128,
    ) -> Invoice {
        assert_not_paused(&env);
        originator.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if invoice.originator != originator {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        if new_amount < MIN_INVOICE_AMOUNT {
            env.panic_with_error(ContractError::InvalidInput);
        }
        invoice.amount = new_amount;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events()
            .publish((symbol_short!("inv_amt"), invoice.id.clone()), new_amount);
        invoice
    }

    /// Cancel a Pending invoice. Only the originator can call this.
    pub fn cancel_invoice(env: Env, invoice_id: Symbol, originator: Address) -> Invoice {
        assert_not_paused(&env);
        originator.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if invoice.originator != originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        let old_status = invoice.status;
        assert_transition(
            &env,
            invoice_id.clone(),
            old_status,
            InvoiceStatus::Cancelled,
            originator.clone(),
        );

        invoice.status = InvoiceStatus::Cancelled;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_cxl"), invoice.originator.clone()),
            invoice.originator.clone(),
        );
        invoice
    }

    // ── Repayment status transitions ───────────────────────────────────────

    /// Transition a Financed invoice to Repaid or keep it Financed (partial
    /// repayment). Requires the repayer's auth. Only works on Financed invoices.
    pub fn set_invoice_repaid_status(
        env: Env,
        id: Symbol,
        repayer: Address,
        fully_repaid: bool,
    ) -> Invoice {
        assert_not_paused(&env);
        repayer.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        let new_status = if fully_repaid {
            InvoiceStatus::Repaid
        } else {
            InvoiceStatus::Financed
        };

        let old_status = invoice.status;
        assert_transition(&env, id.clone(), old_status, new_status, repayer.clone());

        invoice.status = new_status;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_sts"), invoice.id.clone()),
            invoice.status,
        );
        invoice
    }

    /// System transition: Pending -> Financed on offer acceptance.
    /// Only the registered financing contract may call this. Soroban does not
    /// forward *user* auth across contract boundaries, so the financing
    /// contract is authorized by address via the host's implicit
    /// contract-invoker auth (see Stellar docs — Authorization).
    pub fn financing_marks_invoice_financed(env: Env, id: Symbol) -> Invoice {
        assert_not_paused(&env);
        let financing: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Financing contract not configured"));
        financing.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        let old_status = invoice.status;
        assert_transition(
            &env,
            id.clone(),
            old_status,
            InvoiceStatus::Financed,
            financing.clone(),
        );

        invoice.status = InvoiceStatus::Financed;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_sts"), invoice.id.clone()),
            InvoiceStatus::Financed,
        );
        invoice
    }

    /// System transition: Financed -> Financed (partial) / Repaid (full) on
    /// repayment. Only the registered repayment contract may call this,
    /// authorized via implicit contract-invoker auth.
    pub fn repayment_marks_invoice_repaid(env: Env, id: Symbol, fully_repaid: bool) -> Invoice {
        assert_not_paused(&env);
        let repayment: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("repayment"))
            .unwrap_or_else(|| panic!("Repayment contract not configured"));
        repayment.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        let new_status = if fully_repaid {
            InvoiceStatus::Repaid
        } else {
            InvoiceStatus::Financed
        };

        let old_status = invoice.status;
        assert_transition(&env, id.clone(), old_status, new_status, repayment.clone());

        invoice.status = new_status;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_sts"), invoice.id.clone()),
            invoice.status,
        );
        invoice
    }

    // ── Overdue marking ─────────────────────────────────────────────────────

    /// System transition: Overdue -> Defaulted on lender reclaim.
    /// Only the registered repayment contract may call this, authorized via
    /// implicit contract-invoker auth. Marks the invoice as a realized
    /// credit loss, which is what triggers the insurance payout hook and the
    /// originator's reputation default record.
    pub fn repayment_marks_defaulted(env: Env, id: Symbol) -> Invoice {
        assert_not_paused(&env);
        let repayment: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("repayment"))
            .unwrap_or_else(|| panic!("Repayment contract not configured"));
        repayment.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        let old_status = invoice.status;
        assert_transition(
            &env,
            id.clone(),
            old_status,
            InvoiceStatus::Defaulted,
            repayment.clone(),
        );

        invoice.status = InvoiceStatus::Defaulted;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_def"), invoice.id.clone()),
            invoice.originator.clone(),
        );
        invoice
    }

    /// Mark a Financed invoice as Overdue. Can be called by anyone after
    /// due_date has passed. This is a public status transition that doesn't
    /// require originator auth — the time-based condition is sufficient.
    pub fn mark_invoice_overdue(env: Env, id: Symbol) -> Invoice {
        assert_not_paused(&env);
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        // Validate time-based precondition before state transition
        if env.ledger().timestamp() <= invoice.due_date {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        let old_status = invoice.status;
        // For overdue, we use a dummy actor since it's permissionless
        let dummy_actor = env.current_contract_address();
        assert_transition(
            &env,
            id.clone(),
            old_status,
            InvoiceStatus::Overdue,
            dummy_actor,
        );

        invoice.status = InvoiceStatus::Overdue;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_ovd"), invoice.id.clone()),
            invoice.due_date,
        );
        invoice
    }

    // ── Dispute management ───────────────────────────────────────────────────

    /// Mark a Financed invoice as Disputed. Only the originator.
    pub fn raise_dispute(env: Env, invoice_id: Symbol, originator: Address) -> Invoice {
        assert_not_paused(&env);
        originator.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if invoice.originator != originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        let old_status = invoice.status;
        assert_transition(
            &env,
            invoice_id.clone(),
            old_status,
            InvoiceStatus::Disputed,
            originator.clone(),
        );

        invoice.status = InvoiceStatus::Disputed;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_dsp"), invoice.id.clone()),
            invoice.originator.clone(),
        );
        invoice
    }

    /// Resolve a Disputed invoice. Admin only.
    /// Allowed targets: Financed, Repaid, Cancelled, Defaulted.
    pub fn resolve_dispute(
        env: Env,
        admin: Address,
        invoice_id: Symbol,
        target_status: InvoiceStatus,
    ) -> Invoice {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if target_status == InvoiceStatus::Disputed {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let old_status = invoice.status;
        assert_transition(
            &env,
            invoice_id.clone(),
            old_status,
            target_status,
            admin.clone(),
        );

        invoice.status = target_status;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_rsl"), invoice.id.clone()),
            invoice.status,
        );
        invoice
    }

    // ── Query helpers ────────────────────────────────────────────────────────

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

    /// Return all registered invoices. Admin-only analytics function.
    /// At scale, prefer paginated queries — this returns an unbounded Vec.
    pub fn get_all_invoices(env: Env) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (_id, inv) in invoices.iter() {
            result.push_back(inv);
        }
        result
    }

    pub fn get_invoices_by_currency(env: Env, currency: Symbol) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (_id, inv) in invoices.iter() {
            if inv.currency == currency {
                result.push_back(inv);
            }
        }
        result
    }

    pub fn get_invoices_due_before(env: Env, timestamp: u64) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (_id, inv) in invoices.iter() {
            let is_open =
                inv.status == InvoiceStatus::Pending || inv.status == InvoiceStatus::Financed;
            if is_open && inv.due_date < timestamp {
                result.push_back(inv);
            }
        }
        result
    }

    pub fn get_invoices_paginated(env: Env, offset: u32, limit: u32) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for (idx, (_id, inv)) in invoices.iter().enumerate() {
            if idx as u32 >= offset && result.len() < limit {
                result.push_back(inv);
            }
            if result.len() >= limit {
                break;
            }
        }
        result
    }

    pub fn batch_get_invoices(env: Env, ids: Vec<Symbol>) -> Vec<Invoice> {
        let invoices = load_invoices(&env);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for id in ids.iter() {
            if let Some(inv) = invoices.get(id) {
                result.push_back(inv);
            }
        }
        result
    }

    pub fn get_invoices_count(env: Env) -> u32 {
        load_invoices(&env).len()
    }

    pub fn get_stats(env: Env) -> ProtocolStats {
        load_stats(&env)
    }

    // ── Blacklist management ─────────────────────────────────────────────────

    pub fn blacklist_address(env: Env, admin: Address, target: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        let mut list = load_blacklist(&env);
        for entry in list.iter() {
            if entry == target {
                return;
            }
        }
        list.push_back(target);
        save_blacklist(&env, &list);
    }

    pub fn unblacklist_address(env: Env, admin: Address, target: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        let list = load_blacklist(&env);
        let mut new_list: Vec<Address> = Vec::new(&env);
        for entry in list.iter() {
            if entry != target {
                new_list.push_back(entry);
            }
        }
        save_blacklist(&env, &new_list);
    }

    pub fn is_blacklisted(env: Env, address: Address) -> bool {
        let list = load_blacklist(&env);
        for entry in list.iter() {
            if entry == address {
                return true;
            }
        }
        false
    }

    pub fn get_blacklist(env: Env) -> Vec<Address> {
        load_blacklist(&env)
    }

    // ── Verification oracle (issue #181) ─────────────────────────────────────
    //
    // On-chain invoice data is self-reported. This is the layer that lets
    // trusted off-chain verifiers put their name to the facts behind it —
    // document hashes, business registration, tax compliance — and have that
    // statement stored immutably against the invoice.
    //
    // What the contract can and cannot do is worth stating plainly, and is
    // spelled out in `docs/adr/0009-verification-oracle.md`: it cannot check
    // that a PDF hash belongs to a real invoice or that a registration number
    // is genuine. It authenticates that a verifier from the admin-governed set
    // said so, charges for the work, timestamps it, and expires it. Trust is
    // bounded by verifier honesty and by the m-of-n threshold, not eliminated.

    /// Add an address to the trusted verifier set. Admin only. Idempotent.
    pub fn add_verifier(env: Env, admin: Address, verifier: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        let mut verifiers = load_verifiers(&env);
        for existing in verifiers.iter() {
            if existing == verifier {
                return;
            }
        }
        if verifiers.len() >= MAX_VERIFIERS {
            env.panic_with_error(ContractError::InvalidInput);
        }
        verifiers.push_back(verifier);
        save_verifiers(&env, &verifiers);
    }

    /// Remove an address from the trusted verifier set. Admin only.
    ///
    /// Attestations that verifier already submitted are left in place — the
    /// history of who said what, when, is the point of storing them. They
    /// still count until they expire; an admin who needs a compromised
    /// verifier's statements discounted immediately raises the threshold, per
    /// the key-compromise runbook (#145).
    pub fn remove_verifier(env: Env, admin: Address, verifier: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        let verifiers = load_verifiers(&env);
        let mut remaining: Vec<Address> = Vec::new(&env);
        for existing in verifiers.iter() {
            if existing != verifier {
                remaining.push_back(existing);
            }
        }
        save_verifiers(&env, &remaining);
    }

    pub fn get_verifiers(env: Env) -> Vec<Address> {
        load_verifiers(&env)
    }

    pub fn is_verifier(env: Env, address: Address) -> bool {
        for existing in load_verifiers(&env).iter() {
            if existing == address {
                return true;
            }
        }
        false
    }

    /// Number of distinct verifiers that must attest affirmatively before a
    /// verification type counts as Verified — the m of m-of-n. Admin only.
    ///
    /// The threshold is *not* clamped to the current verifier-set size: an
    /// admin may deliberately set it above the set while adding verifiers, and
    /// a removal must not silently weaken it. A threshold no live verifier set
    /// can reach simply means nothing verifies, which is the safe direction.
    pub fn set_verifier_threshold(env: Env, admin: Address, threshold: u32) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        if threshold == 0 || threshold > MAX_VERIFIERS {
            env.panic_with_error(ContractError::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("verthr"), &threshold);
    }

    /// The configured verifier threshold (default 1).
    pub fn get_verifier_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("verthr"))
            .unwrap_or(1)
    }

    /// Set the verification fee in basis points of invoice value. Admin only.
    /// Capped at `MAX_VERIFICATION_FEE_BPS` (5%). Defaults to 0, which
    /// disables fee settlement entirely.
    pub fn set_verification_fee(env: Env, admin: Address, fee_bps: u32) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        if fee_bps > MAX_VERIFICATION_FEE_BPS {
            env.panic_with_error(ContractError::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("verfee"), &fee_bps);
    }

    /// The configured verification fee in basis points (default 0).
    pub fn get_verification_fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("verfee"))
            .unwrap_or(0)
    }

    /// Set how long a new attestation stays valid, in seconds. Admin only.
    /// Clamped to [`MIN_ATTESTATION_VALIDITY_SECS`,
    /// `MAX_ATTESTATION_VALIDITY_SECS`]; defaults to 90 days.
    ///
    /// Changing this does not move the `valid_until` of attestations already
    /// submitted — each one carries its own, fixed at submission.
    pub fn set_attestation_validity(env: Env, admin: Address, validity_secs: u64) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        if !(MIN_ATTESTATION_VALIDITY_SECS..=MAX_ATTESTATION_VALIDITY_SECS).contains(&validity_secs)
        {
            env.panic_with_error(ContractError::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("vervld"), &validity_secs);
    }

    /// The configured attestation validity in seconds (default 90 days).
    pub fn get_attestation_validity(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&symbol_short!("vervld"))
            .unwrap_or(DEFAULT_ATTESTATION_VALIDITY_SECS)
    }

    /// Register the SEP-41 token that settles verification fees denominated in
    /// `currency`. Admin only.
    ///
    /// Verification fees are paid in the invoice's own currency, so a
    /// multi-currency deployment registers each currency once here — the same
    /// pattern the financing contract uses, sharing the `invofi_common`
    /// registry rather than growing a per-currency branch.
    pub fn register_currency(env: Env, admin: Address, currency: Symbol, token_addr: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        invofi_common::register_currency(&env, &currency, &token_addr);
    }

    /// The token registered to settle `currency`, if any.
    pub fn get_currency_token(env: Env, currency: Symbol) -> Option<Address> {
        invofi_common::get_currency_token(&env, &currency)
    }

    /// The fee an attestation on this invoice currently costs, in the
    /// invoice's currency: `invoice.amount * verification_fee_bps / 10_000`.
    ///
    /// The multiplication is checked and the division happens last, so a large
    /// invoice cannot silently wrap into a small (or negative) fee.
    pub fn calculate_verification_fee(env: Env, invoice_id: Symbol) -> i128 {
        let invoice = load_invoices(&env)
            .get(invoice_id)
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        Self::fee_for(&env, invoice.amount)
    }

    fn fee_for(env: &Env, invoice_amount: i128) -> i128 {
        let fee_bps = Self::get_verification_fee(env.clone());
        if fee_bps == 0 {
            return 0;
        }
        invoice_amount
            .checked_mul(fee_bps as i128)
            .unwrap_or_else(|| env.panic_with_error(ContractError::InvalidInput))
            / 10_000
    }

    /// Submit an attestation about one off-chain fact for an invoice.
    ///
    /// Only an address in the trusted verifier set may call this, and only on
    /// an invoice that is still Pending or Financed — verification of a
    /// cancelled or settled invoice would charge a fee for a fact nothing can
    /// act on.
    ///
    /// `approved` is the verifier's verdict: `true` records an affirmative
    /// attestation, `false` a rejection. **The fee is charged either way**,
    /// because it pays for the verification work, not for a favourable answer
    /// — a fee contingent on approval would pay verifiers to approve.
    ///
    /// A verifier re-attesting the same type on the same invoice **replaces**
    /// their earlier statement rather than stacking on it. That is what keeps
    /// the threshold a count of distinct verifiers, and it is how a verifier
    /// refreshes an attestation that has expired or corrects one they got
    /// wrong.
    ///
    /// The fee transfer and the stored attestation are in the same
    /// transaction, so there is no charge without a record and no record
    /// without a charge.
    pub fn attest(
        env: Env,
        invoice_id: Symbol,
        verifier: Address,
        v_type: VerificationType,
        hash: BytesN<32>,
        approved: bool,
    ) -> Attestation {
        assert_not_paused(&env);
        verifier.require_auth();
        if !Self::is_verifier(env.clone(), verifier.clone()) {
            env.panic_with_error(ContractError::Unauthorized);
        }

        let invoice = load_invoices(&env)
            .get(invoice_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if invoice.status != InvoiceStatus::Pending && invoice.status != InvoiceStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        let now = env.ledger().timestamp();
        let threshold = Self::get_verifier_threshold(env.clone());
        let attestations = load_verifications(&env, &invoice_id);
        let status_before = type_status(&env, &attestations, v_type, threshold, now);

        // Charge before writing, so a verifier the originator cannot pay never
        // gets an attestation recorded. CEI: the token is a standard SEP-41
        // contract with no reentrant hooks.
        let fee = Self::fee_for(&env, invoice.amount);
        if fee > 0 {
            let token_addr = invofi_common::get_currency_token(&env, &invoice.currency)
                .unwrap_or_else(|| panic!("Verification fee token not configured for currency"));
            token::TokenClient::new(&env, &token_addr).transfer_from(
                &env.current_contract_address(),
                &invoice.originator,
                &verifier,
                &fee,
            );
        }

        let attestation = Attestation {
            verifier: verifier.clone(),
            v_type,
            hash: hash.clone(),
            timestamp: now,
            valid_until: now.saturating_add(Self::get_attestation_validity(env.clone())),
            status: if approved {
                VerificationStatus::Verified
            } else {
                VerificationStatus::Rejected
            },
        };

        // One attestation per (verifier, type): drop this verifier's previous
        // statement about this fact before appending the new one.
        let mut updated: Vec<Attestation> = Vec::new(&env);
        for existing in attestations.iter() {
            if existing.verifier == verifier && existing.v_type == v_type {
                continue;
            }
            updated.push_back(existing);
        }
        if updated.len() >= MAX_ATTESTATIONS_PER_INVOICE {
            updated = evict_one_slot(&env, &updated, now);
        }
        updated.push_back(attestation.clone());
        save_verifications(&env, &invoice_id, &updated);

        env.events().publish(
            (symbol_short!("ver_sub"), invoice_id.clone()),
            (verifier, v_type, hash, attestation.valid_until, fee),
        );

        let status_after = type_status(&env, &updated, v_type, threshold, now);
        if status_after != status_before
            && (status_after == VerificationStatus::Verified
                || status_after == VerificationStatus::Rejected)
        {
            env.events().publish(
                (symbol_short!("ver_done"), invoice_id),
                (v_type, status_after),
            );
        }

        attestation
    }

    /// Every attestation recorded against an invoice, oldest first.
    pub fn get_verifications(env: Env, invoice_id: Symbol) -> Vec<Attestation> {
        load_verifications(&env, &invoice_id)
    }

    /// Status of one verification type on an invoice. Expiry is derived, so
    /// this is correct whether or not `expire_verifications` has been called.
    pub fn get_verification_status(
        env: Env,
        invoice_id: Symbol,
        v_type: VerificationType,
    ) -> VerificationStatus {
        let attestations = load_verifications(&env, &invoice_id);
        let threshold = Self::get_verifier_threshold(env.clone());
        type_status(
            &env,
            &attestations,
            v_type,
            threshold,
            env.ledger().timestamp(),
        )
    }

    /// Verification status of the invoice as a whole.
    ///
    /// `Verified` requires **every** verification type to be verified — an
    /// invoice with a checked document hash but no business registration is
    /// not a verified invoice. A rejection anywhere makes the whole thing
    /// `Rejected`; otherwise a lapsed type makes it `Expired`.
    pub fn get_invoice_verification_status(env: Env, invoice_id: Symbol) -> VerificationStatus {
        let attestations = load_verifications(&env, &invoice_id);
        let threshold = Self::get_verifier_threshold(env.clone());
        let now = env.ledger().timestamp();

        let mut all_verified = true;
        let mut any_expired = false;
        for v_type in VERIFICATION_TYPES.iter() {
            match type_status(&env, &attestations, *v_type, threshold, now) {
                VerificationStatus::Rejected => return VerificationStatus::Rejected,
                VerificationStatus::Verified => {}
                VerificationStatus::Expired => {
                    all_verified = false;
                    any_expired = true;
                }
                VerificationStatus::Pending => all_verified = false,
            }
        }

        if all_verified {
            VerificationStatus::Verified
        } else if any_expired {
            VerificationStatus::Expired
        } else {
            VerificationStatus::Pending
        }
    }

    /// Record the attestations on an invoice that have passed `valid_until`
    /// and emit `ver_exp` for each, returning how many were newly expired.
    ///
    /// Permissionless, and purely a notification mechanism: reads already
    /// derive expiry from `valid_until`, so nothing about an invoice's
    /// verification status depends on this being called. It exists because
    /// Soroban has no scheduler and an indexer cannot observe a deadline
    /// passing — someone has to poke. Calling it twice is a no-op the second
    /// time, so a keeper cannot spam duplicate events.
    pub fn expire_verifications(env: Env, invoice_id: Symbol) -> u32 {
        assert_not_paused(&env);
        let attestations = load_verifications(&env, &invoice_id);
        if attestations.is_empty() {
            env.panic_with_error(ContractError::NotFound);
        }

        let now = env.ledger().timestamp();
        let mut updated: Vec<Attestation> = Vec::new(&env);
        let mut newly_expired: u32 = 0;

        for attestation in attestations.iter() {
            if effective_status(&attestation, now) == VerificationStatus::Expired
                && attestation.status != VerificationStatus::Expired
            {
                newly_expired += 1;
                env.events().publish(
                    (symbol_short!("ver_exp"), invoice_id.clone()),
                    (
                        attestation.verifier.clone(),
                        attestation.v_type,
                        attestation.valid_until,
                    ),
                );
                updated.push_back(Attestation {
                    status: VerificationStatus::Expired,
                    ..attestation
                });
            } else {
                updated.push_back(attestation);
            }
        }

        if newly_expired > 0 {
            save_verifications(&env, &invoice_id, &updated);
        }
        newly_expired
    }

    // ── Metadata ─────────────────────────────────────────────────────────────

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    pub fn get_min_invoice_amount(_env: Env) -> i128 {
        MIN_INVOICE_AMOUNT
    }

    // ── State Machine History ────────────────────────────────────────────────

    /// Query the full transition history for an invoice.
    pub fn get_transition_history(env: Env, invoice_id: Symbol) -> Vec<TransitionRecord> {
        get_transition_history(&env, invoice_id)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
