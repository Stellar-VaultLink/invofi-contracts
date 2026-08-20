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

Smart-contract contributions happen here; frontend and SDK contributions happen in [invofi](https://github.com/Stellar-VaultLink/invofi). The frontend supports **Freighter and LOBSTR** wallets via an approved-wallet allowlist (`approved-wallets.ts`) — approving a third wallet is a one-line config change.

---

## Overview

InvoFi's protocol state is spread across **five** auditable Soroban contracts (plus the SEP-41
position token), each with a narrow job:

| Contract | Crate | Responsibility |
|---|---|---|
| **registry** | `registry/` | Invoice lifecycle — register, cancel, status transitions, blacklist, disputes |
| **financing** | `financing/` | Offers — create, withdraw, accept (moves principal **and mints the position token**), reject |
| **repayment** | `repayment/` | Repayments (full/partial), mark overdue, reclaim/default |
| **insurance** | `insurance/` | Coverage reserve — stake/unstake, and **payout on default** capped at pool balance |
| **reputation** | `reputation/` | Originator credit history — repayment outcomes → public score |

Cross-contract calls are restricted: the registry only accepts status transitions from the
registered financing/repayment contracts (implicit contract-invoker auth, per Stellar's
Authorization docs), and financing only accepts repayment callbacks from the registered
repayment contract. User auth never propagates across contract boundaries.

