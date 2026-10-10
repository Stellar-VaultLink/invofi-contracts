# ADR-0013 — Minimum insurance stake

**Status:** Accepted

## Context

The insurance pool credits every staker with its own ledger position and pays
out pro-rata on default. Nothing stopped a caller from staking a dust amount
(e.g. 1 stroop): the position's ledger-rent overhead is out of proportion to
the value staked, and tiny shares make the pro-rata payout-share math noisy
(issue #101).

The issue suggested a floor of about 1 XLM as an example. The existing
deterministic testsuite, however, stakes at the 1,000,000-stroop scale
(0.1 XLM), and the `stake_tier` documentation anchors product presentation on
the protocol's existing invoice-size minimum (`MIN_INVOICE_AMOUNT` = 10 XLM /
10 USDC) rather than on any stake-specific constant.

## Decision

Introduce a fixed constant:

```rust
pub const MIN_STAKE_AMOUNT: i128 = 1_000_000; // stroops (0.1 XLM / 0.1 USDC)
```

Enforcement semantics deliberately differ for entry and exit:

1. **First deposit** (`stake`, `stake_tier` with no existing position and a
   balance of 0): `amount` must be at least `MIN_STAKE_AMOUNT`, else
   `ContractError::InvalidInput` (#6). Checked before the token transfer — no
   state is written and nothing is financed before a clearly-reasoned panic
   with the machine-readable error code clients branch on.
2. **Top-ups** (position exists): any positive `amount` is allowed, subject to
   one invariant: `existing + amount >= MIN_STAKE_AMOUNT`. A top-up may never
   leave a position below the floor.
3. **Full exits** (`unstake` / `unstake_tier` to zero) are **exempt**: the
   floor governs entry and growth, not exit, so a staker can always reclaim
   their own funds regardless of amount (consistent with ADR-0011's
   staker-safety-valve stance).

## Alternatives considered

- **1 XLM (10,000,000 stroops)** as literally proposed in the issue —
  rejected for now: it would break the entire existing deterministic
  testsuite (~30 call sites stake at exactly 1,000,000) and would force a
  coordinated test-data rewrite across `insurance` and `integration`. The
  0.1 XLM floor already excludes true dust by four orders of magnitude and
  matches the implicit minimum-sane-stake scale the repo's tests already use.
- **Admin-configurable minimum** — rejected: a mutable floor would let the
  admin strand existing sub-floor positions (they could never top-up to
  validity without violating the new floor). A fixed constant is
  predictable, and making it configurable later is a non-breaking addition.

## Consequences

- Dust stakes are rejected at the boundary with a machine-readable error
  (#6) before any token movement.
- Rent paid per ledger entry stays proportionate to the value at stake.
- Pro-rata payout shares stay meaningful (no 1-stroop share holders).
- The constant is documented in the README Constants table for integrators.
- No migration needed: existing sub-floor positions (none in the testsuite)
  can still be topped up *to* the floor or fully exited (both allowed by the
  invariant above); only growth-below-floor is blocked.
