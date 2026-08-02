# Contributing to InvoFi Contracts

Thank you for your interest! InvoFi Contracts is a Soroban smart contract for invoice financing on Stellar.

### Label → Wave points

Issues are labelled by **complexity**, not raw point values. Each label maps to a Wave reward tier so contributors can gauge effort at a glance:

| Label | Wave points | What it covers |
|---|---|---|
| `trivial` | 100 | Small, well-scoped fixes — typos, one-line bugs, simple docs |
| `medium` | 150 | Standard features and fixes — a single contract function or test |
| `high-complexity` | 200 | Large multi-part efforts — new subsystems, migration-sized changes |
| `good-first-issue` | 100 | Onboarding-friendly tasks; usually also trivial or medium |

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

```bash
cargo test
```

### Lint

```bash
cargo clippy -- -D warnings
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
