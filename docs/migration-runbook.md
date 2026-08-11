# Contract Migration and Redeploy Runbook

Author: RawNuke
Copyright (c) 2026 RawNuke. All rights reserved.

## Purpose

Soroban has no in-place upgrade. A redeploy creates new contract addresses.
The deploy workflow passes no `--salt`, so every deploy produces new IDs.
There is no way to change the code of a live contract. The old contracts keep
running forever. This runbook exists because a redeploy is error-prone and
state-lossy today. It is the step-by-step procedure for issue #125. It covers
snapshot state reads, deploy the new contracts, re-point the frontend
environment, recreate what must be recreated, and verify. It does not automate
the migration. The issue forbids that automation (future work). This runbook
targets testnet.

## Prerequisites

- Rust stable toolchain with the `wasm32v1-none` target.
- The Stellar CLI, installed from the official manual:

```bash
rustup target add wasm32v1-none
cargo install stellar-cli --locked
```

- The `invofi-deployer` identity, funded on testnet.
- Write access to the Vercel project, or to the invofi frontend
  `apps/frontend/.env.local` for local development.

The identity lives in `~/.config/stellar/identity/invofi-deployer.toml`.
Keep the same key across redeploys. A new key creates a new POS asset id
(see Phase 1) and a new admin address.

## Phase 0: Snapshot state reads

Run every read below BEFORE the deploy. The old contracts still serve these
reads after the deploy. The outputs are the only record of the old state.

Define the current contract IDs as shell variables first:

```bash
export REGISTRY_ID=<current-registry-id>
export FINANCING_ID=<current-financing-id>
export REPAYMENT_ID=<current-repayment-id>
export INSURANCE_ID=<current-insurance-id>
export REPUTATION_ID=<current-reputation-id>
```

### Registry

```bash
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_all_invoices
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_invoices_count
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_stats
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_blacklist
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_rate --tier A
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_rate --tier B
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_rate --tier C
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_fee
```

### Financing

```bash
stellar contract invoke --id $FINANCING_ID --network testnet -- get_all_offers
stellar contract invoke --id $FINANCING_ID --network testnet -- get_offers_count
stellar contract invoke --id $FINANCING_ID --network testnet -- get_stats
stellar contract invoke --id $FINANCING_ID --network testnet -- get_currency_token --currency XLM
stellar contract invoke --id $FINANCING_ID --network testnet -- get_currency_token --currency USDC
stellar contract invoke --id $FINANCING_ID --network testnet -- get_position_token
```

### Repayment

```bash
stellar contract invoke --id $REPAYMENT_ID --network testnet -- get_insurance
stellar contract invoke --id $REPAYMENT_ID --network testnet -- get_reputation
```

### Insurance

```bash
stellar contract invoke --id $INSURANCE_ID --network testnet -- get_pool_total
stellar contract invoke --id $INSURANCE_ID --network testnet -- get_stakers_count
stellar contract invoke --id $INSURANCE_ID --network testnet -- get_contract_token_balance
stellar contract invoke --id $INSURANCE_ID --network testnet -- get_stake --staker $STAKER_ADDRESS
```

Repeat the `get_stake` read for every staker. Get the staker list from the
invofi indexer database first.

### Reputation

```bash
stellar contract invoke --id $REPUTATION_ID --network testnet -- get_score --originator $ORIGINATOR_ADDRESS
stellar contract invoke --id $REPUTATION_ID --network testnet -- get_record --originator $ORIGINATOR_ADDRESS
```

Repeat both reads for every originator. Get the originator list from the
invofi indexer database first.

### How to enumerate the subject ids

The invoice and offer ids are `Symbol` values. They appear as the second
topic of the protocol events. Filter the registry event stream for `inv_reg`
(invoices) and `off_new` (offers). The invofi indexer database is the other
source. The indexer database is the more complete source: the event stream
only goes back to the RPC retention window.

### Provenance

Record the deployed wasm hash of each contract:

```bash
stellar contract info hash --id $REGISTRY_ID --network testnet
stellar contract info hash --id $FINANCING_ID --network testnet
stellar contract info hash --id $REPAYMENT_ID --network testnet
stellar contract info hash --id $INSURANCE_ID --network testnet
stellar contract info hash --id $REPUTATION_ID --network testnet
```

Store the hashes with the snapshot. They prove which build the old state
belongs to.

