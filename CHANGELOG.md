# Changelog

All notable changes to InvoFi Contracts are documented here.
Versioning follows [Semantic Versioning](https://semver.org/).
Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
and are enforced by commitlint in CI.

## [Unreleased]

### Added
- **Offer amendment and counter-offer protocol (issue #180)** — financing
  offers are no longer take-it-or-leave-it. A lender can revise their own terms
  with `amend_offer`, an originator can name theirs with `counter_offer`, and
  when the two sides land on the same terms the offer settles in that same
  transaction. Design in `docs/adr/0008-offer-negotiation.md`.
  - **Auto-accept matches two pre-existing commitments** rather than spending
    on a counterparty's behalf. Agreement is exact equality of the canonical
    term tuple `(amount, interest_rate, duration)` — no tolerance, no rounding.
    When the originator counters at the lender's standing terms, their own
    `require_auth` covers the settlement; when the lender amends onto the
    originator's live counter-offer, what authorizes financing them is the
    counter-offer they themselves recorded on-chain. Settlement runs the
    extracted `settle_acceptance`, the same code path `accept_offer` uses, so
    the two can never drift.
  - **Every round is version-guarded.** `amend_offer` / `counter_offer` take
    `expected_round`, the history length the caller believes it is amending. A
    round written against a negotiation that moved underneath it reverts with
    `InvalidInput` instead of applying to terms the caller never saw — which is
    what would otherwise let a stale counter-offer auto-execute.
  - **A recorded counter-offer is bounded and revocable.** It stays executable
    only inside the negotiation window (default 72 h, admin-configurable within
    1 h – 30 days via `set_negotiation_window`); the deadline is frozen when
    the negotiation opens, so later rounds never push it out; proposing again
    supersedes, since only a party's most recent record is live; and either
    party can end the negotiation outright with `close_negotiation`.
  - **Expiry is derived on read**, since Soroban has no scheduler:
    `get_negotiation_status` computes `Expired` from the deadline whether or
    not anyone has called anything. `close_negotiation` is the poke that
    persists the outcome and emits the event — permissionless after the
    deadline, restricted to the two parties before it.
  - New storage keyed `("negot", offer_id)` as a `Vec<NegotiationRecord>`, capped
    at 20 rounds so the entry stays bounded. New reads `get_negotiation`,
    `get_negotiation_status`, `get_negotiation_deadline`,
    `get_negotiation_window`. New events `off_amd`, `ctr_off`, `neg_clsd`.
  - Amended terms are validated against the same bounds `create_offer`
    enforces, so a negotiation cannot reach terms the offer could not have been
    created with.
  - 26 new tests, driven through the contract client and asserting on real
    settlement effects (token balances, registry invoice status): both
    auto-accept directions, the near-miss that must not settle, a superseded
    counter-offer that must not execute, stale-round rejection from both sides,
    derived expiry at the deadline boundary, an expired counter-offer that can
    no longer be taken, revocation, permissionless post-deadline close, the
    round cap, the pause and authorization guards, and the events.
- **Partial repayment with pro-rata interest (issue #176)** — originators
  can now make partial payments against an invoice, with interest calculated
  pro-rata on the remaining principal: `interest = remaining × rate_bps ×
  days_elapsed / 3_650_000`. The offer stays `Financed` until the remaining
  principal reaches zero, at which point the final payment triggers full
  settlement.
  - New `PaymentRecord` struct in `common/src/lib.rs` tracks each payment's
    id, amount, interest_paid, principal_paid, timestamp, and payer.
  - Payment history stored on-chain as `Vec<PaymentRecord>` per invoice
    (storage key `("pays", invoice_id)`), queryable via
    `get_payment_history(invoice_id)`.
  - Minimum partial payment enforced: each payment must be ≥ 1% of the
    original principal unless it fully settles the remaining balance.
  - `get_remaining_principal(offer_id)` and
    `calculate_accrued_interest(offer_id)` read-only queries added.
  - Two new Soroban events: `parpay` (partial payment received, carrying
    offer_id, amount, principal_portion, interest_portion, remaining) and
    `inv_frp` (invoice fully repaid, carrying offer_id, amount,
    principal_portion, interest_portion). The legacy `inv_rep` event is
    preserved for backward compatibility.
  - `calculate_total_due` now returns `remaining_principal + pro-rata
    interest + penalty` instead of the previous flat-yield model.
  - All existing tests updated for the new interest model; proptest
    (2 000 cases) validates the math invariants.
- **Cross-crate integration test harness** — new `integration/` workspace crate
  that deploys all five contracts (registry, financing, repayment, insurance,
  reputation) with mock tokens and drives the full invoice lifecycle across
  contract boundaries. Tests cover every cross-crate boundary: registry ↔
  financing (offer acceptance), financing ↔ repayment (repay), repayment ↔
  insurance (default payout), repayment ↔ reputation (outcome recording),
  and registry ↔ repayment (overdue/delegate). At least one happy-path and
  one negative test per boundary. Existing per-crate unit tests are untouched
  (issue #103).
- **Overdue penalty interest (issue #49)** — `repayment` now accrues a
  penalty on obligations past their invoice due date, threaded through
  `calculate_total_due`, `repay_invoice`, and the `reclaim_invoice` payout
  math. Design in `docs/adr/0007-overdue-penalty-interest.md`.
  - Accrual is anchored on `invoice.due_date` (not the permissionless
    Overdue status transition, which would be gameable by withholding a
    keeper call) and runs in whole elapsed days, truncated — the partial day
    in progress is not charged, so rounding favours the borrower.
  - The accrual base is **frozen** at principal + yield and does not shrink
    as repayments land. This is deliberate: a base tracking the outstanding
    balance would let a large late partial payment retroactively erase
    penalty already accrued, since a read-time recomputation would apply the
    reduced principal across the entire elapsed window.
  - Total accrued penalty is capped at `penalty_cap_bps` of that base, so
    worst-case liability is a fixed multiple of the original obligation
    rather than a function of how long the invoice went unattended.
  - New admin entrypoint `set_penalty(admin, penalty_bps, cap_bps)`
    (pause-guarded, admin-only, `penalty_bps` ≤ 500 = 5%/day, `cap_bps` ≤
    10 000), with getters `get_penalty_bps` / `get_penalty_cap_bps`. **Both
    parameters default to 0, which disables accrual entirely** — this change
    is behaviourally inert until an admin enables it per network.
  - New read-only `calculate_penalty(offer_id)` exposes the penalty
    component on its own, for UIs that show it separately from the combined
    figure.
  - The insurance pool does **not** cover accrued penalty: the claim in
    `reclaim_invoice` remains principal + yield − repaid, per ADR-0003.
    Penalty is a punitive charge owed by the originator, not an insured
    credit loss, and covering it would make staker losses grow with the time
    a defaulted invoice went unreclaimed.
  - 16 new tests cover per-day accrual, the truncation boundary, the cap
    boundary (days 299/300/400/5 000), the frozen base across a 95% partial
    repayment, penalty-must-be-settled-for-full-repayment, accrual continuing
    across the Overdue transition, exclusion from the insurance payout, and
    the admin/range/pause guards on `set_penalty`.

### Changed
- **`calculate_total_due` now reads the registry.** It previously read only
  the offer from financing; it needs `invoice.due_date` to compute accrued
  penalty, so this query now also depends on the registry being reachable.
- **The `off_def` event gained a fourth element** (accrued penalty, `i128`),
  emitted by `reclaim_invoice` so indexers can track the portion of the
  lender's claim the pool did not cover. Consumers that destructure the
  payload positionally need updating.

### Docs
- **Migration runbook**: add `docs/migration-runbook.md` with the snapshot
  state reads, the five-contract redeploy (workflow and manual stellar-cli
  fallback), the frontend env re-point, and the verify step via the manual
  e2e walkthrough (issue #104). The runbook lists which state is re-derivable
  on-chain and which must be re-created, and it is linked from the README
  development section. Closes #125.

## [0.7.0] – 2026-08-06

### Docs
- **README refresh** — roadmap section (shipped vs open), wallet
  support note (Freighter + LOBSTR via the approved-wallet allowlist), and
  corrected test count (110 across all five crates).
- **Compliance link** — README now points at
  `docs/compliance.md` in the main repo (KYC/SEP-12 roadmap, jurisdictions,
  securities-by-design analysis).
- **Commitlint gate** — CI now rejects PRs whose commits are not
  Conventional Commits, matching the app repo.

## [0.6.1] – 2026-08-06

### Security
- **Deployer-bound initialization (issue #75)** — `initialize()` removed from
  every contract (registry, financing, repayment, insurance, reputation). All
  one-time setup now runs in the Soroban **constructor** (`__constructor`),
  which executes atomically inside the deploy operation and can only be
  authorized by the deployer. This eliminates the front-running window where a
  third party could call `initialize` on a freshly deployed contract and
  become admin. Design in `docs/adr/0005-deployer-bound-initialization.md`.
- **Deploy workflow hardened** — `deploy-contract.yml` now passes constructor
  args at deploy time (`stellar contract deploy -- --admin … --registry …`)
  in dependency order (registry → financing → repayment → insurance →
  reputation); the separate post-deploy initialize step is removed, and
  reputation (previously missing) is deployed + wired (`set_insurance`,
  `set_reputation`, `set_payout_caller`, `set_recorder`). `deploy.sh` updated
  to match.
- **Regression tests** — new `test_constructor_binds_admin_at_deploy` and
  `test_constructor_cannot_be_reinvoked` (registry) and
  `test_constructor_binds_admin_and_staking_token` (insurance) prove the
  admin is bound at deploy and that a post-deploy `__constructor` invoke
  fails. All 110 tests pass with constructor-based test deployment.

## [0.6.0] – 2026-08-06

### Added
- **Insurance payout on default** — `reclaim_invoice` now triggers
  `insurance.pay_out(lender, principal + yield − amount_repaid)` after the
  grace period, capped at the pool's available balance and restricted to the
  configured payout caller (the repayment contract). The invoice transitions
  Overdue → Defaulted in the registry (`repayment_marks_defaulted` system
  transition). Pro-rata staker reduction with deterministic remainder
  handling; `pool_pay` protocol event carries the payout amount. Design in
  `docs/adr/0003-insurance-payout.md`. Pool-depleted and no-insurance paths
  are first-class (payout 0 / hook skipped).
- **Reputation contract** — new `reputation/` crate: repayment
  records `record_outcome(originator, 0|1)` after every full repayment and
  default; `get_score` / `get_record` are public reads. Score =
  `repayments − 2×defaults`, floored at 0 (ADR-0004). Recording fails closed
  until a recorder (the repayment contract) is configured.
- **Event-completeness audit** — every state-mutating registry
  function now publishes a structured event. Added `inv_amt`
  (`update_invoice_amount`) and `inv_sts` emissions from
  `set_invoice_repaid_status` / `repayment_marks_invoice_repaid`, so the
  indexer can reconstruct the full invoice lifecycle from events alone.

### Verified (testnet)
- Full on-chain E2E on a fresh 5-contract deployment: register → offer →
  accept (XLM moved + POS minted) → full repay with interest (invoice Repaid,
  reputation recorded: `{repayments: 1, defaults: 0}` → score 1) → insurance
  stake of 50 XLM (pool total verified) → keeper marked a near-due Financed
  invoice Overdue and a past-due one on the previous deployment. All
  cross-contract wiring getters verified (`get_insurance`, `get_reputation`,
  `get_payout_caller`, `get_recorder`).

## [0.5.0] – 2026-08-05


### Added
- **Position tokens** — `accept_offer` now mints a SEP-41 position
  token to the lender, 1:1 with the offer amount. New admin-gated
  `set_position_token` / `get_position_token` on the financing contract; the
  token (a Stellar Asset Contract issued as `POS`) is admin'ed to the financing
  contract so minting authorizes via implicit contract-invoker auth. Design and
  granularity documented in `docs/adr/0002-position-tokens.md`. The mint is
  optional — deployments without a configured position token work unchanged.
- **Insurance pool** — new `insurance/` crate: `stake` / `unstake`
  with flat pool accounting (per-staker `Map<Address, i128>` + pool total),
  pause guard, and audit helper `get_contract_token_balance`. Payouts and yield
  rates deliberately deferred. 11 new tests.
- **Position-token transfer** — position tokens are standard SEP-41
  assets, so transfers use the token contract's own `transfer`; frontend
  portfolio gains a "Transfer Position" form and a one-click `POS` trustline
  helper (holders must hold a trustline, standard Stellar asset behavior).

### Verified (testnet)
- Full on-chain lifecycle re-verified on a fresh deployment: register → offer →
  accept (principal moved **and** POS minted) → position transfer between two
  wallets → insurance stake/partial-unstake. Contract IDs in the invofi README.

## [0.4.0] – 2026-08-03

### Added
- **Currency registry** — `register_currency(admin, currency, token)` and
  `get_currency_token(currency)` now let the admin register arbitrary
  Symbol → SEP-41 token mappings. `accept_offer` and `repay_invoice` resolve
  the token to move funds through the registry, falling back to the legacy
  single-token `initialize()` value for unbuckled currencies. Adding a third
  currency is one `register_currency` call, not a branch in every
  money-touching function.
- **Soroban Scout static analysis** — `scout-security-analysis.yml` workflow
  runs CoinFabrik's Soroban Scout on every PR, failing CI on security issues.
- **Failure-path tests** — 4 new tests cover insufficient balance and wrong
  currency on both `accept_offer` and `repay_invoice`.

## [0.3.1] – 2026-08-02

### Fixed
- **`update_invoice_status` now requires originator auth** and is restricted to
  Pending invoices — previously anyone could flip any invoice to any status.
  Emits a new `inv_sts` protocol event.
- **`get_lender_active_total` includes `Financed` offers** — a partially repaid
  offer is still a live position until fully cleared.
- **Pause guard applied consistently** — `create_offer`, `reject_offer`,
  `repay_invoice`, `reclaim_invoice`, `mark_overdue`, `cancel_invoice`,
  `withdraw_offer`, and `update_invoice_amount` now respect the admin pause
  switch like the other mutating functions.
- **`update_invoice_amount` enforces `MIN_INVOICE_AMOUNT`** (was `> 0`) and the
  pause guard.
- **Protocol + lender stats bookkeeping completed** — `total_financed`,
  `total_repaid`, `total_fee_revenue`, `LenderStats.total_accepted`, and
  `LenderStats.offers_repaid` are now incremented on the corresponding state
  transitions (previously they were never written).
- **Removed the unused legacy `invofi-core` crate** from the workspace.

## [0.3.0] – 2026-07-13

### Added
- **Protocol events** — every state-mutating function now publishes a Soroban
  contract event, enabling off-chain indexers, activity feeds, and real-time
  UI updates without polling:

  | Event topic | Emitted by | Data payload |
  |---|---|---|
  | `inv_reg`  | `register_invoice` | `(originator, amount, due_date)` |
  | `off_new`  | `create_offer` | `(invoice_id, lender, amount, interest_rate)` |
  | `off_acc`  | `accept_offer` | `(invoice_id, lender, amount)` |
  | `off_rej`  | `reject_offer` | `invoice_id` |
  | `off_wdr`  | `withdraw_offer` | `lender` |
  | `off_def`  | `reclaim_invoice` | `(invoice_id, lender)` |
  | `inv_rep`  | `repay_invoice` | `(offer_id, amount, fully_repaid)` |
  | `inv_ovd`  | `mark_overdue` | `due_date` |
  | `inv_cxl`  | `cancel_invoice` | `originator` |
  | `inv_dsp`  | `raise_dispute` | `originator` |
  | `inv_rsl`  | `resolve_dispute` | `new_status` |

  Every event carries the subject's `Symbol` id as its second topic, so
  indexers can filter by invoice or offer without decoding payloads.
- Event emission tests covering register, offer create/accept, repayment,
  and cancellation flows

### Changed
- `version()` returns `soroban_sdk::String` (was `&'static str`, which is not
  a valid Soroban return type)

## [0.2.0] – 2026-07-12

### Added
- `MIN_INVOICE_AMOUNT` constant (10 XLM) — enforced in `register_invoice`
- `MAX_OFFER_DURATION_SECS` constant (365 days) — enforced in `create_offer`
- `InvoiceStatus::Disputed` variant for on-chain dispute tracking
- `raise_dispute(invoice_id, originator)` — business can flag a Financed invoice as disputed
- `resolve_dispute(admin, invoice_id, target_status)` — admin resolves disputes
- `LenderStats` struct with per-address counters (total_offered, total_accepted, offers_pending, offers_repaid)
- `get_lender_stats(lender)` — returns the full `LenderStats` for a lender
- `get_lender_active_total(lender)` — sum of all Accepted offer amounts for a lender
- `get_invoices_count()` / `get_offers_count()` — fast total-count queries
- `get_offers_by_status(status)` — filter all offers by `OfferStatus`
- `get_invoices_by_currency(currency)` — filter all invoices by currency symbol
- `get_invoices_due_before(timestamp)` — open invoices whose `due_date` is before a timestamp
- `get_pending_offers_by_invoice(invoice_id)` — only Pending offers attached to an invoice
- `get_invoices_paginated(offset, limit)` / `get_offers_paginated(offset, limit)` — cursor-free pagination
- `batch_get_invoices(ids)` — multi-ID fetch in a single RPC call
- `get_min_invoice_amount()` — on-chain introspection for the minimum amount constant
- `get_offer_duration_limits()` — on-chain introspection for duration bounds
- `version()` — returns `CARGO_PKG_VERSION` as a static string
- Protocol statistics now increment on `register_invoice` and `create_offer`
- Admin blacklist: `blacklist_address`, `unblacklist_address`, `is_blacklisted`, `get_blacklist`
- Protocol stats: `get_stats()` returns `ProtocolStats` struct with global counters

### Changed
- `register_invoice` now rejects amounts below `MIN_INVOICE_AMOUNT`
- `create_offer` now rejects durations above `MAX_OFFER_DURATION_SECS`

## [0.1.0] – 2026-06-01

### Added
- Core invoice registry: `register_invoice`, `get_invoice`, `update_invoice_status`
- Financing offer lifecycle: `create_offer`, `accept_offer`, `reject_offer`, `withdraw_offer`
- Repayment: `repay_invoice` with partial repayment support
- Overdue and default handling: `mark_overdue`, `reclaim_invoice`
- Query helpers: `get_invoices_by_status`, `get_invoices_by_originator`, `get_offers_by_invoice`, `get_offers_by_lender`, `get_all_invoices`, `get_all_offers`, `calculate_total_due`
- Invoice management: `update_invoice_amount`, `cancel_invoice`
- Admin controls: `initialize`, `pause`, `unpause`, `transfer_admin`, `set_fee`, `get_fee`
- Yield rate oracle: `set_rate`, `get_rate` by `RiskTier` (A/B/C)
- Protocol fee deduction from repayments (configurable, max 5%)
