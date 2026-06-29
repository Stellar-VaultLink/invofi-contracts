#!/usr/bin/env bash
set -euo pipefail

NETWORK="${1:-testnet}"
SOURCE="${2:-invofi-deployer}"

echo "Building contract..."
stellar contract build

WASM="target/wasm32v1-none/release/invofi_invoice_registry.wasm"

if [ ! -f "$WASM" ]; then
  echo "Build failed — WASM not found at $WASM"
  exit 1
fi

echo "Deploying to $NETWORK as $SOURCE..."
CONTRACT_ID=$(stellar contract deploy \
  --wasm "$WASM" \
  --source "$SOURCE" \
  --network "$NETWORK")

echo ""
echo "Contract deployed successfully!"
echo "CONTRACT_ID=$CONTRACT_ID"
echo ""
echo "Add to your .env.local:"
echo "NEXT_PUBLIC_CONTRACT_ID=$CONTRACT_ID"
