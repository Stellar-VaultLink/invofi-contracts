# Error Codes Registry

> **Single source of truth** for every machine-readable error code emitted by the
> InvoFi Soroban contracts. Clients (SDK, frontend, integrators) should branch on
> these stable codes — never on free-text error messages.

## Motivation

The frontend currently detects forbidden states by regex-matching error-message
strings (`/403|unauthorized|forbidden|not authorized|access denied/i`). That is a
presentation-layer heuristic — it breaks the moment an error message is reworded,
localised, or comes from a new contract ([invofi#152](https://github.com/Stellar-VaultLink/invofi/issues/152)).

This registry establishes the canonical naming scheme so clients can branch on
stable codes instead of string-matching human text. The transport mechanism
(error code getter vs. structured event payload) is defined in
[#93](https://github.com/Stellar-VaultLink/invofi-contracts/issues/93) /
[#139](https://github.com/Stellar-VaultLink/invofi-contracts/issues/139); this
document fixes the naming convention only.

## Naming Rules

| Rule | Detail |
|------|--------|
| **Prefix** | `E_` |
| **Case** | `SCREAMING_SNAKE_CASE` |
| **Uniqueness** | Globally unique across all contracts |
| **Stability** | Never reused with a different meaning |
| **Additions** | A new code is a reviewed, documented change — the registry diff is the review surface |

## Error Code Registry

### Initialization & Configuration

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_ALREADY_INITIALIZED` | Constructor has already been called; double-init is not allowed | All contracts | **Retry:** deployment issue — redeploy a fresh contract |
| `E_NOT_INITIALIZED` | Admin/config getter called before the constructor has run | All contracts | **Retry:** deployment issue — ensure the contract was deployed with `__constructor` |
| `E_NOT_CONFIGURED` | A required sub-contract address (registry, repayment, financing, insurance, reputation, position token, payout caller, recorder) has not been set by admin | Financing, Repayment, Insurance, Reputation | **Retry:** protocol admin must configure the missing address |

### Authorization & Access Control

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_UNAUTHORIZED` | Caller is not the required actor (admin, originator, lender, etc.) or the address's `require_auth()` failed | All contracts | **Redirect** to `/403` (or equivalent unauthorized page) |
| `E_BLACKLISTED` | The caller's address is on the protocol blacklist | Registry, Financing | **Redirect** to `/403` |
| `E_NOT_ADMIN` | Caller is not the current contract admin | All contracts | **Redirect** to `/403` or show admin-only error |
| `E_NOT_ORIGINATOR` | Caller is not the invoice originator | Registry, Financing, Repayment | **Redirect** to `/403` |
| `E_NOT_LENDER` | Caller is not the offer's lender | Financing, Repayment | **Redirect** to `/403` |
| `E_NOT_REGISTERED_CALLER` | Cross-contract caller is not the registered system contract (financing, repayment, insurance, reputation) | Registry, Financing, Repayment, Insurance, Reputation | **Retry:** protocol configuration issue — admin must register the system caller |

### Protocol Paused

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_PAUSED` | The contract is paused (emergency circuit breaker active) | All contracts | **Toast/banner:** "Protocol is paused. Please try again later." |

### Not Found

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_NOT_FOUND` | The requested entity (invoice, offer, rate tier) does not exist | Registry, Financing | **Toast:** "Not found" — show 404 state for the entity |

### Invalid State (Precondition Failure)

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_INVALID_STATE` | The entity is not in the required status for the requested operation (e.g. invoice not Pending, offer not Accepted, etc.) | All contracts | **Toast:** "This action cannot be performed in the current state." Disable the action button and refresh state |
| `E_INVOICE_NOT_PENDING` | Invoice must be Pending for this operation | Registry, Financing | **Toast:** Refresh invoice state |
| `E_INVOICE_NOT_FINANCED` | Invoice must be Financed for this operation | Registry, Repayment | **Toast:** Refresh invoice state |
| `E_INVOICE_NOT_OVERDUE` | Invoice must be Overdue for this operation (mark overdue, reclaim) | Registry, Repayment | **Toast:** Refresh invoice state |
| `E_INVOICE_NOT_DISPUTED` | Invoice must be in Disputed status to resolve | Registry | **Toast:** Refresh invoice state |
| `E_OFFER_NOT_PENDING` | Offer must be Pending for this operation (withdraw, accept, reject) | Financing | **Toast:** Refresh offer state |
| `E_OFFER_NOT_ACCEPTED` | Offer must be Accepted or Financed for this operation (repay, reclaim) | Repayment | **Toast:** Refresh offer state |
| `E_INVOICE_NOT_DEFAULTED` | Invoice must be in Defaulted status for insurance payout | Insurance | **Toast:** Refresh invoice state |
| `E_INVOICE_DUE_DATE_NOT_PASSED` | Invoice due date has not yet elapsed; cannot mark overdue | Registry | **Toast:** "Invoice is not yet overdue." |
| `E_GRACE_PERIOD_NOT_ELAPSED` | 7-day grace period after overdue has not yet elapsed; cannot reclaim | Repayment | **Toast:** "Grace period has not elapsed. Reclaim available after 7 days past due." |
| `E_TERMINAL_OFFER` | Cannot schedule repayment on a terminal (Repaid/Defaulted/Rejected) offer | Financing | **Toast:** Refresh offer state |
| `E_NEGOTIATION_CLOSED` | The offer negotiation is no longer open — the negotiation window (default 72 h, admin-configurable) elapsed, a party closed it, or it already settled | Financing | **Toast:** "This negotiation has ended." Disable amend/counter and refresh |

### Invalid Input / Parameters

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_INVALID_AMOUNT` | Amount is zero, negative, or below the minimum (`MIN_INVOICE_AMOUNT`) | Registry, Financing, Repayment, Insurance | **Toast:** "Invalid amount." Show field-level validation error |
| `E_DUST_AMOUNT` | Invoice amount is below `MIN_INVOICE_AMOUNT` (10 XLM / 10 USDC in stroops) | Registry | **Toast:** "Invoice amount must be at least 10 XLM" |
| `E_INVALID_RATE` | Interest rate is out of bounds (must be > 0 and ≤ 10,000 bps) | Financing, Registry | **Toast:** "Interest rate must be between 1 bps and 10,000 bps." |
| `E_INVALID_DURATION` | Offer duration is outside the allowed range (< 86,400 s or > 31,536,000 s) | Financing | **Toast:** "Duration must be between 1 day and 365 days." |
| `E_INVALID_FEE` | Protocol fee exceeds the cap (max 500 bps / 5%) | Registry | **Toast:** "Fee must be at most 5%." |
| `E_INVALID_INSTALLMENT_COUNT` | Installment count is outside 1–1,200 | Financing | **Toast:** "Installment count must be between 1 and 1,200." |
| `E_PAST_DUE_DATE` | Invoice `due_date` is in the past at registration time | Registry | **Toast:** "Due date must be in the future." |
| `E_FIRST_DUE_IN_PAST` | `first_due` for schedule is not in the future | Financing | **Toast:** "First due date must be in the future." |
| `E_ZERO_INSTALLMENT` | Computed installment amount is zero (amount/count too small) | Financing | **Toast:** "Installment amount too small." |
| `E_STALE_NEGOTIATION_ROUND` | `expected_round` does not match the negotiation's current length — the other party moved since the caller read state | Financing | **Toast:** "Terms changed while you were reviewing." Re-read the negotiation and resubmit |

### Duplicate Key

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_DUPLICATE` | An entity with the same ID already exists (invoice ID, offer ID) | Registry, Financing | **Toast:** "An item with this ID already exists." Generate a new ID or show conflict |

### Ownership & Reference Errors

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_SELF_FINANCE` | Lender is the same address as the invoice originator (self-financing prohibited) | Financing | **Toast:** "Lender cannot finance their own invoice." |
| `E_OFFER_INVOICE_MISMATCH` | Offer does not belong to the specified invoice | Repayment | **Toast:** Refresh offer/invoice state |

### Economic / Balance Errors

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_INSUFFICIENT_BALANCE` | Repayment amount exceeds remaining balance (principal + yield − already repaid) | Repayment | **Toast:** "Amount exceeds remaining balance." Show correct max |
| `E_INSUFFICIENT_STAKE` | Unstake amount exceeds the staker's staked balance | Insurance | **Toast:** "Insufficient staked balance." Show available |
| `E_INSUFFICIENT_POOL` | Insurance pool has no funds; payout returns 0 | Insurance | **Toast/info:** "Insurance pool is empty — no payout available." |
| `E_OVERPAYMENT` | Attempted repayment exceeds remaining due amount | Repayment | **Toast:** "Repayment amount exceeds remaining balance." |

### Dispute Errors

| Code | Meaning | Contract(s) | Client behaviour |
|------|---------|-------------|------------------|
| `E_DISPUTE_SELF_RESOLVE` | Attempted to resolve a dispute to the Disputed status itself (no-op) | Registry | **Toast:** "Cannot resolve to Disputed status." |

---

## Client Contract

The SDK must:

1. **Map** each typed contract error (see [#93](https://github.com/Stellar-VaultLink/invofi-contracts/issues/93)) to the corresponding `E_*` code.
2. **Expose** the error code to the frontend as a typed field (see [#139](https://github.com/Stellar-VaultLink/invofi-contracts/issues/139)).
3. **Never** rely on regex-matching free-text error messages — this heuristic ([invofi#152](https://github.com/Stellar-VaultLink/invofi/issues/152)) is retired once codes land.

The frontend must:

- Branch on `E_*` codes: redirect to `/403` on `E_UNAUTHORIZED` / `E_BLACKLISTED` / `E_NOT_ADMIN`, show toast on `E_PAUSED`, etc.
- Never introduce new regex-on-message heuristics.

## Cross-References

| Issue | Relationship |
|-------|-------------|
| [#93](https://github.com/Stellar-VaultLink/invofi-contracts/issues/93) | Rust typed-error enums in contract code (transport of codes) |
| [#139](https://github.com/Stellar-VaultLink/invofi-contracts/issues/139) | Human-readable frontend error mapping (SDK → frontend) |
| [invofi#152](https://github.com/Stellar-VaultLink/invofi/issues/152) | Branded 403 page — the regex heuristic this registry replaces |
| [invofi#141](https://github.com/Stellar-VaultLink/invofi/issues/141) | Enforcing all contract calls through the SDK |

## Adding a New Code

1. Add the code to the appropriate table above with meaning, contracts, and client behaviour.
2. The new code must follow naming rules (`E_` prefix, `SCREAMING_SNAKE_CASE`, unique, never reused).
3. Open a PR — the registry diff is the review surface.
4. Update the typed-error enum in [#93](https://github.com/Stellar-VaultLink/invofi-contracts/issues/93) and the SDK/frontend mapping in [#139](https://github.com/Stellar-VaultLink/invofi-contracts/issues/139).
