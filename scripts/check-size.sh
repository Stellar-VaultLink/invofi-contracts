#!/usr/bin/env bash
# Check the compiled WASM size stays below Stellar's 256KB limit.
set -euo pipefail

WASM="target/wasm32v1-none/release/invofi_invoice_registry.wasm"
MAX_SIZE=262144  # 256 KB in bytes

if [ ! -f "$WASM" ]; then
  echo "WASM not found — run 'stellar contract build' first."
  exit 1
fi

SIZE=$(wc -c < "$WASM")
echo "Contract WASM size: $SIZE bytes (limit: $MAX_SIZE bytes)"

if [ "$SIZE" -gt "$MAX_SIZE" ]; then
  echo "ERROR: WASM exceeds the 256 KB Stellar limit!"
  exit 1
fi

echo "OK — contract size is within limits."
