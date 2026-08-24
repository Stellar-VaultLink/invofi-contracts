# ADR-0010: M-of-N Admin Governance (Multisig)

- Status: Accepted
- Date: 2026-08-22

## Context

Every InvoFi contract — registry, financing, repayment, insurance, reputation
— binds a single admin `Address` at deploy (ADR-0005) and gates every
`set_*`, `pause`/`unpause`, `resolve_dispute`, and `transfer_admin` call on a
single `require_auth()` check against that one address. ADR-0001 flagged this
directly: "Admin is a single deployer key for now. Multisig / DAO admin is
future scope; when it lands it replaces the `assert_admin` check, not the
pause mechanism." This is that follow-through, tracked as a roadmap item
(issue #50).

The risk is concentration: one compromised key can pause the protocol,
redirect cross-contract wiring (`set_financing_contract`,
`set_payout_caller`, …), blacklist addresses, change fee/rate/penalty
parameters, or hand admin to an attacker via `transfer_admin` — all in one
transaction, with no second signer able to stop it.

## Decision

Replace the single stored admin `Address` with an `AdminConfig { signers:
Vec<Address>, threshold: u32 }` in `invofi_common`, and change every
admin-gated entry point's authorization parameter from a single `admin:
Address` to a `signers: Vec<Address>`. A call is authorized once at least
`threshold` *distinct* addresses in `signers` are (a) members of the
configured signer set and (b) each present their own `require_auth()` inside
that same invocation:

```rust
pub fn assert_threshold(env: &Env, cfg: &AdminConfig, provided: &Vec<Address>) {
    let mut counted: Vec<Address> = Vec::new(env);
    for who in provided.iter() {
        if !is_signer(cfg, &who) { env.panic_with_error(ContractError::Unauthorized); }
        if counted.iter().any(|c| c == who) { env.panic_with_error(ContractError::Unauthorized); }
        who.require_auth();
        counted.push_back(who);
    }
    if counted.len() < cfg.threshold { env.panic_with_error(ContractError::Unauthorized); }
}
```

### Same-block, no on-chain proposal queue

This mirrors ADR-0001's "same-block" philosophy rather than introducing a
timelock or an on-chain propose/approve state machine. Soroban already lets a
single transaction carry one signed authorization entry per address —
`Address::require_auth()` verifies each entry independently. So an M-of-N
call is simply a transaction whose `signers` argument lists `threshold` (or
more) addresses, each of whom pre-signed their own authorization entry
off-chain; a coordinator (any one signer, or a relayer) collects those
entries and submits the combined transaction. There is no separate approval
transaction to send, no proposal id to track, and nothing pending on-chain
that a second attacker-controlled key could later push through alone — the
whole action either has enough valid signatures *in this transaction* or it
reverts with no state change at all.

An async on-chain proposal queue (open a proposal, have other signers approve
it in separate transactions over time) was considered and rejected for this
stage: it adds persistent proposal storage, replay/expiry bookkeeping, and a
second class of "pending but not yet authorized" state to reason about, for a
capability (asynchronous, non-atomic approval) the emergency-pause philosophy
already argues against. It remains a reasonable future extension if
real-world signer coordination turns out to need it.

### Single-admin bootstrap mode

`AdminConfig { signers: [admin], threshold: 1 }` — one signer, threshold one
— is **single-admin bootstrap mode**, and it is what every constructor sets
up by default via `invofi_common::init_admin_config`:

```rust
pub fn __constructor(env: Env, admin: Address) {
    invofi_common::init_admin_config(&env, &admin);
}
```

Under bootstrap mode, every threshold-gated call takes a one-element
`Vec<Address>` containing the sole signer and behaves exactly like the old
single-admin check — same authorization outcome, same events, same storage
writes. A deployment stays in this mode until an admin deliberately opts into
true M-of-N.

### Opting into true M-of-N

Each contract exposes `set_signers(signers: Vec<Address>, new_signers:
Vec<Address>, new_threshold: u32)`, itself threshold-gated by the *current*
config. This is the only way to add signers or raise the threshold — there is
no separate "add signer" call that a single key could invoke unilaterally
once a deployment has moved beyond bootstrap. `invofi_common::validate_signers`
enforces: non-empty, `1 <= new_threshold <= new_signers.len()`, no duplicate
address, and a `MAX_ADMIN_SIGNERS` (20) cap so the threshold check's loop
stays bounded (same reasoning as `MAX_VERIFIERS` — see ADR-0009).

`transfer_admin(signers: Vec<Address>, new_admin: Address)` keeps its
original meaning as a full handoff / disaster-recovery escape hatch: it
collapses the *entire* config to a single new signer at threshold 1
(`AdminConfig { signers: [new_admin], threshold: 1 }`), gated by the outgoing
config's threshold. Reconfiguring to a different M-of-N set (add/remove
signers, change the threshold without collapsing to one) goes through
`set_signers` instead.

### What moved and what didn't

Every function that used to take `admin: Address` and compare it against the
stored admin now takes `signers: Vec<Address>` and calls `assert_admin`
(a thin per-contract wrapper around `assert_threshold`):

- **Registry**: `transfer_admin`, `set_signers` (new), `set_financing_contract`,
  `set_repayment_contract`, `pause`, `unpause`, `set_rate`, `set_fee`,
  `resolve_dispute`, `blacklist_address`, `unblacklist_address`,
  `add_verifier`, `remove_verifier`, `set_verifier_threshold`,
  `set_verification_fee`, `set_attestation_validity`, `register_currency`.
- **Financing**: `transfer_admin`, `set_signers` (new), `set_repayment_contract`,
  `register_currency`, `set_position_token`, `pause`, `unpause`,
  `set_negotiation_window`.
- **Repayment**: `transfer_admin`, `set_signers` (new), `set_insurance`,
  `set_reputation`, `set_penalty`, `pause`, `unpause`. (The protocol fee
  recipient inside `repay_invoice`, previously the raw stored admin address,
  now resolves to the primary signer — `AdminConfig.signers[0]` — which is
  identical under bootstrap mode.)
- **Insurance**: `transfer_admin`, `set_signers` (new), `set_staking_token`,
  `set_payout_caller`, `set_registry`, `set_yield_rate`, `pause`, `unpause`.
- **Reputation**: `transfer_admin` (new), `set_signers` (new), `set_recorder`,
  `pause`, `unpause`, `resolve_dispute`.

What did **not** move, per ADR-0001's own framing ("multisig... replaces the
`assert_admin` check, not the pause mechanism"):

- `assert_not_paused` and the `paused` flag are untouched — pausing is still
  same-block and requires no separate unlock.
- The `paused`/emergency-brake call itself is still one transaction; it is
  the *authorization* on that transaction that now may require more than one
  signature, not the mechanism.
- Every non-admin authorization (originator, lender, verifier, cross-contract
  implicit-invoker auth) is untouched.

### New read surface

Every contract gains `get_admin_config() -> AdminConfig`, `get_signers() ->
Vec<Address>`, and `get_threshold() -> u32`. `get_admin() -> Address` is kept
for backward compatibility and returns the *primary* signer
(`signers[0]`) — safe to keep because no contract in this codebase calls
`get_admin()` cross-contract for authorization (verified: it is a
query-only, human/tooling-facing getter everywhere it is used); it is not a
source of authority by itself under true M-of-N.

## Migration path

Soroban contracts here have no upgrade entrypoint (`update_current_contract_wasm`
is not wired into any of the five contracts), so an already-deployed instance
cannot gain this admin model in place — the same constraint ADR-0005 recorded
for its own change ("Live instances deployed before this ADR remain
admin-bound to the deployer"). The migration is:

1. **Fresh deployments** build in single-admin bootstrap mode automatically —
   the constructor is unchanged (`--admin <address>`), so `deploy-contract.yml`
   and `scripts/deploy.sh` need no constructor changes.
2. **Every admin-gated CLI invocation changes shape**: the `--admin <address>`
   invoke argument becomes `--signers '["<address>"]'` (a JSON array). This
   repo's deploy workflow (`.github/workflows/deploy-contract.yml`) is
   updated accordingly as part of this change.
3. **Existing testnet/mainnet instances deployed before this ADR** keep their
   old single-`Address` admin storage layout and old function signatures —
   this migration does not (and cannot, without an upgrade path) touch them.
   Moving one to the new model means deploying a fresh instance with the
   updated WASM and re-running the post-deploy wiring steps
   (`set_financing_contract`, `set_repayment_contract`, `set_insurance`, …)
   against the new instance, exactly as any other WASM upgrade in this
   protocol would require today.
4. **Opting into real M-of-N** on a freshly deployed (bootstrap-mode)
   instance is a single post-deploy call: `set_signers(["<admin>"], [<n
   addresses>], <threshold>)`, signed by the sole bootstrap admin.

## Consequences

- No compromised single key can unilaterally pause, reconfigure, or drain the
  protocol on any deployment that has opted into `threshold > 1` — an
  attacker needs `threshold` signer keys, not one.
- Bootstrap deployments (the default, and every deployment until an operator
  explicitly runs `set_signers`) are behaviourally identical to before this
  ADR — same one-call UX, same events, same storage semantics — so this is
  additive, not a functional regression for anyone who never configures
  multisig.
- Every admin-gated call site's argument list changed shape (`Address` →
  `Vec<Address>`), which is a breaking ABI change for any off-chain caller
  (CLI scripts, indexer, frontend) invoking those functions directly. This is
  intentional and unavoidable for a real multisig — the whole point is that
  more than one address may need to authorize a call — and is called out
  explicitly rather than hidden behind an unchanged signature.
- `MAX_ADMIN_SIGNERS` (20) bounds the threshold-check loop and the
  `set_signers` validation loop, the same way `MAX_VERIFIERS` bounds the
  verification oracle (ADR-0009).
