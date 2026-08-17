# SDKT Static Analysis Audit Triage

This document documents the triage and verification of findings reported by the `sdkt audit` static analyzer (Issue #155).

## Overview

A static-analysis audit run using `sdkt audit` identified potential `AUTH-001` (unauthenticated privileged function) and `MOVE-001` (environment handle reuse) findings across contract crates (`common`, `registry`, `financing`, `repayment`, `insurance`, `reputation`).

A complete manual audit of the codebase confirmed that all state-changing entrypoints enforce authorization and that the reported findings are false positives due to linter AST scanner rules.

## Triage Summary

| Finding Type | Crates Flagged | Root Cause & Resolution |
| --- | --- | --- |
| `AUTH-001` (Delegated Admin Auth) | `registry` | `transfer_admin`, `pause`, `unpause`, `set_rate`, `set_fee`, `set_financing_contract`, `set_repayment_contract`, `resolve_dispute`, `blacklist_address`, `unblacklist_address` delegate caller authentication to the internal helper `assert_admin(&env, &admin)`. `assert_admin` executes `caller.require_auth()` and checks `current_admin == caller`. The linter does not trace cross-function auth delegation. |
| `AUTH-001` (Read-Only Getters) | `common`, `registry`, `financing`, `repayment`, `insurance`, `reputation` | Query getters (`get_admin`, `contract_is_paused`, `get_rate`, `get_fee`, `get_position_token`, `get_insurance`, `get_reputation`, `get_payout_caller`, `get_registry`, `get_recorder`, `get_score`, `get_record`) and internal module helpers (`assert_not_paused`) were flagged by keyword matching (`admin`, `paused`). Read-only getters do not mutate state or move funds and must remain unauthenticated for public on-chain inspection. |
| `MOVE-001` (Env Reuse Warnings) | All contract crates | `sdkt audit` flags passing `&Env` across function calls. In Soroban SDK, `Env` is an environment context reference designed to be passed by reference (`&Env`) to storage helpers, clients, and internal functions. |

## Conclusion & Verification

- All state-mutating operations strictly enforce `require_auth()` (either directly or via `assert_admin`).
- Cross-contract state updates enforce contract-invoker address authorization.
- Test coverage across all contract crates remains 100% green.
