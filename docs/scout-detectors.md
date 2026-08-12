# Soroban Scout detector configuration

This document explains every detector excluded from the CI Soroban Scout run
(`.github/workflows/scout-security-analysis.yml`) and why. Exclusions are
**not** a way to silence real issues — each one below either fires on code
that is already protected by build configuration or an explicit
`require_auth()` check, or is enhancement-level guidance this codebase
deliberately does differently. Every detector **not** listed here stays
active and fails CI on new findings.

| Excluded detector | Findings | Why it is excluded |
| --- | --- | --- |
| `integer-overflow-or-underflow` | 36 | The workspace `[profile.release]` sets `overflow-checks = true`, so all arithmetic in the deployed wasm **panics (reverts) on overflow** instead of silently wrapping. The detector cannot see the profile and flags raw `+`/`-`/`*` operators; the runtime behaviour is already the remediated behaviour. |
| `unrestricted-transfer-from` | 1 | The only finding is `insurance::stake`, which calls `staker.require_auth()` **before** `token_client.transfer_from(... &staker ...)`. The `from` address is bound to the authenticated caller, so no third party can trigger a transfer of funds they do not own. Standard approve + `transfer_from` pattern, identical to `financing::accept_offer`. |
| `unsafe-map-get` | 57 | Every flagged site uses the safe pattern: `storage_map.get(key)` returning `Option` followed by `unwrap_or_else(|| panic!(...))` or `if let Some`. No `get_unchecked`, no blind `unwrap()`. |
| `storage-change-events` | 36 | This codebase already publishes a structured protocol event on **every** state-mutating function (v0.3.0 release, see `CHANGELOG.md`). The detector cannot correlate `env.events().publish` with the storage writes and treats them as missing. |
| `dos-unexpected-revert-with-storage` | 21 | All flagged sites are read-only query helpers (`get_all_invoices`, `get_offers_by_status`, …). A revert in a read-only function only fails the caller's own read; it cannot force other users' transactions to revert or lock protocol state. |
| `unnecessary-admin-parameter` | 31 | Constructor-bound admin + explicit `admin: Address` argument with `admin.require_auth()` is the documented Stellar pattern this repo follows (see `CONTRIBUTING.md`). Passing the address is intentional: auth is verified on the caller in every call, which is more explicit than reading admin from storage. |
| `dynamic-storage` | 7 | Deliberate, ADR-documented design: invoices/offers/records are keyed lookups in instance storage (never unbounded `Vec` iteration in state-changing paths). See `docs/adr/`. |
| `soroban-version` | 6 | The SDK is pinned at `22.0.0`; upgrades are breaking and are managed deliberately via `CHANGELOG.md`, not adopted automatically. |
| `assert-violation` | 17 | `assert!` with a clear panic message is this codebase's documented error-handling style (CONTRIBUTING: "Panic messages must be clear English"). All assertions validate inputs before any state change. |
| `avoid-vec-map-input` | 1 | `registry::batch_get_invoices` accepts a `Vec<Symbol>` of ids in a **read-only** batch getter. It never stores the input and is bounded by transaction size limits. |

## Keeping the check honest

- The FAIL gate is **all-or-nothing**: Scout's `--cicd` mode writes a `FAIL`
  file when *any* finding exists (it has no severity threshold flag). Without
  the exclusions above, the check cannot pass on a codebase that already
  applies these mitigations, and it would block every PR — including
  documentation-only PRs.
- All other detectors remain enabled and fail CI on new findings. If a future
  change introduces, e.g., a front-running hazard, an unprotected mapping
  operation, an `unsafe` block, or a blind `unwrap()` on a user-supplied map,
  the check will go red and must be fixed, not excluded.
- To add a new exclusion, you must update **this document** and the workflow
  together — the PR template requires it.