```
register_invoice()  →  create_offer()  →  accept_offer()
       ↓                                        ↓
  [Pending]                 funds to business + POS minted to lender
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
| `__constructor(admin)` | Deployer (at deploy) | Sets admin atomically in the deploy operation — no `initialize()` to front-run (ADR-0005) |
| `register_invoice(id, originator, amount, currency, due_date)` | Originator | Register invoice; rejects dust (< 10 XLM) and past-due dates |
| `get_invoice(id)` | Anyone | Read invoice state |
| `cancel_invoice(id, originator)` | Originator | Cancel a Pending invoice |
| `set_financing_contract(admin, addr)` / `set_repayment_contract(admin, addr)` | Admin | Authorize the only cross-contract callers |
| `financing_marks_invoice_financed(id)` | financing | System transition: Pending → Financed |
| `repayment_marks_invoice_repaid(id, fully_repaid)` | repayment | System transition: Financed → Financed/Repaid |
| `mark_invoice_overdue(id)` | Anyone | Overdue once due_date passes |
| `raise_dispute / resolve_dispute` | Originator / Admin | Dispute lifecycle |
| `blacklist_address / unblacklist_address / is_blacklisted` | Admin | Address blocking |
| `attest(invoice_id, verifier, type, hash, approved)` | Trusted verifier | Record an off-chain attestation (document hash, business registration, tax compliance); charges the verification fee (ADR-0009) |
| `get_verification_status(invoice_id, type)` / `get_invoice_verification_status(invoice_id)` | Anyone | Verification status per type, and the conjunction across all three |
| `get_verifications(invoice_id)` | Anyone | Every attestation recorded against an invoice |
| `expire_verifications(invoice_id)` | Anyone | Permissionless poke that records lapsed attestations and emits `ver_exp` |
| `add_verifier / remove_verifier / set_verifier_threshold` | Admin | Trusted verifier set and the m of m-of-n |
| `set_verification_fee / set_attestation_validity / register_currency` | Admin | Fee in bps (default 0 = off), attestation validity (default 90 days), fee settlement token |
| `set_rate / set_fee / transfer_admin / pause / unpause` | Admin | Admin controls |

### Financing — `financing/`

| Function | Auth | Description |
|---|---|---|
| `__constructor(admin, registry, token)` | Deployer (at deploy) | Wire to registry + default settlement token (ADR-0005) |
| `create_offer(offer_id, invoice_id, lender, amount, currency, rate, duration)` | Lender | Submit an offer (validates amount/rate/duration bounds) |
| `withdraw_offer / reject_offer` | Lender / Originator | Withdraw or reject a Pending offer |
| `accept_offer(offer_id, originator)` | Originator | Pulls principal lender → business **and mints the lender's position token** |
| `amend_offer(offer_id, lender, expected_round, amount, rate, duration)` | Lender | Revise a Pending offer's terms; settles immediately if they match the originator's live counter-offer (ADR-0008) |
| `counter_offer(offer_id, originator, expected_round, amount, rate, duration)` | Originator | Propose different terms; settles immediately if they match the lender's standing terms (ADR-0008) |
| `close_negotiation(offer_id, caller)` | Lender / Originator, or anyone once expired | End a negotiation — revokes a live counter-offer before the deadline, records expiry after it |
| `get_negotiation / get_negotiation_status / get_negotiation_deadline` | Anyone | Read the on-chain negotiation history, its derived status, and its frozen deadline |
| `set_negotiation_window(admin, secs)` | Admin | Negotiation window, default 72 h, clamped to 1 h – 30 days |
| `register_currency(admin, currency, token)` | Admin | Add a settlement currency — one registry entry, no code branch per currency |
| `set_position_token(admin, token)` | Admin | Configure the SEP-41 position-token contract (ADR-0002) |
| `get_position_token()` | Anyone | Read the configured position token |
| `update_offer_status / update_offer_amount_repaid / update_lender_stats_repaid / update_stats_repaid` | repayment | Repayment callbacks (registered caller only) |
| `pause / unpause / transfer_admin` | Admin | Admin controls |

### Repayment — `repayment/`

| Function | Auth | Description |
|---|---|---|
| `__constructor(admin, registry, financing, token)` | Deployer (at deploy) | Wire to registry + financing (ADR-0005) |
| `repay_invoice(invoice_id, offer_id, repayer, amount)` | Originator | Full or partial repayment (principal + yield) |
| `mark_overdue(invoice_id)` | Anyone | Flag a past-due Financed invoice |
| `reclaim_invoice(invoice_id, offer_id, lender)` | Lender | After the 7-day grace period → offer Defaulted |
| `calculate_total_due(offer_id)` | Anyone | Principal + accrued yield |

### Insurance — `insurance/`

| Function | Auth | Description |
|---|---|---|
| `__constructor(admin, token)` | Deployer (at deploy) | Set admin + staking token (ADR-0005) |
| `stake(staker, amount)` | Staker | Deposit the staking token into the pool (approve + pull pattern) |
| `unstake(staker, amount)` | Staker | Withdraw; the pool pays back directly |
| `get_stake(staker)` | Anyone | Staker's balance |
| `get_pool_total()` | Anyone | Accounting total of staked funds |
| `get_stakers_count()` | Anyone | Number of active stakers |
| `get_contract_token_balance()` | Anyone | Actual on-chain balance — audit check that accounting matches |
| `pay_out(beneficiary, amount)` | Payout caller only (repayment) | Pay up to `amount`, capped at pool balance; returns amount actually paid |
| `get_payout_caller()` | Anyone | Read the configured payout caller |
| `set_yield_rate(admin, rate_bps)` | Admin | Set the annual flat yield rate in basis points (e.g. 500 = 5 %). Banks existing yield prospectively before applying the new rate |
| `get_yield_rate()` | Anyone | Read the current annual yield rate in basis points |
| `accrued_yield(staker)` | Anyone | Preview the total accrued yield for a staker (banked + since last checkpoint) |
| `set_staking_token / pause / unpause / transfer_admin` | Admin | Admin controls |

---

## Position Tokens

On `accept_offer`, the financing contract mints a **SEP-41 position token** to the lender, 1:1
with the offer amount (one token per base unit of principal — see [ADR-0002](./docs/adr/0002-position-tokens.md)).
The token is a **Stellar Asset Contract** (`POS`, issued by the protocol deployer) whose admin is
the financing contract, so minting is authorized via implicit contract-invoker auth.

Position tokens are plain SEP-41 assets: any wallet can hold and transfer them, and
they represent the lender's claim on the financed invoice until it is repaid. Because they are
Stellar assets, a holder must establish a `POS` trustline before mint/transfer can credit them —
the frontend's portfolio offers a one-click trustline helper.

---

### Reputation — `reputation/`

| Function | Auth | Description |
|---|---|---|
| `__constructor(admin)` | Deployer (at deploy) | Sets admin atomically in the deploy operation (ADR-0005) |
| `set_recorder(admin, recorder)` | Admin | Set the repayment contract as the only writer |
| `record_outcome(originator, outcome)` | Recorder only | `0` = repaid, `1` = defaulted; updates outcome counts |
| `get_score(originator)` | Anyone | `repayments − 2×defaults`, floored at 0 (ADR-0004) |
| `get_record(originator)` | Anyone | Raw `{repayments, defaults}` counts — the source of truth |

## Protocol Events

**This table is the canonical event specification.** Every event emitted by the
protocol is listed here; new events must be added to this table when they ship.

Every state-mutating function publishes a Soroban contract event. Topics are
`(event_name, subject_id)` — indexers can filter by invoice or offer id without decoding payloads.

| Event | Emitted by | Data payload |
|---|---|---|
| `inv_reg` | `register_invoice` | `(originator, amount, due_date)` |
| `inv_amt` | `update_invoice_amount` | `new_amount` |
| `inv_sts` | status transitions (update / finance / repay) | `InvoiceStatus` |
| `off_new` | `create_offer` | `(invoice_id, lender, amount, interest_rate)` |
| `off_acc` | `accept_offer` | `(invoice_id, lender, amount)` |
| `off_rej` | `reject_offer` | `invoice_id` |
| `off_wdr` | `withdraw_offer` | `lender` |
| `off_amd` | `amend_offer` | `(lender, amount, interest_rate, duration)` |
| `ctr_off` | `counter_offer` | `(originator, amount, interest_rate, duration)` |
| `neg_clsd` | `close_negotiation`, auto-accept, `withdraw_offer` / `reject_offer` on an open negotiation | `(NegotiationStatus, closer)` |
| `off_def` | `reclaim_invoice` | `(invoice_id, lender)` |
| `inv_rep` | `repay_invoice` | `(offer_id, amount, fully_repaid)` |
| `inv_ovd` | `mark_overdue` | `due_date` |
| `inv_cxl` | `cancel_invoice` | `originator` |
| `inv_dsp` | `raise_dispute` | `originator` |
| `inv_rsl` | `resolve_dispute` | `new_status` |
| `ver_sub` | `attest` (registry) | `(verifier, type, hash, valid_until, fee)` |
| `ver_done` | `attest` (registry) | `(type, VerificationStatus)` — emitted when a type reaches Verified or Rejected |
| `ver_exp` | `expire_verifications` (registry) | `(verifier, type, valid_until)` |
| `pos_mint` | `accept_offer` (financing) | `(lender, amount)` — position token minted |
| `pool_stk` | `stake` (insurance) | `amount` |
| `pool_un` | `unstake` (insurance) | `amount` |
| `pool_pay` | `pay_out` (insurance) | `amount paid` |
| `pool_yld` | `unstake` (insurance) | `yield paid` — emitted only when yield > 0 |
| `reputn` | `record_outcome` (reputation) | `outcome` |

---

## Error Codes

All contracts emit machine-readable `E_*` error codes (see [docs/error-codes.md](./docs/error-codes.md)). Clients must branch on these stable codes — never on free-text error messages. The SDK maps typed contract errors → codes; the frontend maps codes → UI behaviour (redirect on `E_UNAUTHORIZED`, toast on `E_PAUSED`, etc.).

---

## Constants

| Constant | Value | Description |
|---|---|---|
| `GRACE_PERIOD_SECS` | 604,800 | 7-day grace period before lender can reclaim |
| `MIN_OFFER_DURATION_SECS` | 86,400 | Minimum offer duration (1 day) |
| `MAX_OFFER_DURATION_SECS` | 31,536,000 | Maximum offer duration (1 year) |
| `MIN_INVOICE_AMOUNT` | 10,000,000 | Minimum invoice amount in stroops (10 XLM / 10 USDC) |
| `DEFAULT_NEGOTIATION_WINDOW_SECS` | 259,200 | Default offer-negotiation window (72 hours) |
| `MIN_NEGOTIATION_WINDOW_SECS` / `MAX_NEGOTIATION_WINDOW_SECS` | 3,600 / 2,592,000 | Bounds an admin may configure the window to (1 hour – 30 days) |
| `MAX_NEGOTIATION_ROUNDS` | 20 | Cap on recorded negotiation rounds per offer |
| `DEFAULT_ATTESTATION_VALIDITY_SECS` | 7,776,000 | Default verification attestation validity (90 days) |
| `MIN_ATTESTATION_VALIDITY_SECS` / `MAX_ATTESTATION_VALIDITY_SECS` | 86,400 / 31,536,000 | Bounds an admin may configure attestation validity to (1–365 days) |
| `MAX_VERIFICATION_FEE_BPS` | 500 | Verification fee ceiling (5% of invoice value) |
| `MAX_VERIFIERS` | 20 | Maximum size of the trusted verifier set |
| `MAX_ATTESTATIONS_PER_INVOICE` | 60 | Cap on stored attestations per invoice |

---

## Development

```bash
# Build
cargo build --target wasm32v1-none --release

