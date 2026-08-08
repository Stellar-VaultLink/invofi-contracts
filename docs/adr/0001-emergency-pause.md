# ADR-0001: Emergency Pause (Circuit Breaker)

- Status: Accepted
- Date: 2026-08-04

## Context

InvoFi moves real value on testnet today and will move real value on mainnet
soon. If a vulnerability or misconfiguration is discovered, the protocol must
be stoppable immediately — in the same block — without waiting for a
governance or timelock round.

## Decision

Each contract (registry, financing, repayment) stores an `is_paused` flag in
instance storage. The admin can call `pause`/`unpause` at any time. Every
state-changing function begins with `assert_not_paused`, which panics with
"Contract is paused" when the flag is set.

- **Same-block pause is intentional.** A timelock would defeat the purpose of
  an emergency brake at this stage of the protocol.
- **Admin is a single deployer key for now.** Multisig / DAO admin is future
  scope; when it lands it replaces the `assert_admin` check, not the pause
  mechanism. (See also ADR-0002.)
- **No UI.** Pause is an operational tool invoked via `stellar contract invoke`,
  not a product feature.

## Consequences

- A single point of failure can be halted immediately.
- The protocol can resume without redeployment.
- The pause key is a single point of compromise; it is rotated only via
  `transfer_admin`. This is documented and accepted for the current stage.

## Related issues (backlog)

- [#85 — Audit instance vs persistent storage placement across all crates](https://github.com/Stellar-VaultLink/invofi-contracts/issues/85) — the
  pause flag lives in instance storage; this audit reviews that placement and every other
  instance/persistent split decision.
- [#87 — Zero-amount guards across all money-moving entrypoints](https://github.com/Stellar-VaultLink/invofi-contracts/issues/87) — hardens the
  same entrypoint surface the pause protects.
- [#88 — Admin-pause test matrix covering every state-changing entrypoint](https://github.com/Stellar-VaultLink/invofi-contracts/issues/88) —
  proves every guarded function actually reverts while paused, per crate.
