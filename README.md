# InvoFi Contracts

Soroban smart contracts for the [InvoFi](https://github.com/Stellar-VaultLink/invofi) decentralised invoice financing protocol, built with Rust + Soroban SDK 22 on Stellar.

[![CI](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/ci.yml/badge.svg)](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/ci.yml)
[![Clippy](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/clippy.yml/badge.svg)](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/clippy.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

---

## Overview

`InvoiceRegistryContract` governs the full lifecycle of invoice financing on Stellar — from registration through partial repayment, dispute resolution, and overdue handling. All state lives on-chain; no intermediary holds funds.

```
register_invoice()  →  create_offer()  →  accept_offer()
       ↓                                        ↓
  [Pending]                              funds to business
       ↓                                        ↓
  reject_offer()                         [Financed]
  stays Pending                               ↓
                             repay_invoice() (partial or full)
                                               ↓
                                   [Repaid] ← balance cleared
                                   [Overdue] ← mark_overdue()
                                               ↓
                         reclaim_invoice() (after 7-day grace)
                                    offer → [Defaulted]

                        raise_dispute() → [Disputed]
                        resolve_dispute() → [Financed] or [Cancelled]
```

---

## Contract Functions

### Core

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, token)` | Anyone (once) | One-time setup — sets admin and SEP-41 token |
| `register_invoice(id, originator, amount, currency, due_date)` | Originator | Register invoice; rejects if `amount < MIN_INVOICE_AMOUNT` or `due_date <= now` |
| `get_invoice(id)` | Anyone | Read invoice state |
| `cancel_invoice(id, originator)` | Originator | Cancel a Pending invoice |
| `update_invoice_status(id, status)` | Originator | Manual status override |
| `create_offer(offer_id, invoice_id, lender, amount, currency, rate, duration)` | Lender | Submit offer; validates amount, rate, `duration <= MAX_OFFER_DURATION_SECS` |
| `get_offer(id)` | Anyone | Read offer state |
| `accept_offer(offer_id, originator)` | Originator | Accept offer — transfers principal to business; invoice → Financed |
| `reject_offer(offer_id, originator)` | Originator | Reject a pending offer |
| `repay_invoice(invoice_id, offer_id, repayer, amount)` | Originator | Partial or full repayment; offer → Repaid when balance cleared |
| `mark_overdue(invoice_id)` | Anyone | Mark a past-due Financed invoice Overdue |
| `reclaim_invoice(invoice_id, offer_id, lender)` | Lender | After 7-day grace period, marks offer Defaulted |
| `calculate_total_due(offer_id)` | Anyone | Remaining principal + accrued yield |

### Query Helpers

| Function | Auth | Description |
|---|---|---|
| `get_invoices_by_status(status)` | Anyone | All invoices with a given status |
| `get_invoices_by_currency(currency)` | Anyone | Invoices denominated in a given asset symbol |
| `get_invoices_due_before(timestamp)` | Anyone | Open invoices with `due_date < timestamp` |
| `get_invoices_count()` | Anyone | Total invoices ever registered |
| `get_invoices_paginated(offset, limit)` | Anyone | Page through all invoices |
| `batch_get_invoices(ids)` | Anyone | Fetch multiple invoices by ID; skips missing IDs |
| `get_offers_by_invoice(invoice_id)` | Anyone | All offers for an invoice |
| `get_offers_by_lender(lender)` | Anyone | All offers by a lender |
| `get_offers_by_status(status)` | Anyone | All offers matching a status |
| `get_pending_offers_by_invoice(invoice_id)` | Anyone | Only Pending offers on an invoice |
| `get_offers_count()` | Anyone | Total offers ever created |
| `get_offers_paginated(offset, limit)` | Anyone | Page through all offers |

### Lender Analytics

| Function | Auth | Description |
|---|---|---|
| `get_lender_stats(lender)` | Anyone | `LenderStats` — total_offered, total_accepted, offers_pending, offers_repaid |
| `get_lender_active_total(lender)` | Anyone | Sum of amounts across all Accepted offers for a lender |

### Dispute Resolution

| Function | Auth | Description |
|---|---|---|
| `raise_dispute(invoice_id, originator)` | Originator | Mark a Financed invoice Disputed |
| `resolve_dispute(admin, invoice_id, target_status)` | Admin | Resolve Disputed invoice to Financed or Cancelled |

### Protocol Stats

| Function | Auth | Description |
|---|---|---|
| `get_stats()` | Anyone | `ProtocolStats` — total_invoices, total_offers, total_financed, total_repaid, total_fee_revenue |

### Admin Controls

| Function | Auth | Description |
|---|---|---|
| `set_rate(admin, tier, rate_bps)` | Admin | Set yield rate for risk tier A/B/C |
| `get_rate(tier)` | Anyone | Read yield rate for tier |
| `set_fee(admin, fee_bps)` | Admin | Set protocol fee (max 500 bps = 5%) |
| `get_fee()` | Anyone | Read current fee |
| `transfer_admin(admin, new_admin)` | Admin | Rotate admin address |
| `get_admin()` | Anyone | Read admin address |
| `get_token()` | Anyone | Read SEP-41 token address |
| `pause(admin)` | Admin | Halt all state-mutating operations |
| `unpause(admin)` | Admin | Resume normal operation |
| `contract_is_paused()` | Anyone | Query pause state |
| `blacklist_address(admin, target)` | Admin | Block address from registering invoices/offers |
| `unblacklist_address(admin, target)` | Admin | Remove block |
| `is_blacklisted(address)` | Anyone | Query blacklist |
| `get_blacklist()` | Anyone | Return full blacklist |

### Introspection

| Function | Auth | Description |
|---|---|---|
| `version()` | Anyone | Contract semver string |
| `get_min_invoice_amount()` | Anyone | `MIN_INVOICE_AMOUNT` constant |
| `get_offer_duration_limits()` | Anyone | `(MIN_OFFER_DURATION_SECS, MAX_OFFER_DURATION_SECS)` |

---

## Constants

| Constant | Value | Description |
|---|---|---|
| `GRACE_PERIOD_SECS` | 604,800 | 7-day grace period before lender can reclaim |
| `MIN_OFFER_DURATION_SECS` | 86,400 | Minimum offer duration (1 day) |
| `MAX_OFFER_DURATION_SECS` | 31,536,000 | Maximum offer duration (1 year) |
| `MIN_INVOICE_AMOUNT` | 10,000,000 | Minimum invoice amount in stroops (10 XLM / 10 USDC) |

---

## Development

```bash
# Build
cargo build --target wasm32v1-none --release

# Run tests (30+ tests)
cargo test

# Check WASM size stays under 256 KB
bash scripts/check-size.sh

# Deploy to Testnet (requires stellar-cli)
bash scripts/deploy.sh
```

Or trigger the **[Deploy Contract](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/deploy-contract.yml)** GitHub Actions workflow for a one-click Testnet deploy.

---

## Changelog

See [CHANGELOG.md](./CHANGELOG.md) for version history.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for build, test, and PR guidelines.

## License

MIT © 2026 InvoFi Contributors
