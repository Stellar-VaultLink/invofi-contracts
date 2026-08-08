# ADR-0005: Deployer-Bound Initialization (Constructors)

- Status: Accepted
- Date: 2026-08-06

## Context

Every InvoFi contract shipped a public `initialize(...)` entry point that set
the admin (and cross-contract wiring). The deploy flow was: deploy the WASM,
then invoke `initialize` from the deployer. Between those two transactions
there was a **front-running window** — anyone could call `initialize` first and
become admin of a freshly deployed, uninitialized contract. This was reported
as issue #75 by an external reviewer (silasfrostx).

A secondary gap: `initialize` was not bound to the deployer in any way, and
neither CI nor the deploy scripts called it automatically, so a misconfigured
deployment could be left uninitialized indefinitely.

## Decision

Move all one-time setup into the Soroban **constructor** (`__constructor`),
which executes **atomically inside the deploy operation**. The deploy
operation can only be authorized by the deployer, so:

- The admin (and registry/financing/token wiring for financing, repayment,
  insurance, reputation) is bound the instant the contract exists.
- There is no separate `initialize` transaction to front-run.
- A fresh deployment can never be left uninitialized.

Details:

- **`initialize(...)` is removed from every contract** — registry, financing,
  repayment, insurance, reputation. Each now exposes
  `__constructor(admin[, registry, financing, token])` with the same storage
  writes and the same "Already initialized" idempotency guard (defense in
  depth; the host also only invokes the constructor during deploy).
- **The constructor does not call `require_auth` on its arguments.** The
  deployer's signature on the deploy operation *is* the authorization. (A
  constructor that required auth on an `admin` argument that differs from the
  deployer would fail at deploy time.)
- **Deploy order matters.** `financing` references the registry address,
  `repayment` references registry + financing, so contracts deploy in
  dependency order with the IDs passed as constructor args.
- **Deploy workflow updated.** `deploy-contract.yml` passes constructor args
  after `--` (`stellar contract deploy --wasm … -- --admin … --registry …`).
  The separate "Initialize contracts" step is gone. `deploy.sh` likewise.
- **Tests updated.** All 110 tests deploy via `env.register(Contract, args)`.
  Regression tests assert the admin is bound at deploy and that a post-deploy
  re-invoke of `__constructor` fails.

## Alternatives considered

- **Keep `initialize` but require the deployer's signature.** Soroban has no
  reliable "who deployed me" host accessor at this SDK level, and it would
  still leave the deploy-then-init window.
- **`initialize` guarded by admin-only auth.** Does not help — anyone can be
  the first caller on an uninitialized contract.

## Consequences

- Deployments are secure-by-construction against front-running.
- Deployment becomes order-sensitive (constructors reference earlier IDs) —
  documented in `deploy-contract.yml`.
- Re-deploying a contract with changed wiring now requires a fresh deployment
  (no `initialize` to re-run). This is acceptable: wiring is admin-settable
  afterwards (`set_financing_contract`, `set_repayment_contract`,
  `set_position_token`, `register_currency`, …).
- Live instances deployed before this ADR remain admin-bound to the deployer
  (`GBDDLOWR6…` verified on all five testnet contracts, issue #75 follow-up).

## Related issues (backlog)

- [#96 — On-chain contract_version getter for deployment verification](https://github.com/Stellar-VaultLink/invofi-contracts/issues/96) — gives
  operators a way to verify which WASM version is live, complementing the
  constructor-bound admin.
- [#104 — Docs: manual end-to-end lifecycle walkthrough with stellar-cli](https://github.com/Stellar-VaultLink/invofi-contracts/issues/104) — documents
  the order-sensitive deploy + wiring flow for fresh hands.
- [#105 — CI: byte-for-byte WASM determinism check on every build](https://github.com/Stellar-VaultLink/invofi-contracts/issues/105) — makes
  constructor-based deploys reproducible and auditable.
