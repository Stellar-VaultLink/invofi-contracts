#![no_std]

use soroban_sdk::{
    contract, contractimpl, symbol_short,
    xdr::ToXdr,
    Address, Env, Map, Symbol, Vec,
};

use invofi_common::{
    assert_not_paused, Invoice, InvoiceStatus, ProtocolStats, RiskTier, StorageEvictionReason,
    DEFAULT_INVOICE_STORAGE_BUDGET_BYTES, EVICTION_GRACE_PERIOD_SECS, MIN_INVOICE_AMOUNT,
    TERMINAL_INVOICE_RETENTION_SECS,
};

// ─── Storage Helpers ─────────────────────────────────────────────────────────

fn invoice_key(id: &Symbol) -> (Symbol, Symbol) {
    (symbol_short!("inv"), id.clone())
}

fn terminal_at_key(id: &Symbol) -> (Symbol, Symbol) {
    (symbol_short!("term_at"), id.clone())
}

fn load_invoice_ids(env: &Env) -> Vec<Symbol> {
    env.storage()
        .persistent()
        .get(&symbol_short!("inv_ids"))
        .unwrap_or(Vec::new(env))
}

fn save_invoice_ids(env: &Env, ids: &Vec<Symbol>) {
    env.storage().persistent().set(&symbol_short!("inv_ids"), ids);
}

fn load_invoice(env: &Env, id: &Symbol) -> Option<Invoice> {
    env.storage().persistent().get(&invoice_key(id))
}

fn invoice_storage_bytes(env: &Env, invoice: &Invoice) -> u32 {
    // SDK 22 exposes no Storage::bytes API. XDR is the SDK's canonical
    // serialization, so this is the deterministic size attributed to the
    // invoice key/value payload (not ledger-entry framing).
    invoice_key(&invoice.id)
        .to_xdr(env)
        .len()
        .saturating_add(invoice.clone().to_xdr(env).len())
}

fn invoice_storage_budget(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&symbol_short!("inv_budg"))
        .unwrap_or(DEFAULT_INVOICE_STORAGE_BUDGET_BYTES)
}

fn assert_invoice_within_budget(env: &Env, invoice: &Invoice) {
    if invoice_storage_bytes(env, invoice) > invoice_storage_budget(env) {
        panic!("Invoice storage budget exceeded");
    }
}

fn save_invoice(env: &Env, invoice: &Invoice) {
    assert_invoice_within_budget(env, invoice);
    env.storage().persistent().set(&invoice_key(&invoice.id), invoice);
}

fn is_terminal(status: &InvoiceStatus) -> bool {
    *status == InvoiceStatus::Repaid
        || *status == InvoiceStatus::Defaulted
        || *status == InvoiceStatus::Cancelled
}

fn save_terminal_timestamp(env: &Env, invoice: &Invoice) {
    let key = terminal_at_key(&invoice.id);
    if is_terminal(&invoice.status) {
        env.storage().persistent().set(&key, &env.ledger().timestamp());
        let max_ttl = env.storage().max_ttl();
        env.storage().persistent().extend_ttl(&key, max_ttl, max_ttl);
        env.storage()
            .persistent()
            .extend_ttl(&invoice_key(&invoice.id), max_ttl, max_ttl);
        env.storage()
            .persistent()
            .extend_ttl(&symbol_short!("inv_ids"), max_ttl, max_ttl);
    } else {
        env.storage().persistent().remove(&key);
    }
}

fn load_invoices(env: &Env) -> Map<Symbol, Invoice> {
    let mut invoices = Map::new(env);
    for id in load_invoice_ids(env).iter() {
        if let Some(invoice) = load_invoice(env, &id) {
            invoices.set(id, invoice);
        }
    }
    invoices
}

fn save_invoices(env: &Env, invoices: &Map<Symbol, Invoice>) {
    for (_id, invoice) in invoices.iter() {
        save_invoice(env, &invoice);
    }
}

fn add_invoice_id(env: &Env, id: &Symbol) {
    let mut ids = load_invoice_ids(env);
    ids.push_back(id.clone());
    save_invoice_ids(env, &ids);
    let max_ttl = env.storage().max_ttl();
    env.storage()
        .persistent()
        .extend_ttl(&symbol_short!("inv_ids"), max_ttl, max_ttl);
}

