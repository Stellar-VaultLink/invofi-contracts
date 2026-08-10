# ADR-0004 — Originator reputation scoring

**Status:** Accepted (2026-08-06)

## Context

Lenders need a way to screen borrowers before making an offer. The protocol
records repayment outcomes on-chain; the reputation contract turns those
raw counts into a simple, transparent, publicly-readable score.

## Decision

1. **Contract: new `reputation/` crate.** Owns a per-originator map of
   `{ repayments: u32, defaults: u32 }` in persistent storage.

2. **Recording: only the configured recorder can write.** The repayment
   contract is set as the recorder via `set_recorder` (admin-only) and
   calls `record_outcome(originator, outcome)` after every fully-repaid
   invoice (`0` = repaid) and every default (`1` = defaulted). Recording
   fails closed until a recorder is configured. User auth never propagates
   across contract boundaries, so the recorder's auth is implicit
   contract-invoker auth per Stellar's Authorization docs.

3. **Score formula — deliberately simple:**
   `score = repayments * 1 − defaults * 2`, floored at 0. This makes one
   default outweigh two successful repayments — a harsh but easy-to-audit
   signal for an early-stage protocol. The raw `ReputationRecord`
   (`get_record`) is public so any richer off-chain model can be computed
   from the same source data.

4. **Reading: no auth.** `get_score` / `get_record` are callable by anyone
   — the frontend can display a score without signing.

5. **Pause-guarded writes.** `record_outcome` checks the shared pause flag.

6. **Explicitly deferred (follow-up issue):** an amount-weighted or
   recency-decayed model, and migration of existing on-chain history into a
   new formula. The public `get_record` API makes a future re-scoring safe
   — scores are derived, raw outcomes remain the source of truth.

## Consequences

- Lenders get a queryable, honest default-risk signal per originator.
- Score is derived from raw counts; a future formula change cannot corrupt
  history.
- On-chain E2E proven on testnet: a full repayment records
  `{repayments: 1, defaults: 0}` -> score 1. The default outcome
  (`record_outcome(..., 1)`) is recorded by `reclaim_invoice` — a lender
  action after the grace period — and is covered by the contract test
  suite; a live default requires an invoice to pass its 7-day grace
  period, which is impractical on testnet.
