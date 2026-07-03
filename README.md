# InvoFi — Contracts

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Built on Stellar](https://img.shields.io/badge/Built%20on-Stellar-7B4FE2)](https://stellar.org)
[![Soroban](https://img.shields.io/badge/Smart%20Contracts-Soroban-FF5B36)](https://soroban.stellar.org)

Soroban smart contracts for [InvoFi](https://invofi-five.vercel.app) — a decentralized invoice financing protocol on Stellar.

**Frontend:** [invofi-frontend](https://github.com/Stellar-VaultLink/invofi-frontend)
**Live contract:** `CDJS6AFE6VRPAPWOPWOPZLSLQ7NCISA7YHOMAE7HJWOD7G6CQDCVT4L2` (testnet)

---

## This repo vs. the main repo

This is where **contract contributions happen** — fork it, open a PR here for anything touching Soroban/Rust code. It has its own CI and issue queue, scoped to just the contract.

Production runs out of **[Stellar-VaultLink/invofi](https://github.com/Stellar-VaultLink/invofi)**, the integration monorepo that combines this contract with [invofi-frontend](https://github.com/Stellar-VaultLink/invofi-frontend) and is what Vercel actually deploys. Merged PRs here get pulled into that repo periodically. If you're looking for the full project (roadmap, deployed demo, both stacks together), start there instead.

---

## Contracts

### `invofi-invoice-registry`
The core protocol contract. Handles the full invoice financing lifecycle:
- Register invoices on-chain
- Accept and reject financing offers
- Repay invoices and mark overdue

### `invofi-core`
Shared data types and storage helpers used by the registry contract.

---

## Prerequisites

- [Rust](https://rustup.rs) 1.70+
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- [Stellar CLI](https://github.com/stellar/stellar-cli): `cargo install --locked stellar-cli`

## Quick Start

```bash
git clone https://github.com/Stellar-VaultLink/invofi-contracts.git
cd invofi-contracts
cargo test
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
```

Or use the scripts in `scripts/`:

```bash
bash scripts/fund-and-deploy.sh   # generate + fund a deployer key, then build + deploy
bash scripts/deploy.sh            # build + deploy with an existing key
```

## Test

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
| `test_accept_offer` | Offer acceptance + invoice state change |
| `test_reject_offer` | Offer rejection |
| `test_repay_invoice` | Full repayment flow |
| `test_repay_unfinanced_invoice_panics` | Guard against premature repayment |

## License

MIT © 2026 InvoFi Contributors