### Event-replay note

Full history is not retrievable on-chain. Soroban RPC only keeps events
inside its retention window. After that window, the event stream is gone.
The indexer database is the only long-term source of history. Treat the
snapshot as the migration source of truth.

### Save the snapshot

Create a dated snapshot directory:

```bash
mkdir -p snapshot/$(date +%Y-%m-%d)
```

Redirect every command output to a dated file:

```bash
stellar contract invoke --id $REGISTRY_ID --network testnet -- get_all_invoices \
  > snapshot/$(date +%Y-%m-%d)/registry-invoices.json
```

Use one file per contract and per read. Name the files after the read, for
example `registry-invoices.json`, `registry-stats.json`,
`financing-offers.json`, `insurance-pool.json`, `reputation-records.json`.

## Phase 1: State taxonomy

Some state returns automatically with the redeploy. Other state exists only
in the old contracts and must be re-created by hand.

| Contract | Re-derivable on-chain | Must be re-created manually |
|---|---|---|
| registry | admin (constructor arg `--admin`); financing and repayment wiring (`set_financing_contract`, `set_repayment_contract`); rates (`set_rate`); fee (`set_fee`); pause state (fresh contracts start unpaused) | invoices (`register_invoice`); blacklist entries (`blacklist_address`); stats counters (they rebuild from new activity) |
| financing | admin, registry, and token (constructor args); repayment wiring (`set_repayment_contract`); currencies (`register_currency` for XLM and USDC); position token (`set_position_token`) | offers (`create_offer`); lender stats (they rebuild from new activity) |
| repayment | admin, registry, financing, and token (constructor args); insurance and reputation wiring (`set_insurance`, `set_reputation`) | none. The repayment contract stores no user state |
| insurance | admin and staking token (constructor args); payout caller (`set_payout_caller`) | stakes and pool total. The funds sit in the old pool. The old contract keeps custody. Migration of staked funds is future work |
| reputation | admin (constructor arg); recorder (`set_recorder`) | records (`record_outcome`). The recorder restriction makes historical replay impractical (see Phase 4) |
| POS asset | the asset id is deterministic (command below). With the same deployer key, the SAC id is unchanged | none. Lenders keep their trustlines |

The POS asset id is deterministic:

```bash
stellar contract id asset --asset "POS:<deployer-public>" --network testnet
```

The deployer public key is the same one the workflow prints as the admin.
With the same deployer key, the Stellar Asset Contract (SAC) id is unchanged.
Lenders keep their existing `POS` trustlines. With a new deployer key, the
asset id changes and every lender must add a new trustline.

## Phase 2: Deploy the new contracts

### Option A: GitHub Actions workflow

The "Deploy Contracts to Testnet" workflow lives in
`.github/workflows/deploy-contract.yml`. It runs on `workflow_dispatch` with
one input, `network`, fixed to `testnet`.

Open the Actions page of `invofi-contracts` and run the workflow. The
workflow uses the `STELLAR_DEPLOYER_SECRET_KEY` secret when it is set. With
the secret set, the deployer key stays the same. Without it, the workflow
generates a fresh keypair for each run. Set the secret to the current
`invofi-deployer` seed for a migration. A fresh key creates a new POS asset
id (Phase 1).

The workflow summary prints this table:

| Contract | ID |
| --- | --- |
| registry | the new registry id |
| financing | the new financing id |
| repayment | the new repayment id |
| insurance | the new insurance id |
| reputation | the new reputation id |
| position token (POS) | deployed and admin'ed to financing in the next step |
| admin (constructor-bound) | the admin public key |

The workflow then performs this wiring, in this order:

1. `set_financing_contract` on the registry.
2. `set_repayment_contract` on the registry.
3. `set_repayment_contract` on financing.
4. `register_currency` for XLM.
5. `register_currency` for USDC.
6. POS asset deploy (only when the SAC is missing).
7. `set_admin` on the POS asset, to the new financing contract.
8. `set_position_token` on financing.
9. `set_insurance` on repayment.
10. `set_reputation` on repayment.
11. `set_payout_caller` on insurance, to the new repayment contract.
12. `set_recorder` on reputation, to the new repayment contract.

Save the five printed contract IDs. The next phases need them.

### Option B: manual stellar-cli fallback

