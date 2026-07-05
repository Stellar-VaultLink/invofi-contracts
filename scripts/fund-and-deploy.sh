#!/usr/bin/env bash
set -euo pipefail

KEY_NAME="${1:-invofi-deployer}"
NETWORK="${2:-testnet}"

echo "Generating keypair: $KEY_NAME"
stellar keys generate --global "$KEY_NAME" --network "$NETWORK"

echo "Funding account on $NETWORK..."
stellar keys fund "$KEY_NAME" --network "$NETWORK"

echo "Running deploy..."
bash "$(dirname "$0")/deploy.sh" "$NETWORK" "$KEY_NAME"
