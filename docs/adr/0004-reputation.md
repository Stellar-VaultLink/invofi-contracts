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

   **Score decay (issue #139):** outcomes older than the configured
   half-life (`DECAY_HALF_LIFE_SECS`, default 90 days) contribute less
   via exponential decay. The cumulative weighted values
   (`weighted_repayments`, `weighted_defaults`) are recomputed on each
   `record_outcome` or `resolve_dispute` call: pending decay is applied
   first (scaling existing values by `2^(-elapsed / half_life)`), then
   the new outcome is added at full weight. `get_score` reads the cached
   recomputed value in O(1). A `ReputationChanged` event (`rep_chg`) is
   emitted whenever the recomputation changes the stored score.

   Key properties:
   - **Fresh outcomes hit immediately** — a new default contributes its
     full −2 weight with no gaming window.
   - **Old outcomes decay** — an originator's one old default weighs less
     after months of clean repayment.
   - **Score floor at 0 is preserved** — decayed scores are still floored.
   - **Cheap reads** — `get_score` returns the cached value, no
     recomputation on read.
   - **Dispute resolution applies pending decay** — when a default is
     neutralized, the cumulative values are decayed first, then the
     default's weight is removed.

4. **Reading: no auth.** `get_score` / `get_record` are callable by anyone
   — the frontend can display a score without signing.

5. **Pause-guarded writes.** `record_outcome` checks the shared pause flag.

6. **Fully-weighted historical model (deferred follow-up):** a richer
   amount-weighted model that considers invoice sizes is tracked as a
   separate follow-up issue. The public `get_record` API makes a future
   re-scoring safe — scores are derived, raw outcomes remain the source
   of truth. Score decay (issue #139) is now implemented as the
   recency-decayed model.

7. **Dispute-aware adjustment (issue #134):** a default that a dispute
   later overturns must not keep punishing the originator. Because the
   reputation contract tracks only aggregate counts — not invoices — the
   admin (the same key that resolves disputes in the registry) calls
   `resolve_dispute(admin, originator, originator_favourable)` on the
   reputation contract after a registry resolution:
   - `originator_favourable = true` **neutralizes** one previously-recorded
     default — `defaults` decrements by one, floored at 0 — so the `-2`
     penalty stops counting against the originator. The scoring formula
     itself is untouched: the neutralized default simply no longer exists
     in the count.
   - `originator_favourable = false` leaves the recorded outcome
     unchanged; a penalty already applied by `record_outcome(..., 1)`
     stands. Recording a *fresh* default for a resolution against the
     originator remains the repayment contract's job, out of scope here.
   - The adjustment emits a `ReputationChanged` event (topic `rep_chg`,
     payload = corrected score) so indexers and the marketplace UI can
     observe the correction. `get_score` / `get_record` stay public and
     read-only. Admin auth mirrors `resolve_dispute` in the registry.

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
