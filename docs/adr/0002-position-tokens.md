# ADR-0002 — Position tokens: granularity, admin, and lifecycle

**Status:** Accepted (2026-08-05)

## Context

When an offer is accepted, the lender's principal moves to the business
immediately. To represent the lender's continuing claim (and enable transfer /
resale before repayment), the financing contract mints a token to the
lender at acceptance time.

## Decision

1. **Granularity: 1 position token = 1 base unit of the financed currency**
   (i.e. one token per stroop/unit of the offer amount; minted `offer.amount`).
   No fractionalization — an offer of 200 XLM mints 2,000,000,000 POS units at
   7 decimals. This keeps accounting trivial (token balance == claim) and lets
   any future secondary market split positions arbitrarily (transfer works at
   any granularity) without contract changes. More complex receipt structures
   (e.g. yield-bearing tokens) are deferred; documented here so the choice is
   explicit for reviewers.
2. **Token contract: a Stellar Asset Contract (SAC)** deployed as a classic
   asset `POS:<protocol admin>` — the audited, SDF-maintained reference token
   implementation. No custom token code in the auditable contract surface.
3. **Admin: the financing contract.** The SAC is initialized with (and then
   `set_admin`'d to) the financing contract address, so `accept_offer` mints
   via the host's implicit contract-invoker auth — no user signatures needed
   for the mint, and no separate per-lender approvals.
4. **Holders need a `POS` trustline.** Because the token is a Stellar asset,
   mint/transfer credit requires the recipient's trustline. The frontend checks
   and offers a one-click trustline setup; this is standard Stellar asset UX
   and deliberately not papered over in contract code.
5. **Lifecycle: mint-on-accept only.** Burn/redemption on repayment, insurance
   payout integration, and any "position → repayment entitlement" settlement
   are future work (insurance payout; settlement flow). Minting happens only
   after the principal transfer succeeds, so a failed funding can never mint.

## Alternatives considered

- **Custom SEP-41 token contract** — rejected: adds un-audited token code to
  the audit surface when the SAC already provides a battle-tested
  implementation.
- **Off-chain receipts (no token)** — rejected: no transferability story, which
  is the point of position tokens.
- **Fractional ERC-style shares** — rejected for now: more complexity than the
  use case needs; granularity choice above already permits arbitrary splits.
