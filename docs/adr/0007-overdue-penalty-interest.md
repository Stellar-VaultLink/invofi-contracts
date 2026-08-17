# ADR-0007 — Overdue penalty interest

**Status:** Proposed (2026-08-17)

## Context

Repayment charges a flat rate: the borrower owes `principal + principal *
interest_rate / 10_000`, and that figure is identical whether they repay on
the due date or a year later. An invoice that blows past its due date costs
the borrower nothing extra, so there is no on-chain pressure to pay, and the
lender's only recourse is to wait out `GRACE_PERIOD_SECS` and reclaim — which
pushes the loss onto the insurance pool rather than onto the party that caused
it (issue #49).

Three facts about the existing code shape this design.

**The Overdue status is a dead end for repayment.** `mark_overdue` is
permissionless and moves an invoice Financed -> Overdue. But `repay_invoice`
requires `status == Financed`, and the registry's authorized transition
`repayment_marks_invoice_repaid` enforces the same precondition. The only
transition out of Overdue is `repayment_marks_defaulted` (Overdue ->
Defaulted). So once *anyone* calls `mark_overdue`, the originator can never
repay; default becomes the only reachable terminal state. A penalty that
accrued only in the Overdue status would therefore be uncollectible by
construction — the borrower has no entrypoint through which to pay it.

**Accrual anchored on a status transition is gameable.** `mark_overdue` is
permissionless and nothing schedules it. If accrual started at the timestamp
of that call, a borrower who kept a keeper from firing would accrue nothing,
and a third party could inflate another user's liability by choosing when to
call it.

**Lazy accrual on the *outstanding* balance is exploitable.** Deriving the
penalty at read time as `rate x outstanding_principal x elapsed` charges the
*current* principal across the *whole* elapsed window. A borrower who sits at
300 days overdue and then repays 99% of principal collapses the accrued
penalty by ~99% in the same transaction, because the next read multiplies the
full 300 days by the now-tiny balance. Any scheme that both (a) recomputes
from scratch at read time and (b) uses a base that repayments shrink has this
retroactive-erasure hole.

## Decision

1. **Accrual is anchored on `invoice.due_date`, not on the Overdue status.**
   Penalty accrues whenever `now > invoice.due_date`, regardless of whether
   the invoice is still labelled Financed or has been flipped to Overdue.
   `due_date` is immutable, set at registration, and is already the anchor for
   both `mark_overdue` and the `due_date + GRACE_PERIOD_SECS` reclaim gate, so
   this keeps one clock for the whole overdue lifecycle. It also makes the
   penalty collectible: the past-due-but-still-Financed window is exactly
   where `repay_invoice` still works.

2. **The accrual base is frozen at `total_due` (principal + yield).** The base
   is fixed at the value the obligation had on its due date and does not
   shrink as repayments land. This is what closes the retroactive-erasure hole
   in the Context: the base is not a function of `amount_repaid`, so no
   payment can rewrite penalty that has already accrued. Accrued penalty is
   therefore monotonically non-decreasing in time, which is the invariant the
   tests assert.

   The cost is that a borrower who pays down 90% of the balance still accrues
   penalty on the original base. That is punitive, and deliberately so — it is
   bounded by the cap in (4), and the alternative (checkpointing accrued
   penalty and a `last_accrual_ts` on every payment) requires new fields on the
   shared `FinancingOffer` struct, a storage migration, and a new authorized
   financing entrypoint. Not worth it for a bounded, capped charge.

3. **Granularity is whole elapsed days, truncated.** `elapsed_days = (now -
   due_date) / 86_400` in integer arithmetic. Per-second accrual produces
   stroop-level dust on realistic invoice sizes and widens the overflow
   surface for no economic gain on an instrument whose terms are quoted in
   days. Truncation means the partial day in progress is not charged, so
   rounding runs in the **borrower's** favour. That direction is chosen so the
   protocol never overcharges on a boundary; it is a deliberate choice, not an
   artifact of integer division, and it is asserted in the tests.

4. **A hard cap bounds total accrued penalty.** `penalty = min(raw, total_due *
   penalty_cap_bps / 10_000)`. Without a ceiling, a long-abandoned invoice
   accrues without bound, which would make the lender's nominal claim — and,
   if the pool ever covered it, staker losses — grow purely as a function of
   how long nobody acted. The cap makes worst-case liability a fixed multiple
   of the original obligation, known at origination.

5. **Arithmetic is `i128` throughout with saturating multiplication.** Every
   intermediate (`total_due * penalty_bps * elapsed_days`) is computed in
   `i128` with `saturating_mul`, so a pathological `elapsed_days` saturates
   rather than wrapping. Division by `10_000` happens last, after the
   multiplications, to avoid compounding truncation error.

6. **Configuration lives on the repayment contract and defaults to disabled.**
   `set_penalty(admin, penalty_bps, cap_bps)` mirrors the registry's
   `set_rate` / `set_fee` pattern: pause-guarded, admin-only, range-validated.
   Both values default to **0**, which makes `accrued_penalty` return 0 on
   every path. Deploying this change is therefore a no-op until an admin
   explicitly enables it — the feature gate the issue asks for, in addition to
   the existing pause guard that already covers `repay_invoice`,
   `reclaim_invoice`, and `set_penalty` itself. `penalty_bps` is capped at
   `MAX_PENALTY_BPS` (500 = 5%/day) so a mis-keyed admin call cannot set an
   absurd rate; `cap_bps` is capped at 10_000 (100% of the obligation).

   Storage lives on repayment rather than registry or financing because all
   three consumers (`repay_invoice`, `reclaim_invoice`, `calculate_total_due`)
   are repayment entrypoints — this keeps config reads local instead of adding
   a cross-contract hop to the hot path.

7. **Repayments settle against one combined figure; there is no
   penalty-vs-principal ordering.** The amount owed at time `t` is `total_owed
   = total_due + penalty(t)`, and `amount_repaid` accumulates against it;
   `fully_repaid` is `amount_repaid >= total_owed(t)` evaluated at payment
   time. Because every repayment routes to the same place — the lender, less
   the existing protocol fee — splitting a payment into a "penalty part" and a
   "principal part" would change no transfer and no balance. The ordering
   question that a two-bucket design would force (does a partial clear penalty
   first or principal first?) simply does not arise. The protocol fee applies
   to the whole payment, penalty included, consistent with how it already
   treats yield.

8. **The insurance pool does not cover accrued penalty.** `reclaim_invoice`
   computes the penalty and reports it, but the amount claimed from the pool
   stays `principal + yield - amount_repaid`, exactly as ADR-0003 defines it.
   Penalty is a punitive charge owed by the originator, not an insured credit
   loss; folding it into the claim would make staker losses a function of how
   long a defaulted invoice went unreclaimed, which is precisely the unbounded
   exposure the cap in (4) exists to prevent. The accrued figure is emitted on
   the `off_def` event so indexers can track the lender's uncovered claim.

## Relationship to ADR-0006 (fixed installment repayment schedules)

ADR-0006 landed while this was in review. The two do not interact, and that is
deliberate: penalty accrual is anchored on `invoice.due_date`, whereas an
installment schedule carries its own `first_due` and per-installment
timestamps. A missed *installment* therefore accrues no penalty — only the
invoice due date does.

That is the right split for now, because ADR-0006 states the schedule is
**advisory**: `offer.amount_repaid` remains the authoritative record, ad-hoc
repayments never invalidate a schedule, and `get_installment_due` is a keeper
signal rather than an enforcement gate. Charging a penalty off an advisory
structure would make it enforcing through the back door, without the design
review that change deserves. Per-installment penalties are a coherent future
feature, but they belong in their own ADR that first decides whether schedules
become binding.

## Consequences

- Borrowers face a real, compounding-by-the-day cost for running past due,
  bounded by the cap — the lender-protection gap in #49 closes for the
  past-due-but-Financed window.
- Deployments that never call `set_penalty` are bit-for-bit unchanged in
  behaviour, so this can ship dark and be enabled per-network after review.
- `calculate_total_due` now performs a cross-contract read of the invoice (it
  previously read only the offer) because it needs `due_date`. This makes a
  previously offer-only query depend on the registry being reachable.
- The `off_def` event gains a fourth element (accrued penalty). Indexers that
  destructure it positionally need updating; this is noted in the CHANGELOG.
- **The Overdue repayment dead end is not fixed here.** Once `mark_overdue`
  fires, the borrower still cannot repay and still cannot pay the penalty,
  and since `mark_overdue` is permissionless a third party can force that
  state at will. Penalty accrual makes the consequence sharper — the meter
  keeps running on an obligation the borrower is locked out of settling. A
  cure path (accepting Overdue in `repay_invoice`, plus an Overdue ->
  Financed/Repaid registry transition) is the natural follow-up and is filed
  separately rather than smuggled into this change, because it alters the
  invoice state machine and needs its own review.
