#![no_std]
// create_offer takes 8 args — that's the contract's public ABI, and the lint
// also fires inside the #[contractimpl]-generated client where we can't allow it.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Map, Symbol, Vec};

use invofi_common::{
    assert_not_paused, resolve_token, ContractError, FinancingOffer, Invoice, InvoiceStatus,
    LenderStats, NegotiationParty, NegotiationRecord, NegotiationStatus, OfferStatus,
    ProtocolStats, RegistryClient, RepaymentSchedule, ScheduleFrequency,
    DEFAULT_NEGOTIATION_WINDOW_SECS, MAX_NEGOTIATION_ROUNDS, MAX_NEGOTIATION_WINDOW_SECS,
    MAX_OFFER_DURATION_SECS, MIN_NEGOTIATION_WINDOW_SECS, MIN_OFFER_DURATION_SECS,
};

// ─── Storage Helpers ─────────────────────────────────────────────────────────

fn load_offers(env: &Env) -> Map<Symbol, FinancingOffer> {
    env.storage()
        .persistent()
        .get(&symbol_short!("offers"))
        .unwrap_or(Map::new(env))
}

fn save_offers(env: &Env, map: &Map<Symbol, FinancingOffer>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("offers"), map);
}

fn load_lender_stats(env: &Env, lender: &Address) -> LenderStats {
    env.storage()
        .persistent()
        .get(&(symbol_short!("lstats"), lender.clone()))
        .unwrap_or_default()
}

fn save_lender_stats(env: &Env, lender: &Address, stats: &LenderStats) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("lstats"), lender.clone()), stats);
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

// ─── Schedule Storage Helpers ─────────────────────────────────────────────────

fn load_schedules(env: &Env) -> Map<Symbol, RepaymentSchedule> {
    env.storage()
        .persistent()
        .get(&symbol_short!("scheds"))
        .unwrap_or(Map::new(env))
}

fn save_schedules(env: &Env, map: &Map<Symbol, RepaymentSchedule>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("scheds"), map);
}

// ─── Negotiation Storage Helpers (issue #180) ────────────────────────────────
//
// Negotiation state is keyed per offer rather than held in one map, so a busy
// offer never makes every other offer's negotiation more expensive to read.
//
//   ("negot", offer_id)  -> Vec<NegotiationRecord>  the round-by-round history
//   ("negdl", offer_id)  -> u64                     window deadline, frozen at open
//   ("negend", offer_id) -> NegotiationStatus       persisted terminal outcome

fn load_negotiation(env: &Env, offer_id: &Symbol) -> Vec<NegotiationRecord> {
    env.storage()
        .persistent()
        .get(&(symbol_short!("negot"), offer_id.clone()))
        .unwrap_or(Vec::new(env))
}

fn save_negotiation(env: &Env, offer_id: &Symbol, history: &Vec<NegotiationRecord>) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("negot"), offer_id.clone()), history);
}

fn load_deadline(env: &Env, offer_id: &Symbol) -> u64 {
    env.storage()
        .persistent()
        .get(&(symbol_short!("negdl"), offer_id.clone()))
        .unwrap_or(0)
}

fn save_deadline(env: &Env, offer_id: &Symbol, deadline: u64) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("negdl"), offer_id.clone()), &deadline);
}

fn load_outcome(env: &Env, offer_id: &Symbol) -> Option<NegotiationStatus> {
    env.storage()
        .persistent()
        .get(&(symbol_short!("negend"), offer_id.clone()))
}

fn save_outcome(env: &Env, offer_id: &Symbol, outcome: NegotiationStatus) {
    env.storage()
        .persistent()
        .set(&(symbol_short!("negend"), offer_id.clone()), &outcome);
}

/// The most recent round proposed by `party`, if that side has proposed at all.
///
/// "Most recent" is what makes a counter-offer revocable-by-replacement: only
/// a party's latest proposal is live, so an earlier one can never be executed
/// against them after they have moved off it.
fn last_proposal_by(
    history: &Vec<NegotiationRecord>,
    party: NegotiationParty,
) -> Option<NegotiationRecord> {
    let mut latest: Option<NegotiationRecord> = None;
    for record in history.iter() {
        if record.party == party {
            latest = Some(record);
        }
    }
    latest
}

/// Validate a proposed term tuple against the same bounds `create_offer`
/// enforces. A negotiation must not be a way to reach terms the offer could
/// not have been created with in the first place.
fn assert_terms_valid(env: &Env, amount: i128, interest_rate: u32, duration: u64) {
    let rate_ok = (1..=10_000).contains(&interest_rate);
    let duration_ok = (MIN_OFFER_DURATION_SECS..=MAX_OFFER_DURATION_SECS).contains(&duration);
    if amount <= 0 || !rate_ok || !duration_ok {
        env.panic_with_error(ContractError::InvalidInput);
    }
}

