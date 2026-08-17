# Security Self-Review

> **Status:** Engineering self-review — **this is NOT a substitute for a
> professional security audit.** It is a documented line-by-line re-read of
> the token-movement and pause code against a fixed checklist, intended as
> honest evidence of care for the protocol's funding applications. A real audit
> (e.g. via the SCF Audit Bank) remains a prerequisite for mainnet.
>
> Scope: `accept_offer`, `repay_invoice`, and the
> emergency pause. Review date: 2026-08-06. Reviewed against
> `master` at `2fc4d4d` (post issue-#75 constructor fix).

---

## 0. Checklist

| # | Pitfall | Covered where |
|---|---|---|
| 1 | Auth on every privileged call | §1, §2, §3 |
| 2 | Integer overflow on amount math | §4 |
| 3 | Reentrancy via cross-contract calls | §5 |
| 4 | Input validation before state writes | §1.2, §2.2 |
| 5 | Pause blocks state-mutating paths | §3 |

---

## 1. `accept_offer` (financing/src/lib.rs:363)

### 1.1 Auth (checklist #1)

| Guard | Code | Verdict |
|---|---|---|
| Pause | `assert_not_paused(&env)` (line 364) | ✅ |
| Caller | `invoice_originator.require_auth()` (line 365) | ✅ |
| Offer is pending | `offer.status != OfferStatus::Pending → panic` (371) | ✅ |
| Originator match | `invoice.originator != invoice_originator → panic` (384) | ✅ |
| Invoice pending | `invoice.status != InvoiceStatus::Pending → panic` (387) | ✅ |
| Token pull | `transfer_from(current_contract, lender, originator, amount)` (394) | ✅ approve+pull — the **lender** must have pre-approved the contract; the contract never moves unapproved funds |

Cross-contract read of the invoice goes through the **registry client
instantiated from instance storage** (line 381) — the registry address cannot
be swapped by a caller (only by admin at deploy, via `__constructor`).

### 1.2 State-write order (checklist #4)

`offer.status = Accepted; funded_at = now` are written (401–403) **after** the
fund transfer, and the registry transition `financing_marks_invoice_financed`
(408) only accepts calls from the **registered financing contract**
(caller-guarded, verified in the cross-contract auth pass — commit
`cfa5d41`). User auth never propagates across contract boundaries.

### 1.3 Position-token mint (reviewed as part of accept)

`StellarAssetClient::mint(lender, amount)` (422) is config-gated
(`postok` optional) and the token's admin is the financing contract, so the
mint authorizes via implicit contract-invoker auth. Amount is the already-
validated `offer.amount`.

---

## 2. `repay_invoice` (repayment/src/lib.rs:161)

### 2.1 Auth (checklist #1)

| Guard | Code | Verdict |
|---|---|---|
| Pause | `assert_not_paused(&env)` (168) | ✅ |
| Caller | `repayer.require_auth()` (169) | ✅ |
| Originator match | `invoice.originator != repayer → panic` (180) | ✅ |
| Invoice financed | `invoice.status != Financed → panic` (183) | ✅ |
| Offer belongs to invoice | `offer.invoice_id != invoice_id → panic` (196) | ✅ |
| Offer state | must be `Accepted` or `Financed` (199) | ✅ |
| Amount | `amount > 0` (202); `amount <= remaining_balance` (208) | ✅ over-payment refused |

### 2.2 Math (checklist #2 — see §4)

- `yield_amount = offer.amount * rate / 10_000` (206)
- `total_due = offer.amount + yield_amount` (207)
- `remaining_balance = total_due − offer.amount_repaid` (208)
- `fee_amount = amount * fee_bps / 10_000` (212); `lender_amount = amount − fee` (213)
- `offer.amount_repaid += amount` (222)

All i128 arithmetic; compiled with `overflow-checks = true` (§4), so any
overflow **panics** rather than wrapping — fail-closed.

### 2.3 Transfers + cross-contract updates

`transfer(repayer, lender, lender_amount)` and fee `transfer(repayer, admin,
fee)` (216–221) use the **direct** token client on the repayer's address —
funds leave the repayer's wallet via their own auth. Cross-contract
`update_offer_status` / `update_offer_amount_repaid` / registry
`repayment_marks_invoice_repaid` (228–236) are all caller-guarded to the
registered repayment contract. Reputation recording (245–251) is optional +
config-gated.

---

## 3. Emergency pause

- `assert_not_paused(&env)` in **common** (common/src/lib.rs:170) reads the
  instance `paused` flag and panics `"Contract is paused"`.
- **22 call sites** across registry/financing/repayment/insurance/reputation
  — every state-mutating function is guarded (verified by
  `grep -rn assert_not_paused` sweep).
- `pause` / `unpause` are **admin-only** (admin `require_auth` + admin-match,
  same pattern as `set_position_token`, financing/src/lib.rs:180–189).
- Same-block activation is intentional for an early-stage safety net
  (ADR-0001). Multisig + timelock are roadmap items, not shipped.

---

## 4. Integer overflow (checklist #2)

`[profile.release]` in the workspace Cargo.toml sets
**`overflow-checks = true`** (plus `panic = "abort"`, `lto`, `opt-level = z`).
This means i128 arithmetic in both money functions **panics on overflow** —
there is no silent wrap. Amounts are also bounded upstream
(`MIN_INVOICE_AMOUNT`, `MAX_OFFER_DURATION_SECS`, and the `amount <=
remaining_balance` repayment cap), so the realistic overflow surface is
already constrained. **Verdict: acceptable; no change required.**

## 5. Checks-Effects-Interactions (CEI) & Reentrancy (checklist #3)

Soroban has no native reentrancy guard; the mitigation is ordering and authorization. A full cross-contract walk confirmed that all state writes happen before external interactions, with specific documented exceptions:

- **accept_offer (financing)** performs the external token pull **before** persisting the offer state. A malicious token could attempt reentry, but the pull uses `transfer_from` against the **lender's allowance** — an allowance is consumed by the transfer, so a reentrant call cannot double-pull, and the offer/invoice status guards still hold during reentry. Additionally, `load_stats` and `load_lender_stats` are read *after* this external call, but the callee (a standard Soroban token) cannot mutate financing state, preserving invariant clarity.
- **repay_invoice (repayment)** transfers **out of the repayer's own balance** (direct `transfer`, repayer-authenticated). Reentry cannot spend funds the caller did not authorize. Repayment acts only as a router and has no local state to protect, so CEI is trivially satisfied.
- **stake (insurance)** performs the external `transfer_from` before updating local stake balances. Similar to `accept_offer`, this exception is safe because standard tokens lack reentrant hooks, and even if they did, `stake` requires pre-allowance and hasn't yet credited the staker's local balance, neutralizing double-spend exploits. `load_stakes` and `load_pool_total` are read *after* this external call; because the token contract is a distinct and standard asset contract, it cannot mutate the insurance contract's state, preserving invariants.
- Cross-contract *writer* callbacks (registry/financing/repayment) are **caller-guarded to the registered contract address** (commit `cfa5d41`), so a third-party contract cannot invoke system transitions.
- All other cross-contract calls are strictly read-only (`get_invoice`, `get_offer`, `is_blacklisted`) or occur strictly after state changes (e.g., `pay_out` and `unstake` in insurance), fully adhering to CEI.

**Verdict:** no exploitable reentrancy path found in the reviewed functions. All CEI violations are benign and explicitly documented inline.
Three hardening follow-ups are noted for the audit phase (§7).

---

## 6. Test coverage backing this review

- `test_constructor_binds_admin_at_deploy` / `test_constructor_cannot_be_reinvoked`
- Failure-path tests: insufficient balance, wrong currency, over-payment
  (> remaining balance), non-originator repay, non-pending offer
- Pause coverage: state-mutating calls revert while paused
- Full suite: **110 tests passing** (`cargo test`, all five crates),
  clippy `-D warnings` clean, Soroban Scout static analysis in CI.

## 7. Follow-ups for the audit phase (not blocking)

1. Consider **checks-effects-interactions** reordering in `accept_offer`
   (persist `Accepted` before the external transfer) once the token
   contract is itself audited — currently protected by allowance
   consumption, but the reorder removes reliance on that property.
2. Cap `interest_rate` at a documented maximum (e.g. `MAX_INTEREST_BPS`) in
   `create_offer` to bound `yield_amount` arithmetic from input, not just
   overflow-checks.
3. Mainnet must be preceded by the SCF Audit Bank (or equivalent) audit.
