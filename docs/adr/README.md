# Architecture Decision Records — InvoFi Contracts

ADRs capture decisions with lasting consequences for the protocol. New ADRs
get the next number; append, never rewrite (status updates go in the file).

| # | Decision | Status |
|---|---|---|
| 0001 | [Emergency pause (circuit breaker)](./0001-emergency-pause.md) | Accepted |
| 0002 | [Position tokens: granularity, admin, lifecycle](./0002-position-tokens.md) | Accepted |
| 0003 | [Insurance payout on default](./0003-insurance-payout.md) | Accepted |
| 0004 | [Reputation scoring](./0004-reputation.md) | Accepted |
| 0005 | [Deployer-bound initialization (constructors)](./0005-deployer-bound-initialization.md) | Accepted |
| 0006 | [Fixed installment repayment schedules](./0006-repayment-schedules.md) | Accepted |
| 0007 | [Overdue penalty interest](./0007-overdue-penalty-interest.md) | Proposed |
| 0008 | [Offer amendment and counter-offer](./0008-offer-negotiation.md) | Proposed |
| 0009 | [Invoice verification oracle](./0009-verification-oracle.md) | Proposed |

App-layer decisions (wallet allowlist, indexer, SDK) live in the monorepo:
[invofi/docs/adr](https://github.com/Stellar-VaultLink/invofi/tree/main/docs/adr).

## Why ADRs

- The SCF audit bank and future reviewers can see *why* a choice was made
  without digging through commit history.
- Contributors get a decision map: e.g. "admin is a single key today, and ADR-0001
  is the record that multisig replaces it, not the pause mechanism."