fn assert_not_blacklisted(env: &Env, address: &Address) {
    // Cross-contract: check blacklist via the registry contract
    let registry_addr: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("registry"))
        .unwrap_or_else(|| panic!("Not initialized"));
    let registry_client = RegistryClient::new(env, &registry_addr);
    // CEI: Read-only cross-contract call, safe before state mutations.
    if registry_client.is_blacklisted(address) {
        env.panic_with_error(ContractError::Blacklisted);
    }
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct FinancingContract;

#[contractimpl]
impl FinancingContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// One-time setup. Stores the admin, the registry address (for
    /// cross-contract calls) and the default settlement token.
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run (issue #75) — a
    /// fresh deployment can never be hijacked by a third party setting
    /// themselves as admin.
    pub fn __constructor(env: Env, admin: Address, registry: Address, token: Address) {
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
            .set(&symbol_short!("token"), &token);
    }

    /// Register the repayment contract address. Only admin.
    /// The repayment contract is the only caller authorized to invoke
    /// callback methods (update_offer_status, update_offer_amount_repaid, etc.).
    pub fn set_repayment_contract(env: Env, admin: Address, repayment: Address) {
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
            .set(&symbol_short!("repayment"), &repayment);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    pub fn get_registry(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"))
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

    // ── Currency registry ────────────────────────────────────────────────────

    /// Register a currency → token mapping. Admin only.
    pub fn register_currency(env: Env, admin: Address, currency: Symbol, token_addr: Address) {
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
        invofi_common::register_currency(&env, &currency, &token_addr);
    }

    pub fn get_currency_token(env: Env, currency: Symbol) -> Option<Address> {
        invofi_common::get_currency_token(&env, &currency)
    }

    // ── Position tokens (Task 7) ─────────────────────────────────────────────

    /// Configure the SEP-41 position-token contract that represents lenders'
    /// claims on financed invoices. Admin only.
    ///
    /// The token contract MUST be initialized with this financing contract as
    /// its admin, so accept_offer can mint claim tokens on the lender's
    /// behalf (the token's admin.require_auth() resolves via implicit
    /// contract-invoker auth — see ADR-0002).
    pub fn set_position_token(env: Env, admin: Address, token: Address) {
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
            .set(&symbol_short!("postok"), &token);
    }

    /// Returns the configured position-token contract, if any.
    pub fn get_position_token(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("postok"))
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

    // ── Offer CRUD ───────────────────────────────────────────────────────────

    /// Create a financing offer on an invoice. Only the lender can call this.
    pub fn create_offer(
        env: Env,
        offer_id: Symbol,
        invoice_id: Symbol,
        lender: Address,
        amount: i128,
        currency: Symbol,
        interest_rate: u32,
        duration: u64,
    ) -> FinancingOffer {
        assert_not_paused(&env);
        lender.require_auth();
        assert_not_blacklisted(&env, &lender);
        assert!(amount > 0, "offer amount must be greater than zero");
        assert!(interest_rate > 0, "interest_rate must be greater than zero");
        assert!(
            interest_rate <= 10_000,
            "interest_rate must be at most 10000 bps"
        );
        assert!(
            duration >= MIN_OFFER_DURATION_SECS,
            "duration must be at least 1 day (86400 seconds)"
        );
        assert!(
            duration <= MAX_OFFER_DURATION_SECS,
            "duration must be at most 365 days"
        );

        // Cross-contract call: verify invoice exists, is Pending, and lender != originator
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&invoice_id);
        assert!(
            invoice.status == InvoiceStatus::Pending,
            "Invoice must be Pending to accept offers"
        );
        assert!(
            lender != invoice.originator,
            "lender cannot finance their own invoice"
        );

        let mut offers = load_offers(&env);
        if offers.contains_key(offer_id.clone()) {
            env.panic_with_error(ContractError::AlreadyExists);
        }

        let offer = FinancingOffer {
            id: offer_id.clone(),
            invoice_id,
            lender,
            amount,
            currency,
            interest_rate,
            duration,
            status: OfferStatus::Pending,
            funded_at: 0,
            amount_repaid: 0,
        };
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        let mut s = load_stats(&env);
        s.total_offers += 1;
        save_stats(&env, &s);

        let mut lstats = load_lender_stats(&env, &offer.lender);
        lstats.total_offered += offer.amount;
        lstats.offers_pending += 1;
        save_lender_stats(&env, &offer.lender, &lstats);

        env.events().publish(
            (symbol_short!("off_new"), offer.id.clone()),
            (
                offer.invoice_id.clone(),
                offer.lender.clone(),
                amount,
                interest_rate,
            ),
        );
        offer
    }

    /// Get a financing offer by ID.
    pub fn get_offer(env: Env, id: Symbol) -> FinancingOffer {
        load_offers(&env)
            .get(id)
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound))
    }

    /// Withdraw a pending offer. Only the lender.
    pub fn withdraw_offer(env: Env, offer_id: Symbol, lender: Address) -> FinancingOffer {
        assert_not_paused(&env);
        lender.require_auth();
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.lender != lender {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        offer.status = OfferStatus::Rejected;
        offers.set(offer_id.clone(), offer.clone());
        save_offers(&env, &offers);
        Self::close_negotiation_on_offer_exit(&env, &offer_id, &lender);
        env.events().publish(
            (symbol_short!("off_wdr"), offer.id.clone()),
            offer.lender.clone(),
        );
        offer
    }

    // ── Accept / Reject ──────────────────────────────────────────────────────

    /// Accept a financing offer. Only the invoice originator.
    /// Cross-contract: reads + updates invoice status in the registry contract.
    pub fn accept_offer(env: Env, offer_id: Symbol, invoice_originator: Address) -> FinancingOffer {
        assert_not_paused(&env);
        invoice_originator.require_auth();

        let offers = load_offers(&env);
        let offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: read invoice from registry
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);

        if invoice.originator != invoice_originator {
            env.panic_with_error(ContractError::Unauthorized);
        }
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        Self::settle_acceptance(
            &env,
            offer_id,
            offer,
            &registry_client,
            &invoice,
            &invoice_originator,
        )
    }

    /// Move the money and flip every piece of state that an accepted offer
    /// implies: principal to the business, invoice to Financed, position token
    /// minted, stats updated, `off_acc` emitted.
    ///
    /// Split out of `accept_offer` so the auto-accept path in `amend_offer` /
    /// `counter_offer` executes the *same* settlement rather than a parallel
    /// copy that could drift from it. Every caller is responsible for its own
    /// authorization and for checking that the offer is Pending and the
    /// invoice is Pending before calling in.
    fn settle_acceptance(
        env: &Env,
        offer_id: Symbol,
        offer: FinancingOffer,
        registry_client: &RegistryClient,
        invoice: &Invoice,
        closer: &Address,
    ) -> FinancingOffer {
        let env = env.clone();
        let mut offers = load_offers(&env);
        let mut offer = offer;

        // Settlement ends any negotiation that was still running, whichever
        // route reached it: term convergence in amend/counter, or the
        // originator accepting the standing offer outright. Recording it here
        // rather than in each caller is what keeps the two routes from
        // diverging on the negotiation's final state.
        if !load_negotiation(&env, &offer_id).is_empty() && load_outcome(&env, &offer_id).is_none()
        {
            save_outcome(&env, &offer_id, NegotiationStatus::Accepted);
            env.events().publish(
                (symbol_short!("neg_clsd"), offer_id.clone()),
                (NegotiationStatus::Accepted, closer.clone()),
            );
        }

        // Update state before external call (CEI pattern)
        offer.status = OfferStatus::Accepted;
        offer.funded_at = env.ledger().timestamp();
        offers.set(offer_id, offer.clone());
        save_offers(&env, &offers);

        // Pull the lender's principal and pay it straight to the business.
        let token_id = resolve_token(&env, &offer.currency);
        let token_client = token::TokenClient::new(&env, &token_id);
        // CEI: External interaction after state writes. Safe from reentrancy.
        token_client.transfer_from(
            &env.current_contract_address(),
            &offer.lender,
            &invoice.originator,
            &offer.amount,
        );

        // Cross-contract: mark the invoice Financed in the registry via the
        // system transition (the financing contract is the authorized caller;
        // user auth does not propagate across contract boundaries in Soroban).
        // CEI: External interaction before local state writes (stats). Safe because registry is a trusted protocol contract.
        registry_client.financing_marks_invoice_financed(&offer.invoice_id);

        // Mint the lender's position token representing their claim on this
        // financed invoice (1:1 with the offer amount — see ADR-0002). The
        // token's admin is this financing contract, so the mint resolves via
        // implicit contract-invoker auth. If no position token is configured
        // (legacy deployments), financing still works unchanged.
        if let Some(pos_token) = env.storage().instance().get(&symbol_short!("postok")) {
            // SDK 22's StellarAssetInterface exposes mint(to, amount); the
            // token contract authorizes its admin (this financing contract)
            // internally via require_auth, which resolves through implicit
            // contract-invoker auth when we call it cross-contract.
            let pos_client = token::StellarAssetClient::new(&env, &pos_token);
            // CEI: External interaction before local state writes. Safe because pos_token is a trusted admin-configured token.
            pos_client.mint(&offer.lender, &offer.amount);
            env.events().publish(
                (symbol_short!("pos_mint"), offer.id.clone()),
                (offer.lender.clone(), offer.amount),
            );
        }

        let mut s = load_stats(&env);
        s.total_financed += offer.amount;
        save_stats(&env, &s);

        let mut lstats = load_lender_stats(&env, &offer.lender);
        lstats.total_accepted += offer.amount;
        save_lender_stats(&env, &offer.lender, &lstats);

        env.events().publish(
            (symbol_short!("off_acc"), offer.id.clone()),
            (offer.invoice_id.clone(), offer.lender.clone(), offer.amount),
        );
        offer
    }

    /// Reject a financing offer. Only the invoice originator.
    pub fn reject_offer(env: Env, offer_id: Symbol, invoice_originator: Address) -> FinancingOffer {
        assert_not_paused(&env);
        invoice_originator.require_auth();

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // Cross-contract: verify invoice originator
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);
        if invoice.originator != invoice_originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        offer.status = OfferStatus::Rejected;
        offers.set(offer_id.clone(), offer.clone());
        save_offers(&env, &offers);
        Self::close_negotiation_on_offer_exit(&env, &offer_id, &invoice_originator);

        env.events().publish(
            (symbol_short!("off_rej"), offer.id.clone()),
            offer.invoice_id.clone(),
        );
        offer
    }

    // ── Offer negotiation: amendment & counter-offer (issue #180) ────────────
    //
    // The protocol used to be take-it-or-leave-it: a lender posted terms and
    // the originator could only accept or reject them. These entrypoints let
    // the two sides converge instead, with every round recorded on-chain.
    //
    // Design notes that are easy to get wrong, in full in
    // `docs/adr/0008-offer-negotiation.md`:
    //
    // * **Auto-accept is not unilateral.** Settlement moves the lender's
    //   funds, which Soroban will only do with the lender's authorization.
    //   The lender's commitment is their SEP-41 allowance to this contract,
    //   granted before acceptance is possible at all; the originator's
    //   commitment is the counter-offer they recorded in a transaction they
    //   signed. Auto-accept fires only when the *canonical term tuple* of one
    //   side's live proposal is exactly equal to the other's, so neither party
    //   is ever bound to terms they did not themselves put on the table.
    //
    // * **Every round is version-guarded.** `expected_round` is the history
    //   length the caller believes it is amending. A round that lands after
    //   the other side moved is rejected rather than silently applied to terms
    //   the caller never saw — which is what would otherwise let a stale
    //   counter-offer auto-execute.

    /// Configure the negotiation window, in seconds. Admin only.
    ///
    /// The window bounds how long a recorded counter-offer stays executable
    /// against the party who made it, so it is deliberately clamped to
    /// [`MIN_NEGOTIATION_WINDOW_SECS`, `MAX_NEGOTIATION_WINDOW_SECS`] — an
    /// admin cannot set it to something effectively unbounded. Deployments
    /// that never call this run on the 72-hour default.
    ///
    /// Changing the window does not move the deadline of a negotiation that
    /// is already open: each deadline is frozen when its negotiation opens.
    pub fn set_negotiation_window(env: Env, admin: Address, window_secs: u64) {
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
        if !(MIN_NEGOTIATION_WINDOW_SECS..=MAX_NEGOTIATION_WINDOW_SECS).contains(&window_secs) {
            env.panic_with_error(ContractError::InvalidInput);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("negwin"), &window_secs);
    }

    /// The configured negotiation window in seconds (default: 72 hours).
    pub fn get_negotiation_window(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&symbol_short!("negwin"))
            .unwrap_or(DEFAULT_NEGOTIATION_WINDOW_SECS)
    }

    /// Amend a Pending offer's terms. Only the lender who created the offer.
    ///
    /// The amended terms are written onto the offer itself: the offer is
    /// always the lender's standing position, so an originator who calls
    /// `accept_offer` after an amendment accepts the amended terms, and there
    /// is no second place where "the current terms" could disagree.
    ///
    /// `expected_round` must equal the current number of recorded rounds
    /// (`0` opens the negotiation). If the originator appended a counter-offer
    /// after the lender read state, the amendment is rejected with
    /// `InvalidInput` rather than applied on top of terms the lender never saw.
    ///
    /// **Auto-accept:** if `(amount, interest_rate, duration)` exactly equals
    /// the originator's live counter-offer — their most recent
    /// `NegotiationParty::Originator` round — the sides have agreed and the
    /// offer settles in this same transaction. The returned offer's status is
    /// `Accepted` when that happened and `Pending` when it did not.
    pub fn amend_offer(
        env: Env,
        offer_id: Symbol,
        lender: Address,
        expected_round: u32,
        amount: i128,
        interest_rate: u32,
        duration: u64,
    ) -> FinancingOffer {
        assert_not_paused(&env);
        lender.require_auth();
        assert_not_blacklisted(&env, &lender);
        assert_terms_valid(&env, amount, interest_rate, duration);

        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        if offer.lender != lender {
            env.panic_with_error(ContractError::Unauthorized);
        }

        let history = Self::assert_negotiable(&env, &offer_id, &offer, expected_round);

        // Agreement is exact equality with the originator's *live* proposal.
        // Only their latest round counts: a superseded counter-offer must not
        // remain executable against them.
        let agreed = match last_proposal_by(&history, NegotiationParty::Originator) {
            Some(counter) => {
                counter.amount == amount
                    && counter.interest_rate == interest_rate
                    && counter.duration == duration
            }
            None => false,
        };

        // Keep `total_offered` coherent: it was credited with the original
        // amount at create_offer, so an amendment moves it by the delta rather
        // than double-counting the offer.
        let mut lstats = load_lender_stats(&env, &lender);
        lstats.total_offered += amount - offer.amount;
        save_lender_stats(&env, &lender, &lstats);

        offer.amount = amount;
        offer.interest_rate = interest_rate;
        offer.duration = duration;
        offers.set(offer_id.clone(), offer.clone());
        save_offers(&env, &offers);

        Self::record_round(
            &env,
            &offer_id,
            &history,
            NegotiationParty::Lender,
            amount,
            interest_rate,
            duration,
        );

        env.events().publish(
            (symbol_short!("off_amd"), offer_id.clone()),
            (lender.clone(), amount, interest_rate, duration),
        );

        if agreed {
            return Self::execute_agreement(&env, &offer_id, &lender);
        }
        offer
    }

    /// Propose different terms on a Pending offer. Only the invoice
    /// originator.
    ///
    /// A counter-offer does **not** rewrite the offer — the offer belongs to
    /// the lender. It records the originator's position, which the lender can
    /// then meet by amending to the identical term tuple.
    ///
    /// `expected_round` is the same optimistic-concurrency guard as
    /// `amend_offer`.
    ///
    /// **Auto-accept:** proposing exactly the lender's standing terms *is*
    /// agreement, so the offer settles in this same transaction — the
    /// originator's own `require_auth` covers it, exactly as `accept_offer`
    /// would. The returned offer's status is `Accepted` when that happened.
    pub fn counter_offer(
        env: Env,
        offer_id: Symbol,
        originator: Address,
        expected_round: u32,
        amount: i128,
        interest_rate: u32,
        duration: u64,
    ) -> FinancingOffer {
        assert_not_paused(&env);
        originator.require_auth();
        assert_not_blacklisted(&env, &originator);
        assert_terms_valid(&env, amount, interest_rate, duration);

        let offers = load_offers(&env);
        let offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        // Cross-contract: only the invoice's originator may counter.
        // CEI: Read-only cross-contract call before state mutations.
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);
        if invoice.originator != originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        let history = Self::assert_negotiable(&env, &offer_id, &offer, expected_round);

        // The lender's live proposal is the offer itself.
        let agreed = amount == offer.amount
            && interest_rate == offer.interest_rate
            && duration == offer.duration;

        Self::record_round(
            &env,
            &offer_id,
            &history,
            NegotiationParty::Originator,
            amount,
            interest_rate,
            duration,
        );

        env.events().publish(
            (symbol_short!("ctr_off"), offer_id.clone()),
            (originator.clone(), amount, interest_rate, duration),
        );

        if agreed {
            return Self::execute_agreement(&env, &offer_id, &originator);
        }
        offer
    }

    /// End an open negotiation without agreement.
    ///
    /// Before the deadline this is a withdrawal of consent and only the lender
    /// or the invoice originator may call it — an originator uses it to revoke
    /// a counter-offer they no longer want executed against them.
    ///
    /// After the deadline the negotiation is already `Expired` by derivation;
    /// this call is the poke that persists the outcome and emits
    /// `neg_clsd`, and any authenticated address may make it. Soroban has no
    /// scheduler, so an expiry event cannot fire on its own — that is why the
    /// event is emitted by whoever next touches the negotiation, and why
    /// `get_negotiation_status` never depends on someone having called this.
    pub fn close_negotiation(env: Env, offer_id: Symbol, caller: Address) -> NegotiationStatus {
        assert_not_paused(&env);
        caller.require_auth();

        // A persisted outcome means this negotiation was already closed; a
        // second close would emit a second neg_clsd for the same negotiation.
        if load_outcome(&env, &offer_id).is_some() {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        let outcome = match Self::get_negotiation_status(env.clone(), offer_id.clone()) {
            NegotiationStatus::None => env.panic_with_error(ContractError::NotFound),
            NegotiationStatus::Closed | NegotiationStatus::Accepted => {
                env.panic_with_error(ContractError::InvalidTransition)
            }
            NegotiationStatus::Expired => NegotiationStatus::Expired,
            NegotiationStatus::Open => {
                let offer = load_offers(&env)
                    .get(offer_id.clone())
                    .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
                if caller != offer.lender {
                    let registry_addr: Address = env
                        .storage()
                        .instance()
                        .get(&symbol_short!("registry"))
                        .unwrap_or_else(|| panic!("Not initialized"));
                    let registry_client = RegistryClient::new(&env, &registry_addr);
                    let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);
                    if caller != invoice.originator {
                        env.panic_with_error(ContractError::Unauthorized);
                    }
                }
                NegotiationStatus::Closed
            }
        };

        save_outcome(&env, &offer_id, outcome);
        env.events()
            .publish((symbol_short!("neg_clsd"), offer_id), (outcome, caller));
        outcome
    }

    /// The full on-chain negotiation history for an offer, oldest round first.
    /// Empty when no negotiation was ever opened.
    pub fn get_negotiation(env: Env, offer_id: Symbol) -> Vec<NegotiationRecord> {
        load_negotiation(&env, &offer_id)
    }

    /// Unix timestamp at which the negotiation window closes, or `0` if no
    /// negotiation has been opened on this offer. Frozen when the negotiation
    /// opens, so a later `set_negotiation_window` never moves it.
    pub fn get_negotiation_deadline(env: Env, offer_id: Symbol) -> u64 {
        load_deadline(&env, &offer_id)
    }

    /// Current negotiation status.
    ///
    /// `Expired` is derived from the deadline, not stored, so a caller reading
    /// this always sees the truth whether or not anyone has called
    /// `close_negotiation` yet. An offer that leaves `Pending` by any other
    /// route — withdrawn, rejected, or accepted outright — ends its
    /// negotiation, which reads as `Closed`.
    pub fn get_negotiation_status(env: Env, offer_id: Symbol) -> NegotiationStatus {
        if let Some(outcome) = load_outcome(&env, &offer_id) {
            return outcome;
        }
        if load_negotiation(&env, &offer_id).is_empty() {
            return NegotiationStatus::None;
        }
        match load_offers(&env).get(offer_id.clone()) {
            Some(offer) if offer.status == OfferStatus::Pending => {}
            _ => return NegotiationStatus::Closed,
        }
        if env.ledger().timestamp() > load_deadline(&env, &offer_id) {
            NegotiationStatus::Expired
        } else {
            NegotiationStatus::Open
        }
    }

    // ── Negotiation internals ────────────────────────────────────────────────

    /// Guard shared by `amend_offer` and `counter_offer`: the offer must still
    /// be Pending, the negotiation must be open, the caller must not be
    /// working from a stale read, and the round cap must not be exhausted.
    /// Returns the history the caller's round will be appended to.
    fn assert_negotiable(
        env: &Env,
        offer_id: &Symbol,
        offer: &FinancingOffer,
        expected_round: u32,
    ) -> Vec<NegotiationRecord> {
        if offer.status != OfferStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        match Self::get_negotiation_status(env.clone(), offer_id.clone()) {
            NegotiationStatus::None | NegotiationStatus::Open => {}
            _ => env.panic_with_error(ContractError::InvalidTransition),
        }

        let history = load_negotiation(env, offer_id);
        // Optimistic concurrency: reject a round written against a version of
        // the negotiation that no longer exists.
        if history.len() != expected_round {
            env.panic_with_error(ContractError::InvalidInput);
        }
        if history.len() >= MAX_NEGOTIATION_ROUNDS {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        history
    }

    /// Append a round, opening the negotiation (and freezing its deadline) if
    /// this is the first one.
    fn record_round(
        env: &Env,
        offer_id: &Symbol,
        history: &Vec<NegotiationRecord>,
        party: NegotiationParty,
        amount: i128,
        interest_rate: u32,
        duration: u64,
    ) {
        let now = env.ledger().timestamp();
        if history.is_empty() {
            let window = Self::get_negotiation_window(env.clone());
            save_deadline(env, offer_id, now.saturating_add(window));
        }
        let mut history = history.clone();
        history.push_back(NegotiationRecord {
            party,
            amount,
            interest_rate,
            duration,
            timestamp: now,
        });
        save_negotiation(env, offer_id, &history);
    }

    /// Both sides have put the same term tuple on the table: close the
    /// negotiation as `Accepted` and run the ordinary settlement.
    ///
    /// The invoice is re-read here rather than trusted from the caller's
    /// frame: a negotiation can outlive the invoice's Pending status (it can
    /// be cancelled or disputed mid-negotiation), and settling against a
    /// non-Pending invoice would finance an invoice that is no longer
    /// financeable. Both parties are re-checked against the blacklist for the
    /// same reason — this path settles without the counterparty's live
    /// signature, so their eligibility has to be verified now, not assumed
    /// from when they proposed.
    fn execute_agreement(env: &Env, offer_id: &Symbol, closer: &Address) -> FinancingOffer {
        let offer = load_offers(env)
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);
        if invoice.status != InvoiceStatus::Pending {
            env.panic_with_error(ContractError::InvalidTransition);
        }
        assert_not_blacklisted(env, &offer.lender);
        assert_not_blacklisted(env, &invoice.originator);

        Self::settle_acceptance(
            env,
            offer_id.clone(),
            offer,
            &registry_client,
            &invoice,
            closer,
        )
    }

    /// Emit `neg_clsd` when an offer leaves Pending with a negotiation still
    /// open. No outcome is persisted: `get_negotiation_status` already derives
    /// `Closed` from the offer no longer being Pending, and writing a second
    /// source of truth for the same fact is how the two drift apart.
    fn close_negotiation_on_offer_exit(env: &Env, offer_id: &Symbol, closer: &Address) {
        if load_negotiation(env, offer_id).is_empty() || load_outcome(env, offer_id).is_some() {
            return;
        }
        env.events().publish(
            (symbol_short!("neg_clsd"), offer_id.clone()),
            (NegotiationStatus::Closed, closer.clone()),
        );
    }

    // ── Cross-contract callback methods (called by Repayment) ───────────

    fn assert_only_repayment(env: &Env) {
        let repayment_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("repayment"))
            .unwrap_or_else(|| panic!("Repayment contract not configured"));
        repayment_addr.require_auth();
    }

    /// Update the status of an offer. Called by the Repayment contract
    /// after accept/reject/repay/reclaim to keep offer state in sync.
    pub fn update_offer_status(env: Env, id: Symbol, new_status: OfferStatus) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        offer.status = new_status;
        offers.set(id, offer);
        save_offers(&env, &offers);
    }

    /// Update the running amount_repaid on an offer. Called by Repayment.
    pub fn update_offer_amount_repaid(env: Env, id: Symbol, amount_repaid: i128) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut offers = load_offers(&env);
        let mut offer = offers
            .get(id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));
        offer.amount_repaid = amount_repaid;
        offers.set(id, offer);
        save_offers(&env, &offers);
    }

    /// Update lender stats after a repayment. Called by Repayment.
    pub fn update_lender_stats_repaid(env: Env, lender: Address, fully_repaid: bool) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut lstats = load_lender_stats(&env, &lender);
        if fully_repaid {
            lstats.offers_repaid += 1;
        }
        save_lender_stats(&env, &lender, &lstats);
    }

    /// Update protocol-level stats after a repayment. Called by Repayment.
    pub fn update_stats_repaid(env: Env, amount: i128, fee_amount: i128) {
        assert_not_paused(&env);
        Self::assert_only_repayment(&env);
        let mut s = load_stats(&env);
        s.total_repaid += amount;
        s.total_fee_revenue += fee_amount;
        save_stats(&env, &s);
    }

    /// Read the protocol fee in basis points. Called by Repayment.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&symbol_short!("feebps"))
            .unwrap_or(0)
    }

    // ── Query helpers ────────────────────────────────────────────────────────

    pub fn get_offers_by_invoice(env: Env, invoice_id: Symbol) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.invoice_id == invoice_id {
                result.push_back(offer);
            }
        }
        result
    }

    pub fn get_offers_by_lender(env: Env, lender: Address) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.lender == lender {
                result.push_back(offer);
            }
        }
        result
    }

    /// Return all financing offers. Admin-only analytics function.
    /// At scale, prefer paginated queries — this returns an unbounded Vec.
    pub fn get_all_offers(env: Env) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            result.push_back(offer);
        }
        result
    }

    pub fn get_offers_by_status(env: Env, status: OfferStatus) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.status == status {
                result.push_back(offer);
            }
        }
        result
    }

    pub fn get_pending_offers_by_invoice(env: Env, invoice_id: Symbol) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (_id, offer) in offers.iter() {
            if offer.invoice_id == invoice_id && offer.status == OfferStatus::Pending {
                result.push_back(offer);
            }
        }
        result
    }

    pub fn get_offers_count(env: Env) -> u32 {
        load_offers(&env).len()
    }

    pub fn get_offers_paginated(env: Env, offset: u32, limit: u32) -> Vec<FinancingOffer> {
        let offers = load_offers(&env);
        let mut result: Vec<FinancingOffer> = Vec::new(&env);
        for (idx, (_id, offer)) in offers.iter().enumerate() {
            if idx as u32 >= offset && result.len() < limit {
                result.push_back(offer);
            }
            if result.len() >= limit {
                break;
            }
        }
        result
    }

    pub fn get_lender_stats(env: Env, lender: Address) -> LenderStats {
        load_lender_stats(&env, &lender)
    }

    pub fn get_lender_active_total(env: Env, lender: Address) -> i128 {
        let offers = load_offers(&env);
        let mut total: i128 = 0;
        for (_id, offer) in offers.iter() {
            if offer.lender == lender
                && (offer.status == OfferStatus::Accepted || offer.status == OfferStatus::Financed)
            {
                total += offer.amount;
            }
        }
        total
    }

    pub fn get_stats(env: Env) -> ProtocolStats {
        load_stats(&env)
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    pub fn get_offer_duration_limits(_env: Env) -> (u64, u64) {
        (MIN_OFFER_DURATION_SECS, MAX_OFFER_DURATION_SECS)
    }

    // ── Repayment schedules (issue #133) ─────────────────────────────────────

    /// Attach a fixed installment repayment schedule to a financing offer.
    ///
    /// Can be called by the **lender** on a Pending offer (before acceptance)
    /// or by the **originator** on a Financed/Accepted offer (after acceptance,
    /// to formalise a payment plan). Both parties must provide their auth;
    /// the contract checks that the caller is the lender or the invoice
    /// originator.
    ///
    /// Installment math (flat-rate, equal principal slices):
    ///
    ///   installment_principal = offer.amount / count
    ///   installment_yield     = installment_principal × interest_rate / 10_000
    ///   installment_amount    = installment_principal + installment_yield
    ///
    /// `first_due` must be in the future. `count` must be ≥ 1 and ≤ 1 200
    /// (100 years of daily installments — a hard cap that prevents overflow).
    ///
    /// An existing schedule is overwritten — either party can reschedule while
    /// the offer is still active.
    pub fn schedule_repayment(
        env: Env,
        offer_id: Symbol,
        caller: Address,
        frequency: ScheduleFrequency,
        count: u32,
        first_due: u64,
    ) -> RepaymentSchedule {
        assert_not_paused(&env);
        caller.require_auth();

        assert!(count >= 1, "count must be at least 1");
        assert!(count <= 1_200, "count must be at most 1200 installments");
        assert!(
            first_due > env.ledger().timestamp(),
            "first_due must be in the future"
        );

        let offers = load_offers(&env);
        let offer = offers
            .get(offer_id.clone())
            .unwrap_or_else(|| env.panic_with_error(ContractError::NotFound));

        // Only allow scheduling on live (non-terminal) offers.
        if offer.status == OfferStatus::Rejected
            || offer.status == OfferStatus::Repaid
            || offer.status == OfferStatus::Defaulted
        {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // The caller must be the lender or the invoice originator.
        // Retrieve the originator via the registry cross-contract call.
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Not initialized"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        // CEI: Read-only cross-contract call before state mutations.
        let invoice: Invoice = registry_client.get_invoice(&offer.invoice_id);

        if caller != offer.lender && caller != invoice.originator {
            env.panic_with_error(ContractError::Unauthorized);
        }

        // installment_principal = floor(amount / count)
        // We use floor division; any leftover cent is borne by the last
        // installment but the helper only reports whole installments — that
        // is explicitly in-scope per the issue.
        let installment_principal = offer.amount / (count as i128);
        let installment_yield = installment_principal * (offer.interest_rate as i128) / 10_000;
        let installment_amount = installment_principal + installment_yield;

        assert!(
            installment_amount > 0,
            "installment amount must be greater than zero"
        );

        let schedule = RepaymentSchedule {
            offer_id: offer_id.clone(),
            count,
            frequency,
            installment_amount,
            first_due,
        };

        let mut schedules = load_schedules(&env);
        schedules.set(offer_id, schedule.clone());
        save_schedules(&env, &schedules);

        schedule
    }

    /// Read the repayment schedule attached to an offer, if any.
    pub fn get_schedule(env: Env, offer_id: Symbol) -> Option<RepaymentSchedule> {
        load_schedules(&env).get(offer_id)
    }

    /// Return the 1-based index of the installment that is **currently due**
    /// (its due timestamp ≤ `now`) and whose principal has not yet been
    /// covered by `amount_repaid` on the offer.
    ///
    /// Returns `0` when:
    /// - No schedule exists for this offer, or
    /// - All installments have been paid, or
    /// - No installment is due yet (`now` < `first_due`).
    ///
    /// This is the keeper/CLI-friendly read helper described in issue #133.
    pub fn get_installment_due(env: Env, offer_id: Symbol) -> u32 {
        let schedules = load_schedules(&env);
        let schedule = match schedules.get(offer_id.clone()) {
            Some(s) => s,
            None => return 0,
        };

        let offers = load_offers(&env);
        let offer = match offers.get(offer_id) {
            Some(o) => o,
            None => return 0,
        };

        // Terminal offer — nothing due.
        if offer.status == OfferStatus::Repaid || offer.status == OfferStatus::Defaulted {
            return 0;
        }

        let now = env.ledger().timestamp();
        if now < schedule.first_due {
            return 0;
        }

        let period = schedule.frequency.period_secs();
        // How many installments have elapsed (1-based)?
        let elapsed = ((now - schedule.first_due) / period + 1).min(schedule.count as u64) as u32;

        // How many installments are already covered by amount_repaid?
        let paid_count = if schedule.installment_amount == 0 {
            schedule.count
        } else {
            (offer.amount_repaid / schedule.installment_amount) as u32
        };

        if paid_count >= elapsed {
            // All elapsed installments already paid.
            0
        } else {
            // The next unpaid installment that is already due.
            paid_count + 1
        }
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
