# Security Policy

InvoFi's smart contracts run on Stellar Soroban. This repo is the audit-facing
home of the protocol logic — if you find a vulnerability here, it matters.

## Supported Versions

| Version | Status |
| --- | --- |
| `main` branch | Actively maintained |
| Older branches / tags | Not supported |

---

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.** Public disclosure before a fix is ready puts users at risk.

### How to report

Private vulnerability reporting is **enabled** on this repository:

1. Go to the [GitHub Security Advisories](https://github.com/Stellar-VaultLink/invofi-contracts/security/advisories/new) page for this repo.
2. Click **"New draft security advisory"** and fill in the details.
3. We will acknowledge your report within 48 hours and provide an estimated timeline for a fix.

### Alternative contact

Prefer email? Reach the maintainer directly at:

- **samuelojetunde898@gmail.com**

Include as much detail as possible — a description of the vulnerability,
reproduction steps, affected functions, and any suggested mitigation.

---

## Threat Model

Before reporting or evaluating a finding, read the [threat model](./docs/threat-model.md):
assets, trust boundaries, threat actors, per-threat mitigation mappings
(enforcing function + tests), and explicitly documented tradeoffs about what
the protocol does **not** protect against.

---

## What to Report

Smart-contract specific:

- Authorization bypass (`require_auth` gaps, admin escalation, `initialize()` front-running)
- Storage corruption or key collisions
- Cross-contract call issues (reentrancy, auth propagation, fee/amount math)
- Token handling (SEP-41) issues — minting, transfers, decimals, trustlines
- State-machine edge cases (overdue/default/reclaim/dispute transitions)

---

## Process

1. **Acknowledge** within 48 hours.
2. **Triage** severity. Fund-loss paths take priority.
3. **Fix + test** (every fix lands with a regression test — see `CONTRIBUTING.md`).
4. **Coordinate disclosure**, crediting the reporter when the fix ships.

Every accepted fix is documented in `CHANGELOG.md` and, where relevant, in an
ADR under `adr/`.
