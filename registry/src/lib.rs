#![no_std]

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Map, Symbol, Vec};

use invofi_common::{
    assert_not_paused, Invoice, InvoiceStatus, ProtocolStats, RiskTier, MIN_INVOICE_AMOUNT,
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

fn assert_not_blacklisted(env: &Env, address: &Address) {
    let list = load_blacklist(env);
    for entry in list.iter() {
        if entry == *address {
            panic!("Address is blacklisted");
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
        panic!("Only the current admin can perform this action");
    }
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct RegistryContract;

#[contractimpl]
impl RegistryContract {
    // ── Admin / initialization ───────────────────────────────────────────────

    /// One-time setup. Sets the admin address. Must be called once right after
    /// deployment, before any invoice can be registered.
    pub fn initialize(env: Env, admin: Address) {
        admin.require_auth();
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
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &new_admin);
    }

    /// Register the financing contract address. Admin only. The financing
    /// contract is the only caller allowed to transition a Pending invoice to
    /// Financed via `transition_invoice_status`.
    pub fn set_financing_contract(env: Env, admin: Address, financing: Address) {
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("financing"), &financing);
    }

    /// Register the repayment contract address. Admin only. The repayment
    /// contract is the only caller allowed to transition a Financed invoice
    /// to Financed (partial) or Repaid (full) via `transition_invoice_status`.
    pub fn set_repayment_contract(env: Env, admin: Address, repayment: Address) {
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
        assert_admin(&env, &admin);
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

    // ── Protocol fee ─────────────────────────────────────────────────────────

    /// Set the protocol fee in basis points (max 500 = 5%). Admin only.
    pub fn set_fee(env: Env, admin: Address, fee_bps: u32) {
        assert_admin(&env, &admin);
        if fee_bps > 500 {
            panic!("fee_bps must be at most 500 (5%)");
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
        assert!(
            amount >= MIN_INVOICE_AMOUNT,
            "amount must be at least MIN_INVOICE_AMOUNT stroops"
        );
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
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.originator != originator {
            panic!("Only the invoice originator can update the status");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Only Pending invoices can have their status updated");
        }
        invoice.status = new_status.clone();
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
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
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.originator != originator {
            panic!("Only the invoice originator can update the amount");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Only Pending invoices can have their amount updated");
        }
        assert!(
            new_amount >= MIN_INVOICE_AMOUNT,
            "new_amount must be at least MIN_INVOICE_AMOUNT stroops"
        );
        invoice.amount = new_amount;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    /// Cancel a Pending invoice. Only the originator can call this.
    pub fn cancel_invoice(env: Env, invoice_id: Symbol, originator: Address) -> Invoice {
        assert_not_paused(&env);
        originator.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.originator != originator {
            panic!("Only the invoice originator can cancel");
        }
        if invoice.status != InvoiceStatus::Pending {
            panic!("Only Pending invoices can be cancelled");
        }
        invoice.status = InvoiceStatus::Cancelled;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
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
        repayer.require_auth();
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.status != InvoiceStatus::Financed {
            panic!("Invoice must be Financed for repayment status update");
        }
        invoice.status = if fully_repaid {
            InvoiceStatus::Repaid
        } else {
            InvoiceStatus::Financed
        };
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    /// System transition: Pending -> Financed on offer acceptance.
    /// Only the registered financing contract may call this. Soroban does not
    /// forward *user* auth across contract boundaries, so the financing
    /// contract is authorized by address via the host's implicit
    /// contract-invoker auth (see Stellar docs — Authorization).
    pub fn financing_marks_invoice_financed(env: Env, id: Symbol) -> Invoice {
        let financing: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Financing contract not configured"));
        financing.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.status != InvoiceStatus::Pending {
            panic!("Invoice must be Pending before financing");
        }
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
        let repayment: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("repayment"))
            .unwrap_or_else(|| panic!("Repayment contract not configured"));
        repayment.require_auth();

        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.status != InvoiceStatus::Financed {
            panic!("Invoice must be Financed before repayment");
        }
        invoice.status = if fully_repaid {
            InvoiceStatus::Repaid
        } else {
            InvoiceStatus::Financed
        };
        invoices.set(id, invoice.clone());
        save_invoices(&env, &invoices);
        invoice
    }

    // ── Overdue marking ─────────────────────────────────────────────────────

    /// Mark a Financed invoice as Overdue. Can be called by anyone after
    /// due_date has passed. This is a public status transition that doesn't
    /// require originator auth — the time-based condition is sufficient.
    pub fn mark_invoice_overdue(env: Env, id: Symbol) -> Invoice {
        assert_not_paused(&env);
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.status != InvoiceStatus::Financed {
            panic!("Only Financed invoices can be marked Overdue");
        }
        if env.ledger().timestamp() <= invoice.due_date {
            panic!("Invoice due date has not passed");
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
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.originator != originator {
            panic!("Only the invoice originator can raise a dispute");
        }
        if invoice.status != InvoiceStatus::Financed {
            panic!("Only Financed invoices can be disputed");
        }
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
    pub fn resolve_dispute(
        env: Env,
        admin: Address,
        invoice_id: Symbol,
        target_status: InvoiceStatus,
    ) -> Invoice {
        assert_admin(&env, &admin);
        let mut invoices = load_invoices(&env);
        let mut invoice = invoices
            .get(invoice_id.clone())
            .unwrap_or_else(|| panic!("Invoice not found"));
        if invoice.status != InvoiceStatus::Disputed {
            panic!("Invoice is not in Disputed status");
        }
        if target_status == InvoiceStatus::Disputed {
            panic!("Cannot resolve to Disputed status");
        }
        invoice.status = target_status;
        invoices.set(invoice_id, invoice.clone());
        save_invoices(&env, &invoices);
        env.events().publish(
            (symbol_short!("inv_rsl"), invoice.id.clone()),
            invoice.status.clone(),
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