`scripts/fund-and-deploy.sh` and `scripts/deploy.sh` deploy only the
registry today. They print a single `NEXT_PUBLIC_REGISTRY_CONTRACT_ID`.
The sequence below is the complete one. It reproduces the workflow exactly,
with the same constructor args and the same wiring order. Paste it into a
terminal in the repository root:

```bash
set -euo pipefail

NETWORK=testnet
SOURCE=invofi-deployer

# Only when the identity is not present
stellar keys generate $SOURCE --network $NETWORK

stellar keys fund $SOURCE --network $NETWORK

stellar contract build

ADMIN_PUBLIC=$(stellar keys address $SOURCE --network $NETWORK)
XLM_TOKEN=$(stellar contract id asset --asset native --network $NETWORK)
USDC_TOKEN=$(stellar contract id asset --asset USDC:GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5 --network $NETWORK)

# Deploy in dependency order. The constructor args bind admin and wiring
# atomically inside the deploy operation (issue #75).
REGISTRY_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/invofi_registry.wasm --source $SOURCE --network $NETWORK -- --admin "$ADMIN_PUBLIC")
FINANCING_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/invofi_financing.wasm --source $SOURCE --network $NETWORK -- --admin "$ADMIN_PUBLIC" --registry "$REGISTRY_ID" --token "$XLM_TOKEN")
REPAYMENT_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/invofi_repayment.wasm --source $SOURCE --network $NETWORK -- --admin "$ADMIN_PUBLIC" --registry "$REGISTRY_ID" --financing "$FINANCING_ID" --token "$XLM_TOKEN")
INSURANCE_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/invofi_insurance.wasm --source $SOURCE --network $NETWORK -- --admin "$ADMIN_PUBLIC" --token "$XLM_TOKEN")
REPUTATION_ID=$(stellar contract deploy --wasm target/wasm32v1-none/release/invofi_reputation.wasm --source $SOURCE --network $NETWORK -- --admin "$ADMIN_PUBLIC")

# Wire cross-contract callers and currencies
stellar contract invoke --id "$REGISTRY_ID" --source $SOURCE --network $NETWORK -- set_financing_contract --admin "$ADMIN_PUBLIC" --financing "$FINANCING_ID"
stellar contract invoke --id "$REGISTRY_ID" --source $SOURCE --network $NETWORK -- set_repayment_contract --admin "$ADMIN_PUBLIC" --repayment "$REPAYMENT_ID"
stellar contract invoke --id "$FINANCING_ID" --source $SOURCE --network $NETWORK -- set_repayment_contract --admin "$ADMIN_PUBLIC" --repayment "$REPAYMENT_ID"
stellar contract invoke --id "$FINANCING_ID" --source $SOURCE --network $NETWORK -- register_currency --admin "$ADMIN_PUBLIC" --currency XLM --token_addr "$XLM_TOKEN"
stellar contract invoke --id "$FINANCING_ID" --source $SOURCE --network $NETWORK -- register_currency --admin "$ADMIN_PUBLIC" --currency USDC --token_addr "$USDC_TOKEN"

# Position token: POS is deterministic per issuer. Deploy only when missing.
POS_ID=$(stellar contract id asset --asset "POS:$ADMIN_PUBLIC" --network $NETWORK)
if ! stellar contract info --id "$POS_ID" interface --network $NETWORK >/dev/null 2>&1; then
  stellar contract asset deploy --asset "POS:$ADMIN_PUBLIC" --source $SOURCE --network $NETWORK
fi
stellar contract invoke --id "$POS_ID" --source $SOURCE --network $NETWORK -- set_admin --new_admin "$FINANCING_ID"
stellar contract invoke --id "$FINANCING_ID" --source $SOURCE --network $NETWORK -- set_position_token --admin "$ADMIN_PUBLIC" --token "$POS_ID"

# Insurance payout caller and reputation recorder
stellar contract invoke --id "$REPAYMENT_ID" --source $SOURCE --network $NETWORK -- set_insurance --admin "$ADMIN_PUBLIC" --insurance "$INSURANCE_ID"
stellar contract invoke --id "$REPAYMENT_ID" --source $SOURCE --network $NETWORK -- set_reputation --admin "$ADMIN_PUBLIC" --reputation "$REPUTATION_ID"
stellar contract invoke --id "$INSURANCE_ID" --source $SOURCE --network $NETWORK -- set_payout_caller --admin "$ADMIN_PUBLIC" --payout_caller "$REPAYMENT_ID"
stellar contract invoke --id "$REPUTATION_ID" --source $SOURCE --network $NETWORK -- set_recorder --admin "$ADMIN_PUBLIC" --recorder "$REPAYMENT_ID"

echo "registry=$REGISTRY_ID"
echo "financing=$FINANCING_ID"
echo "repayment=$REPAYMENT_ID"
echo "insurance=$INSURANCE_ID"
echo "reputation=$REPUTATION_ID"
echo "position_token=$POS_ID"
echo "admin=$ADMIN_PUBLIC"
```