# Run tests (110 tests across registry / financing / repayment / insurance / reputation)
cargo test

# Check WASM size stays under 256 KB
bash scripts/check-size.sh

# Deploy to Testnet (requires stellar-cli)
bash scripts/deploy.sh
```

Or trigger the **[Deploy Contract](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/deploy-contract.yml)** GitHub Actions workflow for a one-click Testnet deploy.

For a full redeploy and migration, follow the [migration runbook](./docs/migration-runbook.md).

### Mainnet deployment

Mainnet deploys use a separate, **manual-only** workflow:
[**Deploy Contracts to Mainnet**](https://github.com/Stellar-VaultLink/invofi-contracts/actions/workflows/deploy-mainnet.yml)

The workflow has **no push or pull_request trigger** — it can only be started from the Actions UI or the GitHub API with an explicit `workflow_dispatch` event. It runs in two stages:

| Stage | Job | What happens |
|---|---|---|
| 1 | **Build & Hash (dry run)** | Compiles all five WASM artifacts, records their SHA-256 hashes in the job summary, checks sizes, and uploads the artifacts. Requires approval from the `production` environment's required reviewers before the next stage runs. |
| 2 | **Deploy to Mainnet** | Downloads the exact artifacts from stage 1, re-verifies their hashes, then deploys all five contracts and wires cross-contract callers, currencies, and the POS token. Runs under the `production` environment (second reviewer gate). |

**Prerequisites before triggering:**

1. Create a `production` environment in the repository settings and add at least one required reviewer.
2. Add the `STELLAR_MAINNET_DEPLOYER_SECRET_KEY` secret to that environment (not to the repository — scoping it to the environment ensures it is only accessible after reviewer approval).
3. The deployer account must be funded on Mainnet before the workflow runs (Friendbot does not exist on Mainnet).

**Rollback note:** Soroban contracts are immutable once deployed. There is no automated rollback. If a deployed contract contains a critical bug, the recovery path is:
1. Deploy a patched build as a **new** contract using this same workflow.
2. Re-point all cross-contract wiring and frontend env vars to the new contract IDs.
3. Follow the [migration runbook](./docs/migration-runbook.md) for state migration details.
4. Keep the old contract IDs recorded until the new deployment is fully verified.

---

## Roadmap

### Shipped

- [x] Five auditable contract crates with restricted cross-contract auth
- [x] SEP-41 token movement — `accept_offer` (lender → business), `repay_invoice` (principal + yield)
- [x] Position tokens, transferable positions, insurance stake/unstake
- [x] Insurance payout on default, reputation scoring
- [x] Emergency pause / circuit breaker, full protocol event coverage
- [x] Deployer-bound `__constructor` initialization (issue #75), CI: tests + clippy + Soroban Scout

### Open

- [ ] Mainnet deployment
- [ ] Independent security audit (SCF Audit Bank)
- [ ] Overdue-penalty interest
- [ ] Multisig admin governance
- [ ] Contract upgradeability with timelock

---

## Changelog

See [CHANGELOG.md](./CHANGELOG.md) for version history.

## Compliance

See the [compliance & regulatory posture](https://github.com/Stellar-VaultLink/invofi/blob/main/docs/compliance.md) in the main repo — KYC/SEP-12 roadmap, jurisdictions avoided at launch, and the securities-by-design analysis.

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
                <a href="https://github.com/KarenZita01">
                    <img src="https://avatars.githubusercontent.com/u/261386615?v=4" width="100;" alt="KarenZita01"/>
                    <br />
                    <sub><b>Karen Agbo</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/Abdulrasaq1515">
                    <img src="https://avatars.githubusercontent.com/u/209874744?v=4" width="100;" alt="Abdulrasaq1515"/>
                    <br />
                    <sub><b>Abdulrasaq1515</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/p3ris0n">
                    <img src="https://avatars.githubusercontent.com/u/94976593?v=4" width="100;" alt="p3ris0n"/>
                    <br />
                    <sub><b>Promise Raji</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/MJ-RWA">
                    <img src="https://avatars.githubusercontent.com/u/240063069?v=4" width="100;" alt="MJ-RWA"/>
                    <br />
                    <sub><b>MJ | Dev 🏀</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/Fury03">
                    <img src="https://avatars.githubusercontent.com/u/98775983?v=4" width="100;" alt="Fury03"/>
                    <br />
                    <sub><b>Damilola Ogunrotimi</b></sub>
                </a>
            </td>
		</tr>
		<tr>
            <td align="center">
                <a href="https://github.com/hexlaapp">
                    <img src="https://avatars.githubusercontent.com/u/287440938?v=4" width="100;" alt="hexlaapp"/>
                    <br />
                    <sub><b>hexlaapp</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/Awointa">
                    <img src="https://avatars.githubusercontent.com/u/82676631?v=4" width="100;" alt="Awointa"/>
                    <br />
                    <sub><b>Bob_The_Builder</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/Just-Bamford">
                    <img src="https://avatars.githubusercontent.com/u/233368823?v=4" width="100;" alt="Just-Bamford"/>
                    <br />
                    <sub><b>Bamford</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/DevSolex">
                    <img src="https://avatars.githubusercontent.com/u/220715997?v=4" width="100;" alt="DevSolex"/>
                    <br />
                    <sub><b>Dev solex</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/Chigybillionz">
                    <img src="https://avatars.githubusercontent.com/u/184784116?v=4" width="100;" alt="Chigybillionz"/>
                    <br />
                    <sub><b>OKORIE CHIGOZIE JEHOSHAPHAT</b></sub>
                </a>
            </td>
            <td align="center">
                <a href="https://github.com/RawNuke">
                    <img src="https://avatars.githubusercontent.com/u/67506722?v=4" width="100;" alt="RawNuke"/>
                    <br />
                    <sub><b>Raw_Nuke</b></sub>
                </a>
            </td>
		</tr>
		<tr>
            <td align="center">
                <a href="https://github.com/Wetshakat">
                    <img src="https://avatars.githubusercontent.com/u/182114004?v=4" width="100;" alt="Wetshakat"/>
                    <br />
                    <sub><b>Ishaku Dyelshak </b></sub>
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
