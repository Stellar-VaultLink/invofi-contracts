# Contributing to InvoFi Contracts

Thank you for your interest! InvoFi Contracts is a Soroban smart contract for invoice financing on Stellar.

### Issue labels

Issues are labelled by **complexity** so contributors can gauge effort at a glance:

| Label | What it covers |
|---|---|
| `trivial` | Small, well-scoped fixes — typos, one-line bugs, simple docs |
| `medium` | Standard features and fixes — a single contract function or test |
| `high-complexity` | Large multi-part efforts — new subsystems, migration-sized changes |
| `good-first-issue` | Onboarding-friendly tasks; usually also trivial or medium |

Additional area labels (`contracts`, `infra`, `docs`) describe *where* the work lives, not its size — a `docs` issue can still be `trivial`, `medium`, or `high-complexity`.

## Getting started

### Prerequisites
- Rust (stable toolchain) + `wasm32v1-none` target
- [Stellar CLI](https://developers.stellar.org/docs/tools/cli) (`cargo install stellar-cli`)

```bash
rustup target add wasm32v1-none
cargo install stellar-cli --locked
```

### Build

```bash
stellar contract build
```

### Test

CI runs the suite with [cargo-nextest](https://nexte.st/) (parallel, same assertions as `cargo test`). Locally you can use either:

```bash
# Fast parallel runner (preferred; matches CI)
cargo nextest run --target "$(rustc -vV | sed -n 's/^host: //p')"

# Classic serial runner
cargo test --target "$(rustc -vV | sed -n 's/^host: //p')"
```

Install nextest once with `cargo install cargo-nextest --locked`, or on Windows grab the [pre-built binary](https://get.nexte.st/).

Property tests use fewer cases locally by default; CI sets `PROPTEST_CASES=2000`. Override locally when needed:

```bash
PROPTEST_CASES=2000 cargo nextest run --target "$(rustc -vV | sed -n 's/^host: //p')"
```

### Coverage

We collect line coverage with [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) wrapping nextest (LLVM instrumentation — the modern alternative to tarpaulin/grcov).

```bash
# One-time: rustup component add llvm-tools-preview
#           cargo install cargo-llvm-cov cargo-nextest --locked
bash scripts/coverage.sh
```

HTML output lands in `coverage/local/html/`. Per-crate floors live in [`coverage/baseline.json`](./coverage/baseline.json) and are shown on the README badge via [`coverage/badge.json`](./coverage/badge.json).

**Soft PR policy:** changing a crate must not reduce that crate’s line coverage vs the baseline. CI posts a comment / `::warning::` annotation on regressions but does **not** fail the job yet, and there is no repo-wide percentage hard gate. See [`coverage/README.md`](./coverage/README.md).

### Lint

```bash
cargo clippy --target wasm32v1-none -- -D warnings
```

### Check contract size

```bash
stellar contract build
bash scripts/check-size.sh
```

## Deployment

To deploy to testnet, run:

```bash
bash scripts/fund-and-deploy.sh invofi-deployer testnet
```

Then copy the printed `CONTRACT_ID` into your Vercel environment variables as `NEXT_PUBLIC_CONTRACT_ID`.

## Code style

- Follow Rust idioms; run `cargo fmt` before committing
- Every new public function requires at least one test in `test.rs`
- Document all panicking conditions in the function doc comment
- Keep CHANGELOG.md updated for every new feature
