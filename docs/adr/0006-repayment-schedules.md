# ADR-0006 — Fixed Installment Repayment Schedules

**Status:** Accepted  
**Issue:** [#133](https://github.com/Stellar-VaultLink/invofi-contracts/issues/133)  
**Date:** 2026-08-17

---

## Context

Repayment in the InvoFi protocol has always been open-ended: the originator
can repay any amount at any time until the financed invoice is fully cleared.
This works well for ad-hoc settlements but businesses that prefer weekly or
monthly fixed payments — and lenders who want predictable cash-flow
forecasting — have no structured path.

## Decision

Introduce an **advisory fixed-installment repayment schedule** that can be
attached to any live financing offer. The schedule lives in the financing
contract (alongside the offer it describes) and is surfaced via two new
entry-points and one keeper-friendly read helper.

### Data model

```
RepaymentSchedule {
    offer_id:           Symbol,
    count:              u32,           // number of installments
    frequency:          ScheduleFrequency, // Daily | Weekly | Monthly
    installment_amount: i128,          // per-installment amount (computed)
    first_due:          u64,           // Unix timestamp of first due date
}

ScheduleFrequency { Daily = 86_400 s, Weekly = 604_800 s, Monthly = 2_592_000 s }
```

### Installment math — flat-rate equal-principal model

```
installment_principal = floor(offer.amount / count)
installment_yield     = installment_principal × offer.interest_rate / 10_000
installment_amount    = installment_principal + installment_yield
```

Floor division means up to `count − 1` stroops of principal may be left
uncovered by the schedule. This rounding remainder is acceptable because the
schedule is advisory — `offer.amount_repaid` is always the authoritative
source of truth for actual repayment progress.

### New API surface

| Function | Contract | Auth |
|---|---|---|
| `schedule_repayment(offer_id, caller, frequency, count, first_due)` | financing | Lender OR originator |
| `get_schedule(offer_id)` | financing | Anyone |
| `get_installment_due(offer_id)` | financing + repayment (proxy) | Anyone |

`get_installment_due` returns the **1-based index of the first unpaid elapsed
installment**, or 0 when nothing is due (schedule absent, all paid, or
`now < first_due`). Callers use this as a keeper signal:
- `0` → nothing to do
- `n > 0` → installment *n* is overdue; the originator should pay
  `installment_amount` to clear it

### Advisory-with-enforcement design

Ad-hoc partial repayments via `repay_invoice` remain fully permitted and do
not modify or invalidate the schedule. The schedule's only enforcement
mechanism is the `get_installment_due` signal — UI, keepers, or integrations
may surface it to the originator but the protocol never blocks a valid
repayment because it is "off-schedule".

This mirrors how traditional invoice financing works in practice: missing an
agreed payment date is a relationship / credit-score event, not an automatic
on-chain default (which is handled by the separate `reclaim_invoice` /
`mark_overdue` flow that already exists).

Penalty logic for missed installments is explicitly **out of scope** for this
ADR and left as a future work item per the issue brief.

## Alternatives considered

### 1. Store the schedule in the repayment contract

Rejected: the repayment contract is the executor; installment meta-data
belongs with the offer it describes. Keeping it in financing also avoids a new
cross-contract call from `get_installment_due`.

### 2. Amortized (reducing-balance) schedules

Rejected for this iteration. Equal-principal slices are simpler to implement,
auditable in a single formula, and sufficient for the majority of short-term
invoice financing durations (< 1 year). The `ScheduleFrequency` type and the
`count`/`installment_amount` pair are forward-compatible with a future
amortization model if needed.

### 3. On-chain enforcement (block repay if off-schedule)

Rejected. Strict enforcement would break backward compatibility, hurt
businesses with variable cash flow, and put the risk of a protocol freeze
(due to a missed cron job or network congestion) on the originator. Advisory
enforcement keeps the feature safe and composable.

## Consequences

- **`common`**: two new `#[contracttype]` types (`ScheduleFrequency`,
  `RepaymentSchedule`) and three new methods on `FinancingInterface`.
- **`financing`**: a persistent `Map<Symbol, RepaymentSchedule>` keyed by
  `offer_id`; three new public entry-points.
- **`repayment`**: one new thin proxy `get_installment_due` that delegates to
  financing — so CLI/keeper integrations that already hold the repayment
  address need only one contract reference.
- **Test coverage**: 13 new tests in `financing` + 2 new tests in `repayment`
  covering all three acceptance criteria from issue #133.
- **No migration required**: existing deployed offers without a schedule
  continue to work identically (`get_installment_due` returns 0,
  `get_schedule` returns `None`).
