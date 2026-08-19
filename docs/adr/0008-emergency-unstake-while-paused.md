# ADR-0008 — Emergency withdrawal path while the insurance pool is paused

**Status:** Accepted

## Context

ADR-0001 introduced a same-block emergency pause (circuit breaker) in which
every state-changing function begins with `assert_not_paused`. The insurance
pool's `stake` and `unstake` were both treated as paused operations like any
other.

That creates a liveness hazard specific to the insurance pool: the pause switch
exists to stop *the wider protocol* (invoice financing, repayments, payouts) when
something is wrong. It is not about the stakers themselves. If a pause is
triggered for a prolonged incident, stakers have no path to withdraw their own
funds — money stays locked indefinitely even though the incident may be entirely
unrelated to them (see issue #67).

## Decision

`unstake` is **not** guarded by `assert_not_paused`. A staker may withdraw their
own position at any time, including while the pool is paused. `stake` and
`pay_out` remain pause-guarded.

The rationale is the direction of the mutation:

- **`stake`** moves new funds *into* the pool and grows the protocol's exposure.
  During an incident it must stay locked so an attacker or confused user cannot
  grow a position the protocol may need to unwind.
- **`pay_out`** moves pooled funds *to a third party* (a lender) automatically.
  This is the highest-risk operation and must never run during an incident.
- **`unstake`** only unwinds the caller's *own* claim and moves the pool's own
  holdings back to the staker. It is a safety valve, not a state mutation of the
  wider protocol: it reduces the pool's exposure, cannot grow anyone else's
  position, and cannot move funds the staker does not already own. Letting it
  succeed while paused is strictly safer for stakers and does not interfere with
  the incident response.

The alternative — fully locked funds plus an admin-gated
`emergency_withdraw` that refunds every staker pro-rata — was considered and
rejected. It concentrates yet more power in the single pause/admin key (the very
trust assumption ADR-0001 already flags as a single point of compromise),
requires a new permissioned entry point with its own edge cases (rounding,
iteration over unbounded staker sets, race with `pay_out`), and still does not
let an individual staker exit on their own terms. A per-staker `unstake` is the
smallest change that satisfies the requirement and keeps custody with the staker.

## Consequences

- Stakers can always recover their funds, so a prolonged pause no longer locks
  them out. Withdrawals remain a safety valve.
- Staking new funds and automatic payouts stay halted while paused, preserving
  the circuit breaker's purpose.
- The pause-guard coverage matrix in `invofi_common` is updated to list `unstake`
  as an explicit exception alongside `pause`/`unpause`/getters.
- The pool's accounting invariants are unaffected: `unstake` performs the same
  balance/pool-total reduction and token transfer whether or not the pool is
  paused, so `get_stake` sums continue to equal `get_pool_total`.
- This amends ADR-0001's blanket statement that "every state-changing function
  begins with `assert_not_paused`" for this one, narrowly-scoped insurance
  function.
