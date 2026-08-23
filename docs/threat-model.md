# InvoFi Threat Model

> **Status:** Living document, last updated 2026-08-23. Reviewed against `master`
> at the ADR-0010 multisig implementation.
>
> **Purpose:** evaluation baseline for the SCF Audit Bank and external security
> reviewers. It makes explicit what this repo protects, how each protection is
> enforced on-chain, and what it deliberately does **not** protect. This is a
> design-level model, not an audit; see
> [security-self-review.md](./security-self-review.md) for the code-level
> self-review and [../SECURITY.md](../SECURITY.md) for disclosure policy.
>
> Documentation only — this doc asserts nothing about code that is not already
> shipped and tested.

---

## 1. System summary

Five Soroban contracts (`registry`, `financing`, `repayment`, `insurance`,
`reputation`) plus a SEP-41 position token (`POS`). Function-by-function
reference: [README "Contract Functions"](../README.md#contract-functions).
ADRs under [`adr/`](./adr/README.md) record every security-relevant decision.

Money paths:

1. **Financing:** lender approves → `accept_offer` pulls principal
   (lender → originator) and mints POS to the lender.
2. **Repayment:** originator repays principal + yield (+ penalty) →
   lender (less protocol fee to admin primary signer).
3. **Insurance:** stakers deposit into a pool; on default,
   `reclaim_invoice` triggers `pay_out` of the uncovered claim, capped at pool balance.

## 2. Assets

| # | Asset | Stored / flows through | Impact if compromised |
|---|---|---|---|
| A1 | Staked insurance funds | insurance contract token balance | Direct loss of third-party funds |
| A2 | Loan principal in transit | lender allowance → originator via `accept_offer` | Misdirected principal |
| A3 | Repayment flows | repayer → lender / fee recipient | Misdirected or duplicated payments |
| A4 | Position-token mint integrity | POS supply (financing as token admin) | Unbacked claims on invoices |
| A5 | Invoice / offer state integrity | registry + financing storage (state machine) | Phantom financing, wrong default/repaid outcomes |
| A6 | Reputation scores | reputation storage | Lenders misprice credit risk |
| A7 | Verification attestations | registry oracle records | Forged invoices read as verified |
| A8 | Admin control (params, wiring, pause) | per-contract `AdminConfig` | Total protocol control |
| A9 | Protocol availability | pause flag, permissionless pokes | Griefing, lockout of legitimate actions |

## 3. Trust boundaries

```
 ┌────────────────────────────────────────────────────────────────────┐
 │ Off-chain                                                          │
 │  Originator/Lender/Staker wallets · Verifier oracle service        │
 │  Frontend/indexers (read-only) · Deploy CI (production env gate)   │
 └───────┬──────────────────────────┬─────────────────────┬───────────┘
   TB1 signed txns                  TB4 attest()          TB5 deploy op
   (require_auth                    (verifier set +       (deployer key,
    per address)                     m-of-n threshold)     constructor args)
 ┌───────▼──────────────────────────▼─────────────────────▼───────────┐
 │ Soroban host / ledger                                              │
 │  registry ⇄ financing ⇄ repayment ⇄ insurance ⇄ reputation         │
 │         TB2 cross-contract: registered-caller guards +             │
 │         implicit contract-invoker auth; user auth never            │
 │         propagates across boundaries                               │
 │              TB3 ↓ SEP-41 token contracts (external)               │
 └────────────────────────────────────────────────────────────────────┘
```

- **TB1 — User ↔ contract.** Every privileged entry point calls
  `require_auth()` on the acting address; the Soroban host verifies each
  authorization entry against the transaction's signers.
- **TB2 — Contract ↔ contract.** System transitions are callable **only** by
  the registered counterpart address (stored, admin-settable), authorized via
  implicit invoker auth. User auth never propagates across contract boundaries.
- **TB3 — Contract ↔ token contracts.** SEP-41 assets are external and
  **assumed standard and honest** (see §7 assumptions). The POS token's admin
  is the financing contract; mints authorize implicitly by invoker.
- **TB4 — Off-chain oracle ↔ registry.** The contract cannot verify real-world
  facts; it authenticates *who* attested and bounds trust by verifier-set
  membership, m-of-n distinctness, rejection dominance, and expiry (ADR-0009 §1).
- **TB5 — Deploy pipeline ↔ chain.** Admin and wiring are bound atomically in
  the deploy operation (ADR-0005); mainnet deploys additionally pass two human
  reviewer gates (`deploy-mainnet.yml`, `production` environment).

## 4. Threat actors

| Actor | Capabilities | Motivation |
|---|---|---|
| **Originator** (borrower) | Registers invoices, accepts offers, repays | Finance forged/inflated invoices; evade penalty or default consequences |
| **Lender** | Creates/amends offers, provides principal, reclaims on default | Extract value unfairly: early reclaim, payout games, negotiation sniping |
| **Admin** | All `set_*`, wiring, blacklist, dispute resolution, pause, `transfer_admin` | Parameter capture; key compromise is the realistic threat, not malice |
| **Keeper** | Permissionless pokes: `mark_overdue`, `expire_verifications`, `close_negotiation` (post-expiry) | Griefing; liveness failure (absence) |
| **Front-runner** | Observes the public mempool, inserts/reorders transactions | Free-option on pending state changes |
| Supporting: **Verifier** | Calls `attest` within the trusted set | Collusion up to threshold; negligence after approval |
| Supporting: **Staker** | `stake`/`unstake` any time | Exit before losses land (no lockup — accepted, §6) |

## 5. Threats and mitigations

Legend: **Enforcement** cites the enforcing function(s); **Tests** cite
representative regression tests. Status is either *Mitigated* or
*Accepted risk* (cross-referenced to §6).

### 5.1 Originator

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-O1 | Finance a forged invoice | m-of-n verification with structural distinctness (one attestation per `(verifier, type)`), rejection outranks approvals, expiry derived on read | `registry::attest`, `get_invoice_verification_status` (ADR-0009 §2–4) | `test_threshold_requires_distinct_verifiers`, `test_invoice_is_not_verified_until_every_type_is`, `test_attestation_expires_on_read_without_any_call` | Partially mitigated — financing does not *require* verified invoices (§6.5) |
| T-O2 | Register junk economics (dust amounts, past-due dates) | Input validation before state writes | `register_invoice`: `MIN_INVOICE_AMOUNT`, due-date check | `test_min_invoice_amount_rejected`, `test_register_invoice_past_due_date`, `test_register_invoice_zero_amount` | Mitigated |
| T-O3 | Overpay/manipulate repayment balances | Over-payment refused; offer must belong to invoice; Financed-only status guard; i128 + `overflow-checks = true` fails closed | `repay_invoice` guards (repayment/src/lib.rs:349–372, amount ≤ remaining) | over-payment & wrong-originator failure-path tests (self-review §6); integration `test_financing_repayment_full_repay_syncs_state` | Mitigated |
| T-O4 | Cancel or rewrite an invoice after financing | State machine: originator can only transition *Pending* invoices; system transitions caller-guarded | `update_invoice_status`, `cancel_invoice`, `assert_transition` (common/src/lib.rs:1111) | `test_update_invoice_status_on_non_pending_panics`, `test_cancel_non_pending_panics`, `test_state_machine_*` suite | Mitigated |
| T-O5 | Evade overdue penalty by timing keeper calls | Accrual anchored on immutable `due_date`, base frozen at `total_due`, hard cap, saturating math — call timing cannot inflate/erase accrued penalty (ADR-0007 §1–2,4–5) | `calculate_penalty`, `repay_invoice` | proptest monotonicity regressions (`repayment/proptest-regressions`) | Partially mitigated — Overdue dead end remains (§6.6) |
| T-O6 | Act while blacklisted | Blacklist checked in registry registration and financing offer creation | `is_blacklisted` checks (`registry::register_invoice`, `financing::create_offer` path, financing/src/lib.rs:168) | `test_blacklisted_cannot_register_invoice`, `test_blacklisted_cannot_create_offer` | Mitigated (admin-gated tool; abuse = admin compromise, §5.3) |

### 5.2 Lender

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-L1 | Contract moves funds without consent | Approve+pull only: `transfer_from` against the **lender's pre-granted allowance**; contract never moves unapproved funds | `accept_offer` (financing/src/lib.rs:467+), self-review §1.1 | `test_accept_offer`, `test_accept_offer_mints_position_token` | Mitigated |
| T-L2 | Self-dealing (lend to own invoice to farm insurance/reputation) | Offer rejected when lender == originator | `create_offer` guard | `test_create_offer_self_dealing_panics` | Mitigated |
| T-L3 | Reclaim before grace period elapses | Hard timestamp gate | `reclaim_invoice` (repayment/src/lib.rs:578: `now < due_date + GRACE_PERIOD_SECS` panics) | `test_repayment_insurance_reclaim_before_grace_panics` | Mitigated |
| T-L4 | Extract more than the claim from the pool | Payout capped `min(amount, pool_total)`; requires registry proof of `Defaulted` (not merely Overdue); callable only by configured payout caller; pause-guarded | `insurance::pay_out` (insurance/src/lib.rs:612+, safety checks 1–2) | `test_payout_pool_depleted_pays_whats_left`, `test_payout_rejected_when_invoice_overdue_not_defaulted`, `test_payout_without_caller_panics`, `test_paused_blocks_payout` | Mitigated |
| T-L5 | Execute stale/superseded counter-offers | Frozen deadline; round-index freshness checks; auto-settle only on exact term match | `amend_offer` / `counter_offer` / `close_negotiation` (ADR-0008) | `test_stale_round_index_is_rejected`, `test_counter_offer_with_stale_round_index_is_rejected`, `test_expired_counter_offer_cannot_be_executed_by_the_lender`, `test_superseded_counter_offer_is_not_executable` | Mitigated |
| T-L6 | Mint position tokens without an accepted offer | Token admin is the financing contract; mint happens only inside `accept_offer` with amount = validated `offer.amount`; config-gated | `StellarAssetClient::mint` call site (self-review §1.3) | `test_accept_offer_mints_position_token`, `test_accept_offer_without_position_token_still_works` | Mitigated |

### 5.3 Admin

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-A1 | Single compromised admin key controls everything | M-of-N multisig opt-in: distinct signers, each `require_auth`, threshold enforced; duplicates rejected; signer set bounded (`MAX_ADMIN_SIGNERS`) (ADR-0010) | `common::assert_threshold` (common/src/lib.rs:624), `validate_signers` (598), `set_signers` per contract | `test_multisig_requires_threshold_distinct_signatures`, `test_multisig_rejects_duplicate_signer_in_same_call`, `test_multisig_rejects_non_signer_address`, `test_threshold_2_of_3_boundary` | Partially mitigated — deployments stay threshold-1 until operator opts in (§6.1) |
| T-A2 | Front-run initialization of a fresh deployment | Eliminated: no `initialize()`; one-time setup runs in the constructor inside the atomic deploy operation; idempotency guard retained (ADR-0005) | each `__constructor` → `init_admin_config` (common/src/lib.rs:556) | `test_constructor_binds_admin_at_deploy`, `test_constructor_cannot_be_reinvoked` | Mitigated |
| T-A3 | Redirect cross-contract wiring or fee recipient | Wiring setters admin(-threshold)-gated; also pause-blockable; fee recipient resolves to primary signer | `set_financing_contract`, `set_repayment_contract`, `set_payout_caller`, `set_position_token`, `transfer_admin` | `test_transfer_admin_unauthorized_panics`, `test_pause_blocks_transfer_admin`, `test_set_signers_unauthorized_panics` | Mitigated modulo T-A1 residual |
| T-A4 | Mis-keyed parameter change (absurd rates/fees/windows) | Range validation everywhere params are set | `set_rate`, `set_fee`, `set_verification_fee` (≤ 500 bps ceiling), `set_attestation_validity` (1–365 d clamps), `set_negotiation_window` (1 h–30 d clamps), `set_penalty` (`MAX_PENALTY_BPS`) | `test_set_rate_out_of_range_panics`, `test_set_fee_too_high_panics`, `test_verification_fee_above_ceiling_panics`, `test_attestation_validity_below_minimum_panics`, `test_set_negotiation_window_below_minimum_panics` | Mitigated |
| T-A5 | Censor the protocol via indefinite pause | Same-block pause is a deliberate emergency brake (ADR-0001); unpause is itself threshold-gated once multisig is enabled | `pause`/`unpause`, `assert_not_paused` (common/src/lib.rs:512) | `test_pause_unauthorized_panics`, `test_pause_blocks_all_registry_state_changes` (+ per-crate suites) | Accepted risk — trusted-admin censorship is inherent (§6.2) |
| T-A6 | Rewrite reputation history arbitrarily | `resolve_dispute` is admin-gated and only neutralizes *one* recorded default per call; emits corrected score | `reputation::resolve_dispute` (ADR-0004 §7) | `test_resolve_dispute_non_admin_panics`, `test_resolve_dispute_favourable_neutralizes_default` | Mitigated (admin trust assumed; see T-A1) |

### 5.4 Keeper

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-K1 | Third party forces `mark_overdue`, locking the borrower out of repayment | Grace period delays loss; penalty accrual does not depend on the call (anchored on `due_date`), so the griefing payoff is bounded | `mark_overdue` state machine (ADR-0007 Context & Consequences) | `test_registry_repayment_overdue_on_pending_panics`, state-machine tests | Accepted risk — dead end documented, cure path filed separately (§6.6) |
| T-K2 | Spam `expire_verifications` for event noise | Idempotent: second call is a no-op; nothing depends on it being called (status derives from `now > valid_until` on read) | `expire_verifications` (ADR-0009 §4) | `test_expire_verifications_is_idempotent`, `test_attestation_expires_on_read_without_any_call` | Mitigated |
| T-K3 | Keeper absence delays state (no scheduler exists) | Economic truth is lazy-derived where it matters: negotiation expiry reads from frozen deadline; verification expiry reads from `valid_until`; penalty accrues regardless of flags | read-time derivation in financing negotiation status, registry verification status, repayment `calculate_penalty` | `test_negotiation_status_expires_on_read_without_any_call`, `test_attestation_expires_on_read_without_any_call` | Mitigated (liveness affects only indexer-visible events) |
| T-K4 | Stranger closes someone else's open negotiation | Only the two parties may close before the deadline; permissionless only after expiry | `close_negotiation` (ADR-0008) | `test_stranger_cannot_close_an_open_negotiation`, `test_close_negotiation_after_expiry_is_permissionless` | Mitigated |

### 5.5 Front-runner

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-F1 | Claim admin of an uninitialized contract between deploy and init | Constructor binds admin inside the deploy operation — there is no second transaction to front-run (ADR-0005) | `__constructor` / `init_admin_config` idempotency guard | `test_constructor_cannot_be_reinvoked` | Mitigated |
| T-F2 | Race a second `accept_offer` onto an already-financed invoice | Invoice must still be `Pending`; transition enforced by the registry state machine with registered-caller guard | `accept_offer` guard (invoice.status != Pending panics), `financing_marks_invoice_financed` (registry/src/lib.rs:634) | `test_registry_financing_offer_on_financed_invoice_panics`, `test_financing_transition_without_registration_panics` | Mitigated |
| T-F3 | Snipe a live counter-offer by amending onto it first | Auto-settlement on exact match is the *designed* outcome and settles atomically in the same call; near-miss terms never settle | `amend_offer` / `counter_offer` settlement logic (ADR-0008) | `test_auto_accept_when_lender_amends_onto_the_live_counter_offer`, `test_near_miss_terms_do_not_auto_accept` | Mitigated (by design, not prevention) |
| T-F4 | Generic MEV (ordering user transactions profitably) | No auction/commit-reveal mechanisms exist; transfers are fixed-amount against pre-stated terms; overflow/bounds fail closed | n/a — inherent property of the flow set | — | Accepted risk — public-mempool ordering is out of scope (§6.9) |
| T-F5 | Sandwich an admin parameter change (repay just before a fee hike, etc.) | Params are admin-threshold-gated; fee/rate apply at execution time by design; ceilings bound worst case | `set_fee`/`set_rate`/`set_verification_fee` ceilings (T-A4) | ceiling tests above | Accepted risk — small surface, bounded by caps (§6.9) |

### 5.6 Verifier (oracle)

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-V1 | Colluding verifiers approve a forged invoice | m-of-n **distinct** verifiers required; a live rejection dominates any number of approvals; removal revokes the vote while keeping the audit trail; expiry bounds staleness (ADR-0009 §2, §7) | `attest` distinctness, `type_status` evaluating current set only | `test_threshold_requires_distinct_verifiers`, `test_removed_verifier_cannot_attest_but_keeps_its_history`, `test_rotated_out_verifiers_cannot_lock_an_invoice` | Bounded, not eliminated — collusion ≥ threshold passes by design (§6.4) |
| T-V2 | Fee games (paid to approve / free to reject) | Fee charged for the work regardless of outcome (deliberately); `checked_mul`, division last, 5 % ceiling; unregistered-currency fee fails loudly | `attest` fee settlement (ADR-0009 §5–6) | `test_verification_fee_is_charged_to_the_originator`, `test_nonzero_fee_without_a_registered_token_panics` | Mitigated |
| T-V3 | Compromised verifier keeps attesting after removal | Non-members' `attest` calls panic; removed members' old records stop counting toward thresholds | `add_verifier`/`remove_verifier` + status evaluation | `test_add_verifier_is_admin_only`, `test_removed_verifier_cannot_attest_but_keeps_its_history` | Mitigated |

### 5.7 Cross-contract / token boundary

| ID | Threat | Mitigation | Enforcement | Tests | Status |
|---|---|---|---|---|---|
| T-X1 | Spoof system transitions (fake financing/repayment invokes state changes) | Stored registered-address check + `require_auth` on that address (implicit invoker auth); user auth never propagates | `assert_only_repayment` (financing/src/lib.rs:1062), registry transition guards (src/lib.rs:634, 670, 710), `record_outcome` recorder-only | `test_financing_transition_without_registration_panics`, `test_repayment_transition_without_registration_panics`, `test_record_outcome_without_recorder_panics` | Mitigated |
| T-X2 | Reentrancy through external calls | CEI deviations are enumerated and argued benign: pulls use consumed allowances; repayments spend only caller-authenticated funds; writer callbacks are caller-guarded (self-review §5) | ordering + guards in `accept_offer`, `repay_invoice`, `stake`; `assert_only_repayment` | integration suite (`integration/src/test.rs` full-lifecycle tests) | Mitigated with documented reliance on standard-token behaviour (§6.10) |
| T-X3 | Malicious or non-standard token contract | Out of scope: all tokens assumed to be standard SEP-41 assets; arithmetic fails closed (`overflow-checks = true`) | workspace `[profile.release]` | self-review §4 | Assumption — see §7 |
| T-X4 | Storage corruption / key collisions between crates | Instance-scoped storage keys per contract; bounded collections (`MAX_VERIFIERS`, `MAX_ATTESTATIONS_PER_INVOICE`, `MAX_NEGOTIATION_ROUNDS`, `MAX_ADMIN_SIGNERS`) | constants table (README) | cap tests: `test_negotiation_round_cap_is_enforced`, `test_rotated_out_verifiers_cannot_lock_an_invoice` | Mitigated |

## 6. What is NOT protected (documented tradeoffs)

Each item states the exposure, why it is accepted, and where the decision lives.

1. **Bootstrap deployments are single-key admins.** Every fresh deployment
   starts at `AdminConfig { signers: [admin], threshold: 1 }`. Until an operator
   explicitly calls `set_signers`, a single compromised key can do everything
   T-A1 describes. Accepted because Soroban instances here have no upgrade
   path — the multisig had to ship without changing the constructor ABI, and
   opting in is one post-deploy call (ADR-0010 Migration path). **Action for
   mainnet: run `set_signers` before funding.**
2. **No timelock on admin actions, including unpause.** Deliberate: the same-
   block brake is the feature (ADR-0001). A hostile/threshold-compromised admin
   acts instantly and cannot be stopped on-chain.
3. **No contract upgradeability.** A deployed bug can only be fixed by
   redeploying new WASM and re-wiring per the migration runbook
   ([migration-runbook.md](./migration-runbook.md)); state does not migrate
   automatically. Accepted: immutability was judged safer than an upgrade key.
4. **The contract cannot verify reality.** Attestations prove *what an approved
   verifier said*, not that an invoice is genuine. Off-chain oracle correctness
   is explicitly out of scope (ADR-0009 §1). Trust is bounded by the threshold;
   collusion at or above it defeats the oracle.
5. **Verification is not required to finance.** Nothing gates `create_offer` /
   `accept_offer` on `get_invoice_verification_status == Verified` — deliberate
   follow-up deferral (ADR-0009 Consequences). Lenders price unverified risk.
6. **The Overdue repayment dead end.** Once anyone calls `mark_overdue`
   (permissionless), the borrower can never repay and default becomes the only
   terminal state. Documented in ADR-0007 Consequences; the cure path alters
   the state machine and is filed separately rather than smuggled in.
7. **Insurance covers less than the loss, sometimes zero.** Coverage is
   `principal + yield − repaid`, capped at pool balance, with no shortfall
   persistence beyond the `off_def` event (ADR-0003 §3); accrued penalty is
   never covered (ADR-0007 §8). Lenders bear pool-depletion risk.
8. **Stakers have no lockup.** `unstake` is available at any time, so stakers
   can exit ahead of a visible default, thinning the pool exactly when lenders
   need it. Accepted for this stage; a lockup/notice window is unbuilt.
9. **Public-mempool front-running in general.** No commit-reveal or batch
   auctions anywhere. The specific high-value races (T-F1–T-F3) are closed by
   constructors, state machines, and atomic settlement; residual generic MEV on
   ordinary transfers is accepted as irreducible without redesign.
10. **Reentrancy safety leans on token honesty, not strict CEI.** Three
    documented CEI deviations (`accept_offer`, `stake`, plus router-only
    `repay_invoice`) are safe because allowances are consumed and standard
    tokens have no hooks (self-review §5). If a non-standard token were ever
    registered, this reasoning breaks. Follow-up reorder noted in self-review §7.
11. **Ledger-timestamp reliance.** Due dates, grace periods, expiry, and accrual
    all read the ledger timestamp provided by validators; sub-minute manipulation
    is assumed away per Stellar consensus assumptions.
12. **Compliance/KYC is off-chain.** No on-chain identity checks; jurisdictional
    policy lives in the app-layer compliance docs (README → Compliance).
13. **Back-compat getters are not authority.** `get_admin()` returns
    `signers[0]` for tooling convenience and is query-only everywhere (ADR-0010);
    integrators must never treat it as an authorization source.

## 7. Assumptions

- The Soroban host, SEP-40 authorization, and consensus (including timestamps)
  behave per spec.
- All SEP-41 tokens used for settlement/staking are standard, non-malicious
  asset contracts (T-X3).
- Signatures and hashes are sound; key management off-chain follows the deploy
  runbook (mainnet keys scoped to the gated `production` environment).

## 8. Reviewer guidance

Auditors should evaluate each §5 row against its cited enforcement function and
tests, and treat §6 items as in-scope findings **only if** the stated rationale
is factually wrong (e.g. a mitigation claimed but absent). Anything neither
mitigated nor listed in §6 should be reported as a gap via
[SECURITY.md](../SECURITY.md).