fn remove_invoice_id(env: &Env, id: &Symbol) {
    let mut retained = Vec::new(env);
    for existing_id in load_invoice_ids(env).iter() {
        if existing_id != *id {
            retained.push_back(existing_id);
        }
    }
    save_invoice_ids(env, &retained);
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

fn assert_keeper(env: &Env, caller: &Address) {
    caller.require_auth();
    let keeper: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("strgkeep"))
        .unwrap_or_else(|| panic!("Storage keeper not configured"));
    if keeper != *caller {
        panic!("Only the storage keeper can perform this action");
    }
}

fn assert_evictable(env: &Env, invoice: &Invoice) {
    if !is_invoice_eviction_eligible(env, invoice) {
        if !is_terminal(&invoice.status) {
            panic!("Only terminal invoices can be evicted");
        }
        panic!("Invoice retention and eviction grace periods have not elapsed");
    }
}

fn is_invoice_eviction_eligible(env: &Env, invoice: &Invoice) -> bool {
    if !is_terminal(&invoice.status) {
        return false;
    }
    let Some(terminal_at) = env
        .storage()
        .persistent()
        .get::<_, u64>(&terminal_at_key(&invoice.id))
    else {
        return false;
    };
    let Some(eligible_at) = terminal_at
        .checked_add(TERMINAL_INVOICE_RETENTION_SECS)
        .and_then(|timestamp| timestamp.checked_add(EVICTION_GRACE_PERIOD_SECS))
    else {
        return false;
    };
    env.ledger().timestamp() >= eligible_at
}

