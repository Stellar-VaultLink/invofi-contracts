#![no_std]

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Symbol};

use invofi_common::{
    assert_not_paused, resolve_token, FinancingClient, FinancingOffer, Invoice, InvoiceStatus,
    OfferStatus, RegistryClient, GRACE_PERIOD_SECS, MAX_OFFER_DURATION_SECS,
    MIN_OFFER_DURATION_SECS,
};

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct RepaymentContract;

#[contractimpl]
impl RepaymentContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// Initialize the repayment contract. Stores the registry, financing,
    /// and token addresses for cross-contract calls. Admin is the same as
    /// the registry/financing admin.
    pub fn initialize(
        env: Env,
        admin: Address,
        registry: Address,
        financing: Address,
        token: Address,
    ) {
        admin.require_auth();
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

    /// Transfers admin rights. Only current admin.
    pub fn transfer_admin(env: Env, admin: Address, new_admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can transfer admin rights");
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
            panic!("Only admin can pause");
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
            panic!("Only admin can unpause");
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
        repayer.require_auth();

        // Cross-contract: read invoice from registry
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        let invoice: Invoice = registry_client.get_invoice(&invoice_id);

        if invoice.originator != repayer {
            panic!("Only the invoice originator can repay");
        }
        if invoice.status != InvoiceStatus::Financed {
            panic!("Invoice must be Financed before repayment");
        }

        // Cross-contract: read offer from financing
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        let mut offer: FinancingOffer = financing_client.get_offer(&offer_id);

        if offer.invoice_id != invoice_id {
            panic!("Offer does not belong to this invoice");
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            panic!("Offer must be Accepted or Financed before repayment");
        }
        assert!(amount > 0, "repayment amount must be greater than zero");

        let token_id = resolve_token(&env, &offer.currency);
        let token_client = token::TokenClient::new(&env, &token_id);
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;
        let remaining_balance = total_due - offer.amount_repaid;
        assert!(
            amount <= remaining_balance,
            "Repayment amount exceeds remaining balance"
        );

        // Protocol fee deduction
        let fee_bps: u32 = financing_client.get_fee_bps();
        let fee_amount = amount * (fee_bps as i128) / 10_000;
        let lender_amount = amount - fee_amount;
        token_client.transfer(&repayer, &offer.lender, &lender_amount);
        if fee_amount > 0 {
            let admin: Address = env
                .storage()
                .instance()
                .get(&symbol_short!("admin"))
                .unwrap_or_else(|| panic!("Not initialized"));
            token_client.transfer(&repayer, &admin, &fee_amount);
        }

        offer.amount_repaid += amount;
        let fully_repaid = offer.amount_repaid >= total_due;
        let new_status = if fully_repaid {
            OfferStatus::Repaid
        } else {
            OfferStatus::Financed
        };

        // Cross-contract: update offer in financing
        financing_client.update_offer_status(&offer_id, &new_status);
        financing_client.update_offer_amount_repaid(&offer_id, &offer.amount_repaid);
        financing_client.update_lender_stats_repaid(&offer.lender, &fully_repaid);
        financing_client.update_stats_repaid(&amount, &fee_amount);

        // Cross-contract: update invoice status in registry
        let updated_invoice =
            registry_client.set_invoice_repaid_status(&invoice_id, &repayer, &fully_repaid);

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

        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
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
        let invoice: Invoice = registry_client.get_invoice(&invoice_id);

        if invoice.status != InvoiceStatus::Overdue {
            panic!("Invoice must be Overdue before reclaim");
        }
        if env.ledger().timestamp() < invoice.due_date + GRACE_PERIOD_SECS {
            panic!("Grace period has not elapsed");
        }

        // Cross-contract: read offer from financing
        let financing_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("financing"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let financing_client = FinancingClient::new(&env, &financing_addr);
        let mut offer: FinancingOffer = financing_client.get_offer(&offer_id);

        if offer.invoice_id != invoice_id {
            panic!("Offer does not belong to this invoice");
        }
        if offer.lender != lender {
            panic!("Only the financing lender can reclaim");
        }
        if offer.status != OfferStatus::Accepted && offer.status != OfferStatus::Financed {
            panic!("Offer must be Accepted or Financed before reclaim");
        }

        // Cross-contract: update offer status in financing
        financing_client.update_offer_status(&offer_id, &OfferStatus::Defaulted);

        offer.status = OfferStatus::Defaulted;

        env.events().publish(
            (symbol_short!("off_def"), offer.id.clone()),
            (offer.invoice_id.clone(), offer.lender.clone()),
        );
        offer
    }

    // ── Query helpers ────────────────────────────────────────────────────────

    /// Calculate the remaining total due on an offer (principal + yield - repaid).
    /// Cross-contract: reads offer from financing.
    pub fn calculate_total_due(env: Env, offer_id: Symbol) -> i128 {
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
        let yield_amount = offer.amount * (offer.interest_rate as i128) / 10_000;
        let total_due = offer.amount + yield_amount;
        (total_due - offer.amount_repaid).max(0)
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    pub fn get_duration_limits(_env: Env) -> (u64, u64) {
        (MIN_OFFER_DURATION_SECS, MAX_OFFER_DURATION_SECS)
    }
}

#[cfg(test)]
mod test;
