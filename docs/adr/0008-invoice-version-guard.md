# ADR-0008: Invoice Optimistic-Concurrency Version Guard

- Status: Accepted
- Date: 2026-08-18
- Issue: [#110](https://github.com/Stellar-VaultLink/invofi-contracts/issues/110)

## Context

Soroban processes each transaction in a serial, deterministic order, but two
client-submitted transactions can both be *constructed* against the same
ledger state before either has been applied. A classic example:

1. Lender A and Lender B both fetch invoice `INV-001` at ledger N (status:
   Pending, no offer accepted yet).
2. Originator races to accept both offers in two transactions, both of which
   were constructed against ledger N.
3. Depending on transaction ordering, both `accept_offer` calls read the
   invoice as Pending and attempt to transition it to Financed.

Before this ADR the second transition could silently succeed (or fail for
unrelated reasons), violating the invariant that an invoice may only be
financed once.

### Dependency on ADR / Issue #78

Issue #78 (invoice state-machine — `assert_transition` helper in `common`)
**has shipped** on this branch (`feat/78-invoice-state-machine` merged into
`security/110-invoice-version-guard`). The version guard is therefore
implemented *alongside* `assert_transition` inside `common`, sharing the same
import path. The two concerns remain orthogonal: `assert_transition` enforces
*which* transitions are legal; the version guard enforces *when* they land.

## Decision

### 1. Add `version: u64` to `Invoice`

A monotonically-increasing counter is stored in the `Invoice` struct in
`common/src/lib.rs`:

```rust
pub struct Invoice {
    // … existing fields …
    /// Optimistic-concurrency counter. Starts at 0; increments by exactly 1
    /// on every persistent write that changes any field of the invoice.
    pub version: u64,
}
```

**Initial value**: `0`, set by `register_invoice` in the registry.

**Increment rule**: the registry increments `version += 1` inside every
function that writes the invoice back to storage — regardless of which field
changed. This includes all status transitions, amount updates, and dispute
transitions.

### 2. Caller-supplied expected version

Entrypoints that mutate invoice state accept an `expected_version: u64`
argument from the caller:

| Entrypoint | Contract |
|---|---|
| `accept_offer(offer_id, originator, expected_version)` | financing |
| `repay_invoice(invoice_id, offer_id, repayer, amount, expected_version)` | repayment |

The caller reads `invoice.version` from `get_invoice` before submitting the
transaction and passes that value as `expected_version`.

### 3. Guard implementation — `check_invoice_version`

A single helper in `common/src/lib.rs`:

```rust
pub fn check_invoice_version(env: &Env, stored: u64, expected: u64) {
    if stored != expected {
        env.panic_with_error(ContractError::StaleVersion);
    }
}
```

Called in each guarded entrypoint **after** reading the invoice and **before**
any side effects (token transfers, cross-contract calls). This preserves the
CEI (Checks–Effects–Interactions) pattern.

`ContractError::StaleVersion` has discriminant `9`, so the Soroban host
surfaces it as `Error(Contract, #9)`.

### 4. Scope

The guard is **not** applied to offers in this issue. Offer-level concurrency
is tracked separately under issues #77 and #79. As a side effect, `version`
is incremented for *every* registry write (including offer-unrelated ones like
`update_invoice_amount`, `cancel_invoice`, `raise_dispute`,
`resolve_dispute`), which means callers must re-read the invoice after any
intervening write — not only after state transitions.

### 5. Version semantics at each lifecycle step

| Step | Who writes | Stored version after |
|---|---|---|
| `register_invoice` | registry | 0 |
| `accept_offer` → `financing_marks_invoice_financed` | registry (via financing) | 1 |
| First `repay_invoice` → `repayment_marks_invoice_repaid` | registry (via repayment) | 2 |
| Second `repay_invoice` (partial chains) | registry (via repayment) | 3 |
| `mark_overdue` | registry (via repayment) | n+1 |
| `cancel_invoice` | registry | n+1 |
| `update_invoice_amount` | registry | n+1 |
| `raise_dispute` / `resolve_dispute` | registry | n+1 |

### 6. Client retry strategy

A client that receives `Error(Contract, #9)` should:
1. Re-fetch the invoice via `get_invoice` to obtain the current `version`.
2. Re-validate that the desired operation is still valid (e.g. status is still
   Pending before accepting).
3. Resubmit with the fresh `expected_version`.

This is identical to the standard optimistic-concurrency retry loop used in
databases and is well-understood by SDK consumers.

## Consequences

**Positive**
- Two transactions racing on the same invoice can no longer both succeed;
  exactly one wins, the other gets a clean, retriable error.
- The guard is trivially cheap: one integer comparison before any I/O.
- The `version` field is visible in `get_invoice` responses, giving indexers
  and frontends a cheap change-detection signal.

**Negative / tradeoffs**
- All callers of `accept_offer` and `repay_invoice` must supply the version
  they read. Existing integrations need a one-field addition.
- In sequential test flows the version must be tracked accurately (e.g. after
  `accept_offer` the version is 1; after a partial `repay_invoice` it is 2).
  Test helpers must be updated when new fixtures are added.
- The guard does **not** protect registry-internal system calls
  (`financing_marks_invoice_financed`, `repayment_marks_invoice_repaid`) —
  these are already protected by `assert_transition` and by cross-contract
  auth; adding a version argument would require the caller to thread the
  version across contract boundaries, adding complexity for no additional
  safety.

## Alternatives considered

**CAS in storage key** — encode the version in the storage key itself so the
write fails atomically if the key changed. Rejected: Soroban's key-value store
has no native compare-and-swap primitive; the pattern would require two
reads to simulate, which is more expensive and more complex than a simple
field comparison.

**Sequence number from ledger** — use the ledger sequence as the version.
Rejected: a version derived from ledger sequence is meaningful for
*timing* but not for *change count*; two writes in the same ledger would not
be distinguishable, and partial-repayment chains need per-write granularity.

**Event-sourced append-only log** — rejected as out of scope for this change
and requires a significant protocol redesign.