fn evict_invoice(env: &Env, id: &Symbol, reason: StorageEvictionReason) -> u32 {
    let invoice = load_invoice(env, id).unwrap_or_else(|| panic!("Invoice not found"));
    assert_evictable(env, &invoice);
    let reclaimed_bytes = invoice_storage_bytes(env, &invoice);
    env.storage().persistent().remove(&invoice_key(id));
    env.storage().persistent().remove(&terminal_at_key(id));
    remove_invoice_id(env, id);
    env.events().publish(
        (Symbol::new(env, "storage_evicted"), id.clone()),
        (reason, reclaimed_bytes),
    );
    reclaimed_bytes
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
        add_invoice_id(&env, &invoice.id);

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
        load_invoice(&env, &id)
            .unwrap_or_else(|| panic!("Invoice not found"))
    }

    /// Manually update the status of a Pending invoice. Only the invoice
    /// originator can call this.
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
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = new_status.clone();
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
        env.events()
            .publish((symbol_short!("inv_sts"), invoice.id.clone()), new_status);
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
        save_terminal_timestamp(&env, &invoice);
        env.events().publish(
            (symbol_short!("inv_amt"), invoice.id.clone()),
            new_amount,
        );
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
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = InvoiceStatus::Cancelled;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
        env.events().publish(
            (symbol_short!("inv_cxl"), invoice.id.clone()),
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
        if invoice.status != InvoiceStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = if fully_repaid {
            InvoiceStatus::Repaid
        } else {
            InvoiceStatus::Financed
        };
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
        env.events()
            .publish((symbol_short!("inv_sts"), invoice.id.clone()), invoice.status.clone());
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
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = InvoiceStatus::Financed;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
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
        if invoice.status != InvoiceStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = if fully_repaid {
            InvoiceStatus::Repaid
        } else {
            InvoiceStatus::Financed
        };
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
        env.events()
            .publish((symbol_short!("inv_sts"), invoice.id.clone()), invoice.status.clone());
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
        if invoice.status != InvoiceStatus::Overdue {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = InvoiceStatus::Defaulted;
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
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
        if invoice.status != InvoiceStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        if env.ledger().timestamp() <= invoice.due_date {
            env.panic_with_error(ContractError::InvalidTransition);
        }
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
        if invoice.status != InvoiceStatus::Financed {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        invoice.status = InvoiceStatus::Disputed;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
        env.events().publish(
            (symbol_short!("inv_dsp"), invoice.id.clone()),
            invoice.originator.clone(),
        );
        invoice
    }

    /// Resolve a Disputed invoice. Admin only.
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
        if invoice.status != InvoiceStatus::Disputed {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        if target_status == InvoiceStatus::Disputed {
            env.panic_with_error(ContractError::InvalidInput);
        }
        invoice.status = target_status;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        save_terminal_timestamp(&env, &invoice);
        env.events().publish(
            (symbol_short!("inv_rsl"), invoice.id.clone()),
            invoice.status.clone(),
        );
        invoice
    }

    // ── Query helpers ────────────────────────────────────────────────────────

    /// Configure the account authorized to run storage maintenance. Panics
    /// unless `admin` is the current admin.
    pub fn set_storage_keeper(env: Env, admin: Address, keeper: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("strgkeep"), &keeper);
    }

    /// Return the configured storage keeper, if one has been configured.
    pub fn get_storage_keeper(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("strgkeep"))
    }

    /// Configure the maximum XDR key/value payload size permitted for an
    /// invoice. Panics unless `admin` is current admin or `bytes` is nonzero.
    pub fn set_invoice_storage_budget(env: Env, admin: Address, bytes: u32) {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        if bytes == 0 {
            panic!("Invoice storage budget must be greater than zero");
        }
        env.storage().instance().set(&symbol_short!("inv_budg"), &bytes);
    }

    /// Return the invoice storage budget. Defaults to 10 KiB.
    pub fn get_invoice_storage_budget(env: Env) -> u32 {
        invoice_storage_budget(&env)
    }

    /// Return the deterministic XDR key/value payload size attributed to an
    /// invoice. SDK 22 does not expose host storage-byte accounting.
    pub fn get_invoice_storage_bytes(env: Env, id: Symbol) -> u32 {
        let invoice = load_invoice(&env, &id).unwrap_or_else(|| panic!("Invoice not found"));
        invoice_storage_bytes(&env, &invoice)
    }

    /// Return whether a terminal invoice has passed both retention windows.
    /// Missing invoices and non-terminal invoices return `false`.
    pub fn is_invoice_eviction_eligible(env: Env, id: Symbol) -> bool {
        let Some(invoice) = load_invoice(&env, &id) else {
            return false;
        };
        is_invoice_eviction_eligible(&env, &invoice)
    }

    /// Extend an active invoice's persistent-entry TTL to the network maximum.
    /// Panics unless `keeper` is configured and authorized, or the invoice is
    /// terminal. The ID index is bumped at the same time so keeper queries
    /// remain available.
    pub fn bump_invoice_ttl(env: Env, keeper: Address, id: Symbol) {
        assert_not_paused(&env);
        assert_keeper(&env, &keeper);
        let invoice = load_invoice(&env, &id).unwrap_or_else(|| panic!("Invoice not found"));
        if is_terminal(&invoice.status) {
            panic!("Terminal invoices cannot have their active TTL bumped");
        }
        let max_ttl = env.storage().max_ttl();
        env.storage()
            .persistent()
            .extend_ttl(&invoice_key(&id), max_ttl, max_ttl);
        env.storage()
            .persistent()
            .extend_ttl(&symbol_short!("inv_ids"), max_ttl, max_ttl);
        env.events().publish(
            (Symbol::new(&env, "ttl_bumped"), id),
            (keeper, max_ttl),
        );
    }

    /// Evict an eligible terminal invoice during keeper automation. Panics
    /// unless `keeper` is authorized and the one-year retention plus 30-day
    /// notice period have elapsed. Returns XDR payload bytes reclaimed.
    pub fn keeper_evict_invoice(env: Env, keeper: Address, id: Symbol) -> u32 {
        assert_not_paused(&env);
        assert_keeper(&env, &keeper);
        evict_invoice(&env, &id, StorageEvictionReason::RetentionExpired)
    }

    /// Evict an eligible terminal invoice at the admin's direction. This does
    /// not bypass the retention or notice period. Returns XDR payload bytes
    /// reclaimed and emits `storage_evicted` with reason `Admin`.
    pub fn evict_invoice(env: Env, admin: Address, id: Symbol) -> u32 {
        assert_not_paused(&env);
        assert_admin(&env, &admin);
        evict_invoice(&env, &id, StorageEvictionReason::Admin)
    }

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

    // ── Metadata ─────────────────────────────────────────────────────────────

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    pub fn get_min_invoice_amount(_env: Env) -> i128 {
        MIN_INVOICE_AMOUNT
    }
}

#[cfg(test)]
mod test;
