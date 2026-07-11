# InvoFi Contracts

Soroban smart contracts for the InvoFi decentralized invoice financing protocol.

## Overview

The `InvoiceRegistryContract` governs the full lifecycle of invoice financing on Stellar — from registration through partial repayment to overdue handling.

## Contract Functions

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, token)` | Anyone (once) | One-time setup |
| `register_invoice(...)` | Originator | Register a new Pending invoice |
| `get_invoice(id)` | Anyone | Read invoice state |
| `cancel_invoice(id, originator)` | Originator | Cancel a Pending invoice |
| `update_invoice_status(id, status)` | Originator | Manual status update |
| `create_offer(...)` | Lender | Submit financing offer (min 1 day duration) |
| `get_offer(id)` | Anyone | Read offer state |
| `get_offers_by_invoice(invoice_id)` | Anyone | All offers for an invoice |
| `get_offers_by_lender(lender)` | Anyone | All offers by a lender |
| `calculate_total_due(offer_id)` | Anyone | Remaining principal + yield |
| `accept_offer(offer_id, originator)` | Originator | Accept offer; funds business immediately |
| `reject_offer(offer_id, originator)` | Originator | Reject a pending offer |
| `repay_invoice(invoice_id, offer_id, repayer, amount)` | Originator | Partial or full repayment |
| `mark_overdue(invoice_id)` | Anyone | Mark past-due Financed invoice Overdue |
| `reclaim_invoice(invoice_id, offer_id, lender)` | Lender | Mark offer Defaulted after grace period |
| `get_invoices_by_status(status)` | Anyone | All invoices with a given status |
| `set_rate(admin, tier, rate_bps)` | Admin | Set yield rate for risk tier |
| `get_rate(tier)` | Anyone | Read yield rate for tier |
| `transfer_admin(admin, new_admin)` | Admin | Rotate admin address |
| `get_admin()` | Anyone | Read admin address |
| `get_token()` | Anyone | Read token address |

## Constants

| Constant | Value | Description |
|---|---|---|
| `GRACE_PERIOD_SECS` | 604800 | 7-day grace period before lender can reclaim |
| `MIN_OFFER_DURATION_SECS` | 86400 | Minimum offer duration (1 day) |

## Invoice Lifecycle

`Pending → Financed → Repaid`
`Pending → Cancelled`
`Financed → Overdue → (offer Defaulted)`

## Development

`bash
# Build
cargo build --target wasm32-unknown-unknown --release

# Test
cargo test

# Deploy to testnet
bash scripts/deploy.sh
`

## License

MIT