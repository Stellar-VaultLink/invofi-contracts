# Contributing to InvoFi Contracts

## Prerequisites

- Rust 1.70+ with `wasm32-unknown-unknown` target
- Stellar CLI: `cargo install --locked stellar-cli`

## Setup

```bash
git clone https://github.com/Stellar-VaultLink/invofi-contracts.git
cd invofi-contracts
cargo test
```

## Workflow

1. Fork and create a branch: `git checkout -b feat/your-change`
2. Make changes to `lib.rs` or `invofi-core/src/`
3. Add or update tests in `test.rs`
4. Run `cargo test` — all tests must pass
5. Run `stellar contract build` — WASM must compile
6. Open a pull request against `master`

## Commit Convention

```
feat(contracts): add thing
fix(contracts): correct thing
test(contracts): add test for thing
chore: update deps
```

## Code Standards

- No `unwrap()` in contract logic — use `panic!` with a descriptive message
- All public functions must have a matching test
- Data types live in `invofi-core`, contract logic in `lib.rs`

## License

MIT © 2026 InvoFi Contributors
