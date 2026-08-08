# Architecture Decision Records — InvoFi Contracts

ADRs capture decisions with lasting consequences for the protocol. New ADRs
get the next number; append, never rewrite (status updates go in the file).

| # | Decision | Task | Status |
|---|---|---|---|
| 0001 | [Emergency pause (circuit breaker)](./0001-emergency-pause.md) | 4A | Accepted |
| 0002 | [Position tokens: granularity, admin, lifecycle](./0002-position-tokens.md) | 7/8 | Accepted |
| 0003 | [Insurance payout on default](./0003-insurance-payout.md) | 10 | Accepted |
| 0004 | [Reputation scoring](./0004-reputation.md) | 11 | Accepted |
| 0005 | [Deployer-bound initialization (constructors)](./0005-deployer-bound-initialization.md) | #75 | Accepted |

App-layer decisions (wallet allowlist, indexer, SDK) live in the monorepo:
[invofi/docs/adr](https://github.com/Stellar-VaultLink/invofi/tree/main/docs/adr).

## Backlog map

Every open issue in this repo is cross-linked from the decision (or doc) it
implements, verifies, or extends. Each ADR ends with a *Related issues
(backlog)* list; use them as the starting point when picking up work.

| Backlog area | Issues | Home |
|---|---|---|
| Pause / circuit breaker | #85, #87, #88 | [ADR-0001](./0001-emergency-pause.md) |
| Position tokens / SEP-41 | #80, #81, #94, #97 | [ADR-0002](./0002-position-tokens.md) |
| Insurance pool & payouts | #82, #94, #101, #102 | [ADR-0003](./0003-insurance-payout.md) |
| Reputation scoring | #83, #100 | [ADR-0004](./0004-reputation.md) |
| Deployment & verification | #96, #104, #105 | [ADR-0005](./0005-deployer-bound-initialization.md) |
| Protocol events spec | #90, #95, #100 | [README → Protocol Events](../README.md#protocol-events) |
| Offer / invoice lifecycle | #77, #78, #79, #92 | README → Contract Functions |
| Indexer & keeper integration | #84, #91, #98 | [invofi/docs/adr/0002-event-indexer](https://github.com/Stellar-VaultLink/invofi/blob/main/docs/adr/0002-event-indexer.md) |
| Hardening / tests / misc | #86, #89, #93, #99, #103, #106 | [security-self-review](../security-self-review.md) |

## Why ADRs

- The SCF audit bank and future reviewers can see *why* a choice was made
  without digging through commit history.
- Contributors get a decision map: e.g. "admin is a single key today, and ADR-0001
  is the record that multisig replaces it, not the pause mechanism."
- The backlog map above closes the loop: a decision record points to the
  issues that operationalize it, so the ADRs stay the single coherent index
  of the protocol's design + work queue.
