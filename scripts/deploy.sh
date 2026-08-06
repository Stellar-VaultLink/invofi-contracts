#!/usr/bin/env bash
set -euo pipefail

NETWORK="${1:-testnet}"
SOURCE="${2:-invofi-deployer}"

echo "Building contracts..."
stellar contract build

WASM="target/wasm32v1-none/release/invofi_registry.wasm"

if [ ! -f "$WASM" ]; then
  echo "Build failed — WASM not found at $WASM"
  exit 1
fi

# The registry takes its admin through the __constructor (issue #75): the
# constructor runs atomically inside the deploy operation, which only the
# deployer can authorize — there is no separate initialize() call to front-run.
ADMIN_PUBLIC=$(stellar keys address "$SOURCE" --network "$NETWORK")

echo "Deploying registry to $NETWORK as $SOURCE..."
CONTRACT_ID=$(stellar contract deploy \
  --wasm "$WASM" \
  --source "$SOURCE" \
  --network "$NETWORK" \
  -- --admin "$ADMIN_PUBLIC")

echo ""
echo "Registry deployed successfully!"
echo "REGISTRY_ID=$CONTRACT_ID"
echo ""
echo "Add to your .env.local:"
echo "NEXT_PUBLIC_REGISTRY_CONTRACT_ID=$CONTRACT_ID"
echo ""
echo "Note: financing/repayment/insurance/reputation are deployed by the"
echo "deploy-contract.yml GitHub Action, which wires every constructor arg."
