# Changelog

All notable changes to InvoFi Contracts are documented here.
Versioning follows [Semantic Versioning](https://semver.org/).

## [0.3.0] – 2026-07-13

### Added
- **Protocol events** — every state-mutating function now publishes a Soroban
  contract event, enabling off-chain indexers, activity feeds, and real-time
  UI updates without polling:

  | Event topic | Emitted by | Data payload |
  |---|---|---|
  | `inv_reg`  | `register_invoice` | `(originator, amount, due_date)` |
  | `off_new`  | `create_offer` | `(invoice_id, lender, amount, interest_rate)` |
  | `off_acc`  | `accept_offer` | `(invoice_id, lender, amount)` |
  | `off_rej`  | `reject_offer` | `invoice_id` |
  | `off_wdr`  | `withdraw_offer` | `lender` |
  | `off_def`  | `reclaim_invoice` | `(invoice_id, lender)` |
  | `inv_rep`  | `repay_invoice` | `(offer_id, amount, fully_repaid)` |
  | `inv_ovd`  | `mark_overdue` | `due_date` |
  | `inv_cxl`  | `cancel_invoice` | `originator` |
  | `inv_dsp`  | `raise_dispute` | `originator` |
  | `inv_rsl`  | `resolve_dispute` | `new_status` |

  Every event carries the subject's `Symbol` id as its second topic, so
  indexers can filter by invoice or offer without decoding payloads.
- Event emission tests covering register, offer create/accept, repayment,
  and cancellation flows

### Changed
- `version()` returns `soroban_sdk::String` (was `&'static str`, which is not
  a valid Soroban return type)

## [0.2.0] – 2026-07-12

### Added
- `MIN_INVOICE_AMOUNT` constant (10 XLM) — enforced in `register_invoice`
- `MAX_OFFER_DURATION_SECS` constant (365 days) — enforced in `create_offer`
- `InvoiceStatus::Disputed` variant for on-chain dispute tracking
- `raise_dispute(invoice_id, originator)` — business can flag a Financed invoice as disputed
- `resolve_dispute(admin, invoice_id, target_status)` — admin resolves disputes
- `LenderStats` struct with per-address counters (total_offered, total_accepted, offers_pending, offers_repaid)
- `get_lender_stats(lender)` — returns the full `LenderStats` for a lender
- `get_lender_active_total(lender)` — sum of all Accepted offer amounts for a lender
- `get_invoices_count()` / `get_offers_count()` — fast total-count queries
- `get_offers_by_status(status)` — filter all offers by `OfferStatus`
- `get_invoices_by_currency(currency)` — filter all invoices by currency symbol
- `get_invoices_due_before(timestamp)` — open invoices whose `due_date` is before a timestamp
- `get_pending_offers_by_invoice(invoice_id)` — only Pending offers attached to an invoice
- `get_invoices_paginated(offset, limit)` / `get_offers_paginated(offset, limit)` — cursor-free pagination
- `batch_get_invoices(ids)` — multi-ID fetch in a single RPC call
- `get_min_invoice_amount()` — on-chain introspection for the minimum amount constant
- `get_offer_duration_limits()` — on-chain introspection for duration bounds
- `version()` — returns `CARGO_PKG_VERSION` as a static string
- Protocol statistics now increment on `register_invoice` and `create_offer`
- Admin blacklist: `blacklist_address`, `unblacklist_address`, `is_blacklisted`, `get_blacklist`
- Protocol stats: `get_stats()` returns `ProtocolStats` struct with global counters

### Changed
- `register_invoice` now rejects amounts below `MIN_INVOICE_AMOUNT`
- `create_offer` now rejects durations above `MAX_OFFER_DURATION_SECS`

## [0.1.0] – 2026-06-01

### Added
- Core invoice registry: `register_invoice`, `get_invoice`, `update_invoice_status`
- Financing offer lifecycle: `create_offer`, `accept_offer`, `reject_offer`, `withdraw_offer`
- Repayment: `repay_invoice` with partial repayment support
- Overdue and default handling: `mark_overdue`, `reclaim_invoice`
- Query helpers: `get_invoices_by_status`, `get_invoices_by_originator`, `get_offers_by_invoice`, `get_offers_by_lender`, `get_all_invoices`, `get_all_offers`, `calculate_total_due`
- Invoice management: `update_invoice_amount`, `cancel_invoice`
- Admin controls: `initialize`, `pause`, `unpause`, `transfer_admin`, `set_fee`, `get_fee`
- Yield rate oracle: `set_rate`, `get_rate` by `RiskTier` (A/B/C)
- Protocol fee deduction from repayments (configurable, max 5%)
