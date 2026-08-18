# ADR-0009 — Storage Schema Versioning

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Issue** | [#66](https://github.com/Stellar-VaultLink/invofi-contracts/issues/66) |
| **Cross-references** | [Issue #46 — Storage Schema](https://github.com/Stellar-VaultLink/invofi-contracts/issues/46), ADR-0005 (constructors) |

---

## Context

Soroban contracts write state under string storage keys with no version marker.
Nothing currently prevents a future release from silently breaking reads of
already-deployed state by renaming a key or changing a value's shape. Issue #46
documents the current schema but provides no runtime guard.

We need a mechanism that:

1. Tags every deployed instance with the storage layout version it was
   initialised under.
2. Detects a version mismatch at call time — before any state is read or
   written — and fails fast with a clear, machine-readable error.
3. Handles *legacy* deployments that predate this mechanism gracefully — they
   have no version tag, so a missing tag must be treated as a valid legacy
   state, not an error.
4. Describes how a future migration would be written and authorized without
   actually performing one today.

---

## Decision

### 1. Constants: `SCHEMA_VERSION: u32` per crate

Every contract crate declares a module-level constant:

```rust
/// Current storage schema version for this contract.
///
/// Increment when a new release changes the layout of any persistent or
/// instance storage key. A matching `migrate()` entrypoint must be
/// implemented and called on every live instance before the new WASM is
/// deployed. See docs/adr/0009-storage-schema-versioning.md.
pub const SCHEMA_VERSION: u32 = 1;
```

The value starts at `1` and is a monotonically increasing integer. It is
**never decremented or reused**.

### 2. Storage key: `schver` (instance storage)

The version is stored under the symbol key `schver` in **instance storage**
so it shares the same lifetime as the other initialization keys (`admin`,
`token`, etc.) and is written exactly once — at construction.

```rust
// written in __constructor, immediately after the other init keys:
write_schema_version(&env, SCHEMA_VERSION);
```

`write_schema_version` is defined in `invofi-common`:

```rust
pub fn write_schema_version(env: &Env, version: u32) {
    env.storage()
        .instance()
        .set(&symbol_short!("schver"), &version);
}
```

### 3. Guard: `assert_schema_version(env, expected)` from `invofi-common`

Every state-mutating public entrypoint calls this guard as its **first
action** (after receiving `env`):

```rust
pub fn some_entrypoint(env: Env, …) {
    assert_not_paused(&env);
    assert_schema_version(&env, SCHEMA_VERSION);
    // … remainder of function
}
```

The function implements the three-case contract:

| Stored `schver` | Behaviour |
|---|---|
| **Absent** (key missing) | Legacy deployment — fall through silently. Reads and writes continue working against the legacy storage shape. |
| **Present, matches `expected`** | Proceed normally. |
| **Present, mismatches `expected`** | Panic with `ContractError::SchemaMismatch` (discriminant **10**). |

```rust
pub fn assert_schema_version(env: &Env, expected: u32) {
    let stored: Option<u32> = env.storage().instance().get(&symbol_short!("schver"));
    match stored {
        None => { /* legacy — fall through */ }
        Some(v) if v == expected => { /* happy path */ }
        Some(_) => env.panic_with_error(ContractError::SchemaMismatch),
    }
}
```

`ContractError::SchemaMismatch` carries discriminant `10` (a **stable**
value that must never be renumbered after deployment). Clients and indexers
can match on `Error(Contract, #10)` without parsing diagnostic strings.

### 4. Exceptions — entrypoints that must NOT call the guard

| Entrypoint | Reason |
|---|---|
| `__constructor` | `schver` doesn't exist yet; `write_schema_version` is called instead. |
| `pause` / `unpause` | Circuit breakers must work even when the contract needs migration. An operator needs to halt a broken deployment. |
| Read-only getters (`get_*`, `contract_is_paused`, `version`, etc.) | Cannot corrupt state. Including the guard is harmless but optional. |

---

## Storage Key Audit (as of v1 schema)

Cross-reference: [Issue #46](https://github.com/Stellar-VaultLink/invofi-contracts/issues/46).

### `common`

| Key | Storage | Owner | Value type |
|---|---|---|---|
| `curtok` | instance | `common` | `Map<Symbol, Address>` — currency → SEP-41 token |
| `token` | instance | `common` | `Address` — legacy single-token fallback |

### `registry`

| Key | Storage | Value type |
|---|---|---|
| `invoices` | persistent | `Map<Symbol, Invoice>` |
| `rates` | persistent | `Map<RiskTier, u32>` |
| `blklist` | persistent | `Vec<Address>` |
| `admin` | instance | `Address` |
| `stats` | instance | `ProtocolStats` |
| `financing` | instance | `Address` |
| `repayment` | instance | `Address` |
| `paused` | instance | `bool` |
| `feebps` | instance | `u32` |
| `schver` | instance | `u32` |

### `financing`

| Key | Storage | Value type |
|---|---|---|
| `offers` | persistent | `Map<Symbol, FinancingOffer>` |
| `lstats` + lender | persistent | `LenderStats` (compound key) |
| `scheds` | persistent | `Map<Symbol, RepaymentSchedule>` |
| `admin` | instance | `Address` |
| `registry` | instance | `Address` |
| `token` | instance | `Address` |
| `repayment` | instance | `Address` |
| `stats` | instance | `ProtocolStats` |
| `paused` | instance | `bool` |
| `postok` | instance | `Address` — position token |
| `feebps` | instance | `u32` |
| `schver` | instance | `u32` |
| `curtok` | instance | `Map<Symbol, Address>` — via `common` |

### `repayment`

| Key | Storage | Value type |
|---|---|---|
| `admin` | instance | `Address` |
| `registry` | instance | `Address` |
| `financing` | instance | `Address` |
| `token` | instance | `Address` |
| `insadd` | instance | `Address` (optional) |
| `repadd` | instance | `Address` (optional) |
| `penbps` | instance | `u32` — penalty rate bps |
| `pencap` | instance | `u32` — penalty cap bps |
| `paused` | instance | `bool` |
| `schver` | instance | `u32` |

### `insurance`

| Key | Storage | Value type |
|---|---|---|
| `stakes` | persistent | `Map<Address, i128>` |
| `admin` | instance | `Address` |
| `token` | instance | `Address` |
| `pooltot` | instance | `i128` |
| `paycall` | instance | `Address` |
| `registry` | instance | `Address` |
| `paused` | instance | `bool` |
| `schver` | instance | `u32` |

### `reputation`

| Key | Storage | Value type |
|---|---|---|
| `reputn` | persistent | `Map<Address, ReputationRecord>` |
| `admin` | instance | `Address` |
| `recorder` | instance | `Address` |
| `paused` | instance | `bool` |
| `schver` | instance | `u32` |

---

## The `migrate()` Pattern (scaffolding — not yet implemented)

> **This section documents the convention for future migrations. No actual
> key renames or value-shape changes are performed in this ADR.**

### Function signature convention

Each contract that needs to migrate its storage layout should expose:

```rust
pub fn migrate(env: Env, admin: Address, from_version: u32, to_version: u32) {
    // 1. Auth: only the admin may call this.
    admin.require_auth();
    let current_admin: Address = env.storage().instance()
        .get(&symbol_short!("admin"))
        .unwrap_or_else(|| panic!("Not initialized"));
    if current_admin != admin {
        env.panic_with_error(ContractError::Unauthorized);
    }

    // 2. Verify we are migrating from exactly the version stored on-chain.
    let stored: Option<u32> = env.storage().instance().get(&symbol_short!("schver"));
    let stored_v = stored.unwrap_or(0); // 0 means legacy (pre-versioning)
    if stored_v != from_version {
        env.panic_with_error(ContractError::InvalidInput);
    }

    // 3. Perform the actual data migration here.
    //    - Rename keys: read old, write new, delete old.
    //    - Reshape values: read, convert, write back.
    //    - Back-fill new keys with defaults.
    //
    //    Example (renaming "foo" → "bar"):
    //    let v: Option<OldType> = env.storage().persistent().get(&symbol_short!("foo"));
    //    if let Some(old) = v {
    //        env.storage().persistent().set(&symbol_short!("bar"), &NewType::from(old));
    //        env.storage().persistent().remove(&symbol_short!("foo"));
    //    }

    // 4. Bump the stored schema version to the new value.
    //    This is the commit point — after this line the contract accepts calls
    //    from the new WASM binary.
    write_schema_version(&env, to_version);
}
```

### Authorization

Only the admin of the contract may call `migrate`. The admin address is
whatever was set in `__constructor` (or updated by `transfer_admin`). No new
role or governance mechanism is introduced — this keeps the migration
surface as small as possible. When multisig governance ships (roadmap), the
admin will already be a multisig, so `migrate` automatically inherits it.

### Deployment procedure for a future migration (step-by-step)

1. **Implement and test `migrate()`** in the affected crate.
   - Write unit tests that pre-populate storage in the old layout, call
     `migrate(env, admin, from, to)`, and assert the new layout is correct
     and the old keys are absent.
   - Write a test that calling `migrate` with the wrong `from_version`
     panics with `InvalidInput`.

2. **Increment `SCHEMA_VERSION`** in the crate's `lib.rs` from `N` to `N+1`.
   - At this point `assert_schema_version` on any un-migrated instance will
     panic with `SchemaMismatch(#10)`, which prevents stale state from being
     read or written.

3. **Deploy the new WASM** to every live instance using `stellar contract install`
   and `stellar contract deploy --wasm-hash`.

4. **Call `migrate(admin, N, N+1)`** on every live instance **before opening
   it to user calls**. Use the admin keypair that corresponds to the `admin`
   key stored on that instance.
   - Automate this step in `scripts/deploy.sh` or a dedicated migration
     script to avoid human error.
   - On Testnet this can be done in a single transaction; on Mainnet, prefer
     a time-locked multisig proposal.

5. **Verify** by calling a read-only getter and confirming no `SchemaMismatch`
   error is returned.

6. **Document** the migration in `CHANGELOG.md` with the from/to version
   numbers and a short description of what changed.

### What `migrate()` must never do

- It must never be callable when already on the target version (`from_version
  == to_version`).
- It must not silently succeed if the stored version doesn't match
  `from_version` — fail loudly with `InvalidInput`.
- It must not leave the contract in a half-migrated state on error. Use
  Soroban's atomic transaction guarantee: if any step panics, the entire
  ledger transaction rolls back, leaving storage unchanged.

---

## Consequences

### Benefits

- **Fail-fast on version mismatch.** Any call to a contract deployed with
  schema v1 storage but a v2 binary panics immediately with a typed,
  machine-readable error (`#10`). No silent data corruption.
- **Backward compatible.** Pre-versioning deployments (absent `schver` key)
  continue to work unchanged — the legacy fallback path is a deliberate part
  of the contract.
- **Uniform pattern.** A single implementation in `invofi-common` means every
  crate gets the same semantics with zero duplication.
- **Auditable.** The `schver` key is visible in Horizon's contract data dump
  for any instance. An indexer can scan all InvoFi instances and flag any
  that are running mismatched versions without calling the contract.

### Trade-offs

- **No partial rollback.** Once `migrate()` advances `schver`, the only way
  to go back is to call a hypothetical `migrate(admin, N+1, N)`. For this
  reason, migrations should be one-way unless a `rollback()` function is
  explicitly implemented and tested.
- **Admin single point of failure.** Until multisig governance ships, a
  compromised admin key can call `migrate` and corrupt storage layout.
  This is an existing risk across all admin operations, not unique to this ADR.
- **Cost of always checking.** `assert_schema_version` adds one instance
  storage read per entrypoint call. On Soroban this is negligible (instance
  reads are cached per transaction).
