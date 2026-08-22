#!/usr/bin/env bash
# Run the workspace test suite under cargo-llvm-cov + nextest and print a
# per-crate summary. Overrides any WASM default target so coverage is collected
# on the host (same as CI).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

HOST="$(rustc -vV | sed -n 's/^host: //p')"
OUT_DIR="${ROOT}/coverage/local"
mkdir -p "$OUT_DIR"

PROPTEST_CASES="${PROPTEST_CASES:-256}"
export PROPTEST_CASES

echo "==> Collecting coverage on ${HOST} (PROPTEST_CASES=${PROPTEST_CASES})"
# Emit LCOV in the same invocation that runs tests. Splitting into
# `--no-report` + `report` fails when `--target` is set (object path mismatch).
cargo llvm-cov nextest \
  --workspace \
  --target "$HOST" \
  --lcov \
  --output-path "${OUT_DIR}/lcov.info" \
  --summary-only \
  | tee "${OUT_DIR}/summary.txt"

echo
echo "LCOV: ${OUT_DIR}/lcov.info"
echo "Text: ${OUT_DIR}/summary.txt"
echo
echo "Compare against coverage/baseline.json before opening a PR that touches"
echo "money-moving crates. Soft CI check warns if touched-crate coverage drops."
echo "Optional HTML: cargo llvm-cov report --html --output-dir coverage/local/html"
echo "(only works if you re-run without --target, or from a host-default build)."
