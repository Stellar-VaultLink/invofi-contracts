# ADR-0009 — Invoice verification oracle

**Status:** Proposed (2026-08-20)

## Context

Everything the registry knows about an invoice, the originator typed in.
`register_invoice` takes an amount, a currency and a due date on trust; nothing
ties them to a real invoice, a real business, or a business in good standing
with its tax authority. A lender pricing an offer is pricing self-reported data
(issue #181).

The facts that would help — this PDF is the invoice, this company is
registered, this company is current on its filings — live off-chain, in
registries a Soroban contract cannot read.

## Decision

### 1. The trust boundary, stated plainly

The contract **cannot** verify that a document hash corresponds to a real
invoice, or that a business registration number is genuine. No on-chain code
can. What it does is narrower and worth being precise about:

- it authenticates that the attestation came from an address in an
  admin-governed verifier set;
- it stores the statement immutably, with the verifier's identity, the hash of
  the evidence, and the time;
- it requires **m of n** distinct verifiers before a fact counts as verified;
- it expires the statement, so a stale fact stops counting.

Trust is bounded by verifier honesty and by the threshold. It is not
eliminated. Calling this "trust-minimised verification" is fair; calling it
"on-chain fact verification" is not, and the contract documentation says so
rather than letting an integrator infer the stronger claim.

The off-chain oracle service does the actual checking and calls `attest` with
the result. Its correctness is out of scope for this contract, which is exactly
the point of pinning the boundary here.

### 2. m-of-n, with rejection outranking approval

`set_verifier_threshold` sets how many **distinct** verifiers must attest
affirmatively before a verification type reads `Verified`. Distinctness is
structural: `attest` keeps at most one attestation per (verifier, type), so a
single verifier cannot reach a threshold of three by calling three times.
Re-attesting replaces, which is also how a verifier refreshes an expired
statement or corrects a wrong one.

A live rejection makes the type `Rejected` regardless of how many approvals
sit beside it. One verifier saying a document is forged should not be outvoted
by two saying it looked fine — the asymmetry is deliberate, and it is the
conservative direction for a protocol where the downside of financing a forged
invoice is much worse than the downside of not financing a real one.

The threshold is deliberately **not** clamped to the current verifier-set size.
An admin may set it above the set while onboarding verifiers, and removing a
verifier must not silently weaken a threshold that was set on purpose. A
threshold no live set can reach means nothing verifies, which fails safe.

### 3. Verification status is per type; the invoice status is the conjunction

`get_verification_status(invoice_id, v_type)` is the primitive. The aggregate
`get_invoice_verification_status` requires **every** type to be verified: an
invoice with a checked document hash but no business registration is not a
verified invoice. Rejection anywhere dominates; otherwise a lapsed type reads
`Expired`.

### 4. Expiry is derived on read

Soroban has no scheduler. An attestation's `valid_until` cannot flip anything
when it passes, so status is computed from `now > valid_until` at read time and
is correct whether or not anyone has called anything.

`expire_verifications` is a permissionless poke that persists the lapse and
emits `ver_exp` for each newly expired attestation. It exists only so an
indexer has something to observe — a deadline passing is not an event, and
nothing about verification status depends on this being called. It is a no-op
the second time, so a keeper cannot spam duplicate events.

A lapsed *rejection* expires too. A stale "this invoice is bad" is no more
informative than a stale "this invoice is good".

Validity is admin-configurable (default 90 days) and clamped to 1–365 days: an
attestation is a snapshot of a fact that can change without telling the chain,
so it must not be settable to never expire. Changing the setting does not move
the `valid_until` of attestations already submitted; each carries its own,
fixed at submission.

### 5. The fee is charged for the work, not for the answer

`fee = invoice_amount * verification_fee_bps / 10_000`, paid by the invoice
originator to the attesting verifier in the invoice's own currency, via a
SEP-41 `transfer_from` against an allowance the originator granted the registry.

The issue does not say what happens to the fee on a rejected attestation. **It
is charged.** A fee contingent on approval pays verifiers to approve, which
defeats the point of having them. The verifier did the work either way, and the
rejection is the valuable output.

The transfer and the storage write are in the same transaction: no charge
without a record, no record without a charge. The charge happens first, so a
verifier the originator cannot pay never gets an attestation stored.

The multiplication is `checked_mul` on `i128` and the division by 10 000
happens last, so a large invoice cannot wrap into a small or negative fee. The
fee ceiling is 5% — the same ceiling `set_fee` applies to the protocol fee.

**The default is 0 bps, which disables fee settlement entirely.** A deployment
that never configures a fee never touches a token, and the oracle is usable
without one.

### 6. Fees settle in the invoice's currency

The registry reuses the `invofi_common` currency registry that financing
already uses, via its own admin-gated `register_currency`. One entry per
currency, no per-currency branch, and a fee on a EUR invoice is paid in EUR.
Attesting on an invoice in an unregistered currency with a non-zero fee fails
loudly rather than silently skipping the charge.

### 7. Removal revokes a verifier's vote but keeps their history

`remove_verifier` drops an address from the set and leaves its attestations in
storage, but those records stop counting toward any status the moment the
verifier leaves. Both halves matter, and they answer different questions.

**Why the records stay:** who said what, when, is the point of storing them,
and deleting history on removal would make the record unauditable exactly when
an audit matters. `get_verifications` still returns them.

**Why they stop counting:** removal is how an admin withdraws trust, and the
reason is normally that the key is compromised or the verifier was found
negligent. If their prior approvals kept satisfying a threshold, removal would
revoke nothing — the invoice would still read `Verified` on the word of a
verifier the protocol has just disowned. `type_status` therefore evaluates
only the current verifier set.

This is also what makes the eviction policy in decision 8 safe.

### 8. History yields to liveness when an invoice hits its cap

An invoice retains at most `MAX_ATTESTATIONS_PER_INVOICE` (60) attestations —
`MAX_VERIFIERS x 3`, exactly enough for the whole active set to speak on all
three types. But records from departed verifiers keep occupying slots, so an
invoice that rotates through twenty verifiers fills the list and would
otherwise lock out every future verifier permanently. Expiry does not recover
capacity: it rewrites a record's status, it does not remove it.

When the list is full, one slot is therefore freed in a defined order: the
oldest record from a verifier no longer in the set, then the oldest lapsed
record. Both classes are precisely the records that decision 7 already
excludes from status, so eviction can never move a verification type off
`Verified` or `Rejected` as a side effect of an unrelated attestation. Live
records from active verifiers are never dropped; if nothing is evictable the
attestation is refused instead.

## Consequences

- A lender can distinguish an invoice three verifiers vouched for from one
  nobody has looked at, and can see who vouched and when.
- Verifiers are paid per attestation, so running the off-chain service is
  fundable, and no incentive points at approving.
- Attestations decay. A 90-day-old registration check reads `Expired`, not
  `Verified`, and the invoice falls back to unverified until someone re-attests.
- The registry now moves tokens. It previously did not, and this is the only
  path in it that does.
- Nothing in the financing path *requires* verification. Wiring "only verified
  invoices can be financed" into `create_offer` is a policy decision with its
  own migration story for invoices registered before the oracle existed, and is
  deliberately left to a follow-up.

## Alternatives considered

**Put the oracle in its own contract.** Cleaner separation, and it would keep
tokens out of the registry. Rejected for now: attestations are per-invoice data
whose lifecycle is the invoice's, and every read would otherwise be a
cross-contract call from wherever the invoice is being read. The issue also
scopes this to the registry. If the verifier set grows governance of its own,
splitting it out is a contained change.

**Store a verdict per invoice rather than per (verifier, type).** Much smaller
storage, but it destroys the audit trail and makes m-of-n impossible to compute.

**Refund the fee on rejection.** Sounds fair to the originator, and it is the
reading someone might expect. Rejected: it makes rejecting unpaid work, which
is precisely the wrong incentive to give a verifier.

**Let the verifier choose `valid_until`.** Rejected: a verifier could set a
hundred-year validity and make their statement effectively permanent. The
contract owns the clock.
