```
 ██╗███╗   ██╗██╗   ██╗ ██████╗ ███████╗██╗
 ██║████╗  ██║██║   ██║██╔═══██╗██╔════╝██║
 ██║██╔██╗ ██║██║   ██║██║   ██║█████╗  ██║
 ██║██║╚██╗██║╚██╗ ██╔╝██║   ██║██╔══╝  ██║
 ██║██║ ╚████║ ╚████╔╝ ╚██████╔╝██║     ██║
 ╚═╝╚═╝  ╚═══╝  ╚═══╝   ╚═════╝ ╚═╝     ╚═╝
```

<div align="center">

**Soroban smart contracts for the InvoFi decentralised invoice financing protocol**

[![CI](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/ci.yml/badge.svg)](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Built on Stellar](https://img.shields.io/badge/Built%20on-Stellar-7B4FE2)](https://stellar.org)
[![Soroban](https://img.shields.io/badge/Smart%20Contracts-Soroban-FF5B36)](https://soroban.stellar.org)
[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange)](https://rustup.rs)

**Live contract (testnet):** `CDJS6AFE6VRPAPWOPWOPZLSLQ7NCISA7YHOMAE7HJWOD7G6CQDCVT4L2` · **Frontend:** [invofi-frontend](https://github.com/Stellar-VaultLink/invofi-frontend) · **Monorepo:** [invofi](https://github.com/Stellar-VaultLink/invofi)

</div>

---

## This repo vs. the monorepo

This is where **contract contributions happen** — fork it and open PRs here for anything touching Soroban / Rust. It has its own CI (`cargo test` + `stellar contract build`) and issue queue scoped to the contracts.

Production runs from **[Stellar-VaultLink/invofi](https://github.com/Stellar-VaultLink/invofi)**, the integration monorepo. Merged PRs here are periodically pulled in and deployed from there.

---

## Contract: `invofi-invoice-registry`

The core protocol contract. Handles the full invoice financing lifecycle:

- Register invoices on-chain with validation (`amount > 0`, `due_date > now`)
- Submit competing financing offers (`interest_rate > 0`, `offer amount > 0`)
- Accept an offer — pulls the lender's approved principal via `token.approve` and pays it directly to the business
- **Partial or full repayment** — `amount_repaid` tracks progress; the offer stays `Financed` until `amount_repaid >= principal + yield`, then flips to `Repaid`
- Mark overdue, reclaim after 7-day grace period (produces an on-chain default record)
- Admin-configured yield rates per risk tier (A / B / C in basis points)
- `get_invoices_by_status` — server-side filter returns all invoices matching a given `InvoiceStatus`

Requires a one-time `initialize(admin, token)` call after deployment. There is no collateral custody — if a business never repays, `reclaim_invoice` only produces a default record on-chain.

---

## Data Types

### `Invoice`

| Field | Type | Description |
|---|---|---|
| `id` | `Symbol` | Unique invoice identifier |
| `originator` | `Address` | Stellar address of the business |
| `amount` | `i128` | Invoice amount in stroops |
| `currency` | `Symbol` | `XLM` or `USDC` |
| `due_date` | `u64` | Unix timestamp of payment due date |
| `status` | `InvoiceStatus` | `Pending → Financed → Repaid / Overdue / Cancelled` |

### `FinancingOffer`

| Field | Type | Description |
|---|---|---|
| `id` | `Symbol` | Unique offer identifier |
| `invoice_id` | `Symbol` | Invoice this offer targets |
| `lender` | `Address` | Stellar address of the investor |
| `amount` | `i128` | Principal amount in stroops |
| `currency` | `Symbol` | `XLM` or `USDC` |
| `interest_rate` | `u32` | Basis points (500 = 5.00%) |
| `duration` | `u64` | Financing duration in seconds |
| `amount_repaid` | `i128` | Running total of stroops repaid — starts at 0 on acceptance |
| `status` | `OfferStatus` | `Pending → Accepted → Financed → Repaid / Rejected / Defaulted` |
| `funded_at` | `u64` | Unix timestamp when offer was accepted |

---

## Function Reference

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, token)` | Anyone (once) | One-time setup — sets admin and SEP-41 token; panics if called again |
| `register_invoice(id, originator, amount, currency, due_date)` | Originator | Register invoice; asserts `amount > 0` and `due_date > now` |
| `get_invoice(id)` | Anyone | Read invoice state; panics if not found |
| `update_invoice_status(id, status)` | Originator | Change invoice status |
| `create_offer(offer_id, invoice_id, lender, amount, currency, rate, duration)` | Lender | Submit offer; asserts `amount > 0` and `rate > 0` |
| `get_offer(id)` | Anyone | Read offer state; panics if not found |
| `accept_offer(offer_id, originator)` | Business | Accept offer — transfers principal to business; invoice → Financed |
| `reject_offer(offer_id, originator)` | Business | Reject pending offer |
| `repay_invoice(invoice_id, offer_id, repayer, amount)` | Business | Pay `amount` toward balance; stays Financed until fully cleared, then → Repaid |
| `mark_overdue(invoice_id)` | Anyone | Mark a past-due Financed invoice Overdue |
| `reclaim_invoice(invoice_id, offer_id, lender)` | Lender | After 7-day grace period, marks offer Defaulted |
| `get_invoices_by_status(status)` | Anyone | Return `Vec<Invoice>` filtered by `InvoiceStatus` |
| `set_rate(admin, tier, rate_bps)` | Admin | Set yield rate for risk tier A/B/C (0–10000 bps) |
| `get_rate(tier)` | Anyone | Read yield rate for a risk tier; panics if not set |
| `transfer_admin(admin, new_admin)` | Admin | Rotate admin address |
| `get_admin()` | Anyone | Read admin address |
| `get_token()` | Anyone | Read SEP-41 token address |

### Partial Repayment Flow

```text
accept_offer()              offer.amount_repaid = 0, invoice → Financed
      │
repay_invoice(amount=X)     token.transfer(repayer → lender, X)
      │                      offer.amount_repaid += X
      │
      ├─ amount_repaid < total_due  →  invoice stays Financed, further repayments allowed
      │
      └─ amount_repaid >= total_due →  invoice → Repaid, offer → Repaid
```

`total_due = offer.amount + (offer.amount * offer.interest_rate / 10_000)`

---

## Prerequisites

- [Rust 1.70+](https://rustup.rs)
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- [Stellar CLI](https://github.com/stellar/stellar-cli): `cargo install --locked stellar-cli`

---

## Quick Start

```bash
git clone https://github.com/Stellar-VaultLink/invofi-contracts.git
cd invofi-contracts
cargo test
# → 21+ tests pass
```

## Build

```bash
stellar contract build
# → target/wasm32v1-none/release/invofi_invoice_registry.wasm
```

## Deploy to Testnet

```bash
stellar keys generate --global invofi-deployer --network testnet
stellar keys fund invofi-deployer --network testnet

stellar contract deploy \
  --wasm target/wasm32v1-none/release/invofi_invoice_registry.wasm \
  --source invofi-deployer \
  --network testnet
# Save CONTRACT_ID

# Initialize — must run once before any other function
stellar contract invoke \
  --id <CONTRACT_ID> --source invofi-deployer --network testnet \
  -- initialize --admin <ADMIN_ADDRESS> --token <SEP41_TOKEN>
```

Or use the helper scripts:

```bash
bash scripts/fund-and-deploy.sh   # generate + fund deployer key, build, deploy
bash scripts/deploy.sh            # build + deploy with an existing key
```

---

## Tests

```bash
cargo test -- --nocapture
```

| Test | Verifies |
|---|---|
| `test_register_and_get_invoice` | Invoice creation and retrieval |
| `test_duplicate_invoice_id_panics` | Duplicate ID rejection |
| `test_get_non_existent_invoice` | Not-found panic |
| `test_update_invoice_status` | Status mutation |
| `test_create_and_get_offer` | Offer creation and retrieval |
| `test_accept_offer` | Offer acceptance + principal transferred to business |
| `test_reject_offer` | Offer rejection, invoice stays Pending |
| `test_repay_invoice` | Full repayment — principal + yield transferred to lender |
| `test_partial_repayment` | Partial payment — offer stays Financed, `amount_repaid` updated |
| `test_full_repayment_via_partials` | Two partial payments clear the balance → Repaid |
| `test_repay_unfinanced_invoice_panics` | Guard against premature repayment |
| `test_initialize_twice_panics` | `initialize()` can only be called once |
| `test_reclaim_invoice_after_grace_period` | Reclaim marks offer Defaulted after grace period |
| `test_reclaim_before_grace_period_panics` | Reclaim rejected before grace period |
| `test_set_and_get_rate` | Yield rate set/read for all three risk tiers |
| `test_set_rate_out_of_range_panics` | Rate validated to 0–10000 bps |
| `test_set_rate_unauthorized_panics` | Only admin can set rates |
| `test_get_unset_rate_panics` | Reading an unconfigured tier panics |
| `test_transfer_admin` | Admin rotation, new admin can act |
| `test_transfer_admin_unauthorized_panics` | Only the current admin can rotate |
| `test_register_invoice_zero_amount` | `amount = 0` panics |
| `test_register_invoice_past_due_date` | Past `due_date` panics |
| `test_create_offer_zero_amount` | `offer amount = 0` panics |
| `test_get_invoices_by_status_empty` | Empty result when no matches |
| `test_get_invoices_by_status_matching` | Correct subset returned |

---

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). Open issues and PRs in this repo for anything contract-scoped.

## License

MIT © 2026 InvoFi Contributors