Keep the same `invofi-deployer` identity for the next migration. The POS
asset id depends on it (Phase 1).

## Phase 3: Re-point the frontend environment

A redeploy is a Vercel settings change, not a code change. The frontend code
does not change. Set these variables in the Vercel project for the invofi
frontend:

| Variable | Value |
|---|---|
| `NEXT_PUBLIC_REGISTRY_CONTRACT_ID` | the new registry id |
| `NEXT_PUBLIC_FINANCING_CONTRACT_ID` | the new financing id |
| `NEXT_PUBLIC_REPAYMENT_CONTRACT_ID` | the new repayment id |

The optional variable `NEXT_PUBLIC_POSITION_TOKEN_ASSET` stays unchanged
when the deployer key is the same. The asset id is `POS:<deployer-public>`.
The variable defaults to the live testnet POS asset when unset.

The legacy fallback `NEXT_PUBLIC_CONTRACT_ID` is used only when the three
contract id variables above are all unset. Do not set it for a five-contract
deployment. The frontend routes all calls to that one contract in legacy
mode.

For local development, edit `apps/frontend/.env.local.example` in the invofi
repository. Copy the file to `.env.local` and replace the three ids. The
SDK reads these values through `createInvofiClient` in
`apps/sdk/src/config.ts`.

The frontend wires only registry, financing, and repayment. Insurance and
reputation are reached only through cross-contract calls, so they need no
frontend variables.

## Phase 4: Recreate the state that must be re-created

This phase is manual. The issue says stop: no automation of the migration
itself. Every command below needs the subject id from the Phase 0 snapshot.

### Registry: re-register invoices

The originator key must sign each call. Amounts below 10,000,000 stroops
(10 XLM or 10 USDC) are rejected. The due date must be in the future.

```bash
stellar contract invoke --id $NEW_REGISTRY_ID --source $ORIGINATOR_KEY --network testnet \
  -- register_invoice --id $INVOICE_ID --originator $ORIGINATOR_ADDRESS \
  --amount $AMOUNT_STROOPS --currency $CURRENCY --due_date $DUE_DATE_TS
```

### Financing: recreate offers

Each lender signs its own offer. The invoice must exist in the new registry
first.

```bash
stellar contract invoke --id $NEW_FINANCING_ID --source $LENDER_KEY --network testnet \
  -- create_offer --offer_id $OFFER_ID --invoice_id $INVOICE_ID \
  --lender $LENDER_ADDRESS --amount $AMOUNT_STROOPS --currency $CURRENCY \
  --interest_rate $RATE_BPS --duration $DURATION_SECS
```

Offers that were already accepted cannot be recreated as accepted. The old
offers moved real funds. A recreated offer starts Pending again.

### Registry: re-add blacklist entries

```bash
stellar contract invoke --id $NEW_REGISTRY_ID --source invofi-deployer --network testnet \
  -- blacklist_address --admin $ADMIN_PUBLIC --target $BLACKLISTED_ADDRESS
```

Run one call per address from the Phase 0 `get_blacklist` snapshot.

### Reputation: records

`record_outcome` accepts calls from the recorder only. The recorder is the
repayment contract. The admin has no backdoor to write records. Re-recording
history would need the repayment contract to replay outcomes, and there is
no such mechanism today. Where an authoritative indexer history exists, this
replay is future work. Otherwise accept the reset: a fresh reputation
contract starts with empty records and scores of 0. New activity rebuilds
them.

### Insurance: stakes cannot be recreated

The staked funds sit in the old insurance contract. The old contract keeps
custody. There is no transfer or migration function in the new contract.
Stakers must unstake from the old contract themselves. Migration of staked
funds is future work. Record the Phase 0 `get_pool_total` and per-staker
`get_stake` outputs in the snapshot for that future work.

