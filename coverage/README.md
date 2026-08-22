# Coverage baseline

Per-crate line-coverage floor used by the soft CI check in `.github/workflows/ci.yml`.

| File | Purpose |
|---|---|
| `baseline.json` | Documented floors per crate + workspace figure |
| `badge.json` | Shields.io endpoint payload rendered on the README |

## Current baseline (2026-08-21)

| Crate | Line coverage |
|---|---:|
| `invofi-common` | 81.36% |
| `invofi-registry` | 98.68% |
| `invofi-financing` | 96.39% |
| `invofi-repayment` | 94.86% |
| `invofi-insurance` | 83.38% |
| `invofi-reputation` | 88.27% |
| `invofi-integration` | 100.00% |
| **workspace (weighted)** | **94.9%** |

## Policy

- **Touched crates only.** A PR that lowers line coverage on a crate it modified gets a warning annotation and a PR comment. Untouched crates are ignored.
- **Not a hard gate (yet).** The coverage job stays green even when a regression is reported so we can establish signal before flipping the switch.
- **No repo-wide % gate.** Do not add `--fail-under-lines` until the team agrees on a number.

## Updating the baseline

After you add tests and coverage goes up on `master`:

1. Run `bash scripts/coverage.sh` locally (or copy numbers from the Coverage job summary).
2. Update `crates.*` values and `workspace_lines_pct` in `baseline.json`.
3. Update `badge.json` `message` / `color` to match the workspace figure.
4. Commit with a message like `ci(coverage): raise baseline after <feature>`.
