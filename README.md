# InvoFi Contracts

Soroban smart contracts for the [InvoFi](https://github.com/Stellar-VaultLink/invofi) decentralised invoice financing protocol, built with Rust + Soroban SDK 22 on Stellar.

[![CI](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/ci.yml/badge.svg)](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/ci.yml)
[![Clippy](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/clippy.yml/badge.svg)](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/clippy.yml)
[![Scout](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/scout-security-analysis.yml/badge.svg)](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/scout-security-analysis.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

---

## Project Map

InvoFi is split across two repositories so the fast-moving app layer and the slow-moving, audit-bound contract layer stay decoupled:

| Repo | Contains | Why separate |
|---|---|---|
| [**invofi**](https://github.com/Stellar-VaultLink/invofi) | Next.js frontend (`apps/frontend`), SDK, docs, scripts | App-layer changes constantly, Node/npm CI, no audit dependency |
| **invofi-contracts** (this repo) | All Soroban Rust contracts — registry, financing, repayment, insurance, reputation, common | Stable, auditable, slow-moving history; Rust-only CI; the repo that goes through the SCF Audit Bank |

Smart-contract contributions happen here; frontend and SDK contributions happen in [invofi](https://github.com/Stellar-VaultLink/invofi).

---

## Overview

InvoFi's protocol state is spread across **five** auditable Soroban contracts (plus the SEP-41
position token), each with a narrow job:

| Contract | Crate | Responsibility |
|---|---|---|
| **registry** | `registry/` | Invoice lifecycle — register, cancel, status transitions, blacklist, disputes |
| **financing** | `financing/` | Offers — create, withdraw, accept (moves principal **and mints the position token**), reject |
| **repayment** | `repayment/` | Repayments (full/partial), mark overdue, reclaim/default |
| **insurance** | `insurance/` | Coverage reserve — stake/unstake, and **payout on default** capped at pool balance (Task 10) |
| **reputation** | `reputation/` | Originator credit history — repayment outcomes → public score (Task 11) |

Cross-contract calls are restricted: the registry only accepts status transitions from the
registered financing/repayment contracts (implicit contract-invoker auth, per Stellar's
Authorization docs), and financing only accepts repayment callbacks from the registered
repayment contract. User auth never propagates across contract boundaries.

```
register_invoice()  →  create_offer()  →  accept_offer()
       ↓                                        ↓
  [Pending]                 funds to business + POS minted to lender (Task 7)
       ↓                                        ↓
  reject_offer()                            [Financed]
  stays Pending                                 ↓
                             repay_invoice() (partial or full)
                                                 ↓
                                     [Repaid] ← balance cleared
                                     [Overdue] ← mark_overdue()
                                                 ↓
                           reclaim_invoice() (after 7-day grace)
                                      offer → [Defaulted]
```

---

## Contract Functions

### Registry — `registry/`

| Function | Auth | Description |
|---|---|---|
| `initialize(admin)` | Admin (once) | One-time setup |
| `register_invoice(id, originator, amount, currency, due_date)` | Originator | Register invoice; rejects dust (< 10 XLM) and past-due dates |
| `get_invoice(id)` | Anyone | Read invoice state |
| `cancel_invoice(id, originator)` | Originator | Cancel a Pending invoice |
| `set_financing_contract(admin, addr)` / `set_repayment_contract(admin, addr)` | Admin | Authorize the only cross-contract callers |
| `financing_marks_invoice_financed(id)` | financing | System transition: Pending → Financed |
| `repayment_marks_invoice_repaid(id, fully_repaid)` | repayment | System transition: Financed → Financed/Repaid |
| `mark_invoice_overdue(id)` | Anyone | Overdue once due_date passes |
| `raise_dispute / resolve_dispute` | Originator / Admin | Dispute lifecycle |
| `blacklist_address / unblacklist_address / is_blacklisted` | Admin | Address blocking |
| `set_rate / set_fee / transfer_admin / pause / unpause` | Admin | Admin controls |

### Financing — `financing/`

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, registry, token)` | Admin (once) | Wire to registry + default settlement token |
| `create_offer(offer_id, invoice_id, lender, amount, currency, rate, duration)` | Lender | Submit an offer (validates amount/rate/duration bounds) |
| `withdraw_offer / reject_offer` | Lender / Originator | Withdraw or reject a Pending offer |
| `accept_offer(offer_id, originator)` | Originator | Pulls principal lender → business **and mints the lender's position token** (Task 7) |
| `register_currency(admin, currency, token)` | Admin | Add a settlement currency — one registry entry, no code branch per currency |
| `set_position_token(admin, token)` | Admin | Configure the SEP-41 position-token contract (ADR-0002) |
| `get_position_token()` | Anyone | Read the configured position token |
| `update_offer_status / update_offer_amount_repaid / update_lender_stats_repaid / update_stats_repaid` | repayment | Repayment callbacks (registered caller only) |
| `pause / unpause / transfer_admin` | Admin | Admin controls |

### Repayment — `repayment/`

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, registry, financing, token)` | Admin (once) | Wire to registry + financing |
| `repay_invoice(invoice_id, offer_id, repayer, amount)` | Originator | Full or partial repayment (principal + yield) |
| `mark_overdue(invoice_id)` | Anyone | Flag a past-due Financed invoice |
| `reclaim_invoice(invoice_id, offer_id, lender)` | Lender | After the 7-day grace period → offer Defaulted |
| `calculate_total_due(offer_id)` | Anyone | Principal + accrued yield |

### Insurance — `insurance/` (Task 9)

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, token)` | Admin (once) | Set admin + staking token |
| `stake(staker, amount)` | Staker | Deposit the staking token into the pool (approve + pull pattern) |
| `unstake(staker, amount)` | Staker | Withdraw; the pool pays back directly |
| `get_stake(staker)` | Anyone | Staker's balance |
| `get_pool_total()` | Anyone | Accounting total of staked funds |
| `get_stakers_count()` | Anyone | Number of active stakers |
| `get_contract_token_balance()` | Anyone | Actual on-chain balance — audit check that accounting matches |
| `pay_out(beneficiary, amount)` | Payout caller only (repayment) | Pay up to `amount`, capped at pool balance; returns amount actually paid (Task 10) |
| `get_payout_caller()` | Anyone | Read the configured payout caller |
| `set_staking_token / pause / unpause / transfer_admin` | Admin | Admin controls |

> Yield-rate calculation remains intentionally out of scope — the pool is flat accounting with
> payout-on-default wired through `pay_out` (Task 10). See ADR-0003 for the payout design.

---

## Position Tokens (Tasks 7 & 8)

On `accept_offer`, the financing contract mints a **SEP-41 position token** to the lender, 1:1
with the offer amount (one token per base unit of principal — see [ADR-0002](./docs/adr/0002-position-tokens.md)).
The token is a **Stellar Asset Contract** (`POS`, issued by the protocol deployer) whose admin is
the financing contract, so minting is authorized via implicit contract-invoker auth.

Position tokens are plain SEP-41 assets: any wallet can hold and transfer them (Task 8), and
they represent the lender's claim on the financed invoice until it is repaid. Because they are
Stellar assets, a holder must establish a `POS` trustline before mint/transfer can credit them —
the frontend's portfolio offers a one-click trustline helper.

---

### Reputation — `reputation/` (Task 11)

| Function | Auth | Description |
|---|---|---|
| `initialize(admin)` | Admin (once) | One-time setup |
| `set_recorder(admin, recorder)` | Admin | Set the repayment contract as the only writer |
| `record_outcome(originator, outcome)` | Recorder only | `0` = repaid, `1` = defaulted; updates outcome counts |
| `get_score(originator)` | Anyone | `repayments − 2×defaults`, floored at 0 (ADR-0004) |
| `get_record(originator)` | Anyone | Raw `{repayments, defaults}` counts — the source of truth |

## Protocol Events

Every state-mutating function publishes a Soroban contract event. Topics are
`(event_name, subject_id)` — indexers can filter by invoice or offer id without decoding payloads.

| Event | Emitted by | Data payload |
|---|---|---|
| `inv_reg` | `register_invoice` | `(originator, amount, due_date)` |
| `off_new` | `create_offer` | `(invoice_id, lender, amount, interest_rate)` |
| `off_acc` | `accept_offer` | `(invoice_id, lender, amount)` |
| `off_rej` | `reject_offer` | `invoice_id` |
| `off_wdr` | `withdraw_offer` | `lender` |
| `off_def` | `reclaim_invoice` | `(invoice_id, lender)` |
| `inv_rep` | `repay_invoice` | `(offer_id, amount, fully_repaid)` |
| `inv_ovd` | `mark_overdue` | `due_date` |
| `inv_cxl` | `cancel_invoice` | `originator` |
| `inv_dsp` | `raise_dispute` | `originator` |
| `inv_rsl` | `resolve_dispute` | `new_status` |
| `pos_mint` | `accept_offer` (financing) | `(lender, amount)` — position token minted |
| `pool_stk` | `stake` (insurance) | `amount` |
| `pool_un` | `unstake` (insurance) | `amount` |
| `pool_pay` | `pay_out` (insurance, Task 10) | `amount paid` |
| `reputn` | `record_outcome` (reputation, Task 11) | `outcome` |

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

# Run tests (96 tests across registry / financing / repayment / insurance)
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

## Maintainers

- [@samjay8](https://github.com/samjay8) — project maintainer and protocol owner

## Contributors

Thanks to everyone who has contributed to InvoFi — the list below is generated automatically from the GitHub API whenever code lands on `master`. No action needed on your part after a merged PR.

<!-- readme: contributors -start -->
<table>
	<tbody>
		<tr>
            <td align="center">
                <a href="https://github.com/samjay8">
                    <img src="https://avatars.githubusercontent.com/u/197444055?v=4" width="100;" alt="samjay8"/>
                    <br />
                    <sub><b>Samuel Ojetunde</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/hexlaapp">
                    <img src="https://avatars.githubusercontent.com/u/287440938?v=4" width="100;" alt="hexlaapp"/>
                    <br />
                    <sub><b>hexlaapp</b></sub>
                </a>
            </td>
		</tr>
	<tbody>
</table>
<!-- readme: contributors -end -->

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for build, test, and PR guidelines.

## License

MIT © 2026 InvoFi Contributors