### Testnet note

For testnet, recreation is usually unnecessary. Testnet exists for testing.
The snapshot reads still have value as the record of what the old contracts
held. This phase exists for a real redeploy, not for testnet practice.

## Phase 5: Verify via the e2e walkthrough

Issue #104 adds `docs/manual-e2e.md`, the end-to-end walkthrough. It is
still open at the time of writing. Once it lands, follow it end-to-end on
the new contract ids. Until then, run the minimal smoke check below.

### Deploy-time checks

Read the on-chain hash of every new contract:

```bash
stellar contract info hash --id $NEW_REGISTRY_ID --network testnet
stellar contract info hash --id $NEW_FINANCING_ID --network testnet
stellar contract info hash --id $NEW_REPAYMENT_ID --network testnet
stellar contract info hash --id $NEW_INSURANCE_ID --network testnet
stellar contract info hash --id $NEW_REPUTATION_ID --network testnet
```

Each hash must match the wasm build you deployed. Keep the build artifact
with the snapshot so the comparison stays possible.

### Wiring checks

```bash
stellar contract invoke --id $NEW_FINANCING_ID --network testnet -- get_position_token
stellar contract invoke --id $NEW_FINANCING_ID --network testnet -- get_currency_token --currency XLM
stellar contract invoke --id $NEW_FINANCING_ID --network testnet -- get_currency_token --currency USDC
stellar contract invoke --id $NEW_REPAYMENT_ID --network testnet -- get_insurance
stellar contract invoke --id $NEW_REPAYMENT_ID --network testnet -- get_reputation
stellar contract invoke --id $NEW_INSURANCE_ID --network testnet -- get_payout_caller
stellar contract invoke --id $NEW_REPUTATION_ID --network testnet -- get_recorder
```

The position token must equal the POS id. The currency token reads must
return the XLM and USDC asset contracts. The four wiring reads must return
the matching new contract ids.

### Lifecycle smoke check

Run the full lifecycle on testnet with small amounts. Use three keys:
`$ORIGINATOR_KEY`, `$LENDER_KEY`, and the deployer. Fund the lender with
testnet XLM first. Register, offer, accept, repay:

```bash
# Registry: originator registers an invoice (10 XLM, due in 1 day)
stellar contract invoke --id $NEW_REGISTRY_ID --source $ORIGINATOR_KEY --network testnet \
  -- register_invoice --id inv-smoke --originator $ORIGINATOR_ADDRESS \
  --amount 10000000 --currency XLM --due_date $(( $(date +%s) + 86400 ))

# Financing: lender creates an offer (10 XLM at 5% for 1 day)
stellar contract invoke --id $NEW_FINANCING_ID --source $LENDER_KEY --network testnet \
  -- create_offer --offer_id off-smoke --invoice_id inv-smoke \
  --lender $LENDER_ADDRESS --amount 10000000 --currency XLM \
  --interest_rate 500 --duration 86400

# Financing: originator accepts. Principal moves, POS mints
stellar contract invoke --id $NEW_FINANCING_ID --source $ORIGINATOR_KEY --network testnet \
  -- accept_offer --offer_id off-smoke --invoice_originator $ORIGINATOR_ADDRESS

# Repayment: originator repays principal plus yield (10.5 XLM)
stellar contract invoke --id $NEW_REPAYMENT_ID --source $ORIGINATOR_KEY --network testnet \
  -- repay_invoice --invoice_id inv-smoke --offer_id off-smoke \
  --repayer $ORIGINATOR_ADDRESS --amount 10500000
```

Confirm the outcome:

```bash
stellar contract invoke --id $NEW_REGISTRY_ID --network testnet -- get_invoice --id inv-smoke
stellar contract invoke --id $NEW_FINANCING_ID --network testnet -- get_stats
```

The invoice status must be `Repaid`. The financing stats must show the
financed and repaid amounts. The reputation contract must record the outcome:
`get_record` for the originator shows repayments 1, defaults 0, score 1.

## Rollback

Keep the old contract ids in a second Vercel variable set until verification
passes. Do not delete them. The old contracts keep running and keep custody
of any funds. To revert, restore the old variable values in Vercel. A revert
is a settings change, not a code change. The old contracts never stopped
serving their state.
