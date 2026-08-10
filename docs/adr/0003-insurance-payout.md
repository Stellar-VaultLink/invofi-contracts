# ADR-0003 — Insurance payout on default

**Status:** Accepted (2026-08-06)

## Context

The insurance crate built a staking pool with flat pool accounting but no
payout logic. The payout hook wires that pool to the protocol's realized-credit-loss
event: when a Financed invoice is never repaid and the lender reclaims
after the 7-day grace period, the lender should be made whole up to the
pool's available balance. This is the highest-risk code in the plan because
it moves staked funds automatically, so the design deliberately favors
defaults-safe behavior over generosity.

## Decision

1. **Trigger: `reclaim_invoice` (repayment contract).** After the grace
   period elapses, the lender's `reclaim_invoice` call transitions the
   offer to Defaulted and the invoice Overdue -> Defaulted (via the
   registry's authorized `repayment_marks_defaulted` system transition).
   In the same call, the repayment contract invokes
   `insurance.pay_out(lender, remaining_due)`.

2. **Exposure covered = principal + yield − amount_repaid.** The lender's
   outstanding claim is computed as `offer.amount + offer.amount *
   interest_rate / 10_000 − offer.amount_repaid`, floored at 0.

3. **Payout capped at the pool's available balance.** `pay_out` pays
   `min(requested, pool_total)` and returns the amount actually paid — 0
   when the pool is empty. The lender's claim is *not* partially persisted
   on-chain beyond the returned value; the `off_def` protocol event carries
   the payout amount so indexers can track shortfalls. Full recovery beyond
   the pool (e.g. treasury top-up) is deferred and noted as a follow-up.

4. **Caller restriction: only the configured payout caller.** `pay_out`
   requires auth from the address set via `set_payout_caller` — the
   repayment contract. Users can never call it directly.

5. **Pro-rata staker reduction.** The pool reduces each staker's balance
   proportionally to their share of the pool; the final staker absorbs the
   integer-division remainder so reductions sum exactly to the payout.
   Stakers with zeroed balances are removed from the map.

6. **Pause-guarded.** `pay_out` checks the shared pause flag; a
   paused pool rejects payouts.

7. **Optional wiring.** If no insurance contract is configured, the payout
   hook is skipped entirely — deployments without insurance keep the
   prior behavior.

## Consequences

- Lenders are partially insured against default, up to pool capacity.
- Stakers bear default losses pro-rata — the economic incentive to only
  back sound invoices.
- Payout math is deterministic and capped; a depleted pool yields `off_def`
  with `payout = 0`, which tests cover explicitly.
- A full on-chain default E2E requires waiting out the 7-day grace period,
  which is impractical on testnet; the default path (default -> payout ->
  pool-depleted) is covered by the contract test suite. What is proven live
  on-chain is the keeper's Overdue transition (mark_overdue) and the
  offer/position-token/repayment lifecycle; the Overdue -> Defaulted
  transition is exercised by tests and awaits a live reclaim once a
  Financed invoice passes its grace period.
