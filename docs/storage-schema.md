# InvoFi Contracts — Storage Key Schema

> **Status**: living document, started by issue #110 (invoice version guard).
> The fuller schema audit from issue #46 should be merged here when it ships.
> If fields in this document conflict with the code, the code is authoritative.

---

## Overview

InvoFi's five contracts each own a disjoint slice of on-chain state stored via
the Soroban `Env::storage()` API. All keys are `Symbol` values (either
`symbol_short!` literals or compound keys). The sections below document each
contract's storage layout.

---

## Registry (`registry/`)

### Key: `invoices` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("invcs")` |
| Value type | `Map<Symbol, Invoice>` |
| Set by | `register_invoice`, all status-transition functions |
| Read by | `get_invoice`, all functions that mutate invoice state |

#### `Invoice` struct fields

| Field | Type | Description |
|---|---|---|
| `id` | `Symbol` | Unique invoice identifier, supplied by the originator at registration. |
| `originator` | `Address` | Address that registered the invoice. |
| `amount` | `i128` | Invoice face value in stroops (minimum `10_000_000` = 10 XLM). |
| `currency` | `Symbol` | Settlement currency token key (e.g. `USDC`, `XLM`). |
| `due_date` | `u64` | Unix timestamp (seconds) after which the invoice may be marked overdue. |
| `status` | `InvoiceStatus` | Current lifecycle state; see table below. |
| `version` | `u64` | **Optimistic-concurrency counter.** Starts at `0` on registration; incremented by exactly `+1` on every write that persists the invoice back to storage (status transitions, amount updates, dispute lifecycle). Callers must supply the version they read as `expected_version` to `accept_offer` and `repay_invoice`. See [ADR-0008](./adr/0008-invoice-version-guard.md). |

> **`version` field added in issue #110 (2026-08-18).** Any serialised
> `Invoice` value stored before this change will be missing the field; a
> migration that re-registers all invoices with `version: 0` is required
> before upgrade. See [migration-runbook.md](./migration-runbook.md).

#### `InvoiceStatus` enum variants

| Variant | Discriminant | Meaning |
|---|---|---|
| `Pending` | 0 | Registered, awaiting an accepted offer. |
| `Financed` | 1 | An offer has been accepted; repayment is in progress. |
| `Repaid` | 2 | Fully repaid. |
| `Cancelled` | 3 | Cancelled by the originator while still Pending. |
| `Overdue` | 4 | Past `due_date` and marked by `mark_overdue`. |
| `Defaulted` | 5 | Lender reclaimed after the grace period. |
| `Disputed` | 6 | Dispute raised by the originator. |

### Key: `fin_addr` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("fin_addr")` |
| Value type | `Address` |
| Set by | `set_financing_contract` |
| Read by | `financing_marks_invoice_financed` (auth check) |

### Key: `rep_addr` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("rep_addr")` |
| Value type | `Address` |
| Set by | `set_repayment_contract` |
| Read by | `repayment_marks_invoice_repaid`, `mark_invoice_overdue`, `marks_invoice_defaulted` (auth checks) |

### Key: `admin` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("admin")` |
| Value type | `Address` |
| Set by | `__constructor`, `transfer_admin` |
| Read by | All admin-gated functions |

---

## Financing (`financing/`)

### Key: `offers` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("offers")` |
| Value type | `Map<Symbol, FinancingOffer>` |
| Set by | `create_offer`, `accept_offer`, `withdraw_offer`, `reject_offer`, all `update_offer_*` callbacks |
| Read by | `get_offer`, all offer-mutating functions |

> Note: Offer state is **not** currently version-guarded (issues #77/#79 cover
> offer-level concurrency separately).

### Key: `currencies` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("currs")` |
| Value type | `Map<Symbol, Address>` |
| Set by | `register_currency` |
| Read by | `resolve_token` (common helper) |

---

## Repayment (`repayment/`)

### Key: `registry` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("registry")` |
| Value type | `Address` |
| Set by | `__constructor` |

### Key: `financing` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("financing")` |
| Value type | `Address` |
| Set by | `__constructor` |

### Key: `penalty` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("penalty")` |
| Value type | `(u32, u32)` — `(rate_bps, cap_bps)` |
| Set by | `set_penalty` |
| Read by | `calculate_penalty`, `repay_invoice` |

---

## Insurance (`insurance/`)

### Key: `stakes` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("stakes")` |
| Value type | `Map<Address, i128>` |
| Set by | `stake`, `unstake` |
| Read by | `get_stake`, `unstake` |

### Key: `pool_total` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("ptotal")` |
| Value type | `i128` |
| Set by | `stake`, `unstake`, `pay_out` |
| Read by | `get_pool_total`, `pay_out` |

---

## Reputation (`reputation/`)

### Key: `outcomes` (instance storage)

| Attribute | Value |
|---|---|
| Storage type | Instance |
| Key | `Symbol("outcs")` |
| Value type | `Map<Address, OutcomeRecord>` |
| Set by | `record_outcome` |
| Read by | `get_score`, `get_record` |

---

## Cross-cutting notes

- **Soroban storage types**: all contracts use **instance** storage for their
  primary maps. Temporary and persistent storage are not used in this version.
- **Storage limits**: Soroban charges rent on persistent and instance entries;
  large maps (many invoices or offers) will increase ledger-entry fees. A
  per-entry storage design (one entry per invoice) is a future optimisation
  tracked under issue #46.
- **Upgrade / migration**: adding a new field to a stored struct (such as
  `version` in this release) requires a migration pass to rewrite all
  existing entries. The `migration-runbook.md` documents the procedure.
