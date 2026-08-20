# ADR-0008 — Offer amendment and counter-offer

**Status:** Proposed (2026-08-20)

## Context

Financing offers are take-it-or-leave-it. A lender calls `create_offer` with a
rate, an amount and a duration; the originator's only moves are `accept_offer`
and `reject_offer`. Terms that are nearly right die the same way terms that are
absurd do, and the only way to converge is for the lender to withdraw and post a
fresh offer — which loses the audit trail of how the two sides got there
(issue #180).

Two facts about Soroban shape everything below.

**Nobody can spend a counterparty's funds on their behalf.** Settlement moves
principal from the lender to the business with `transfer_from`, which the token
contract will only honour against an allowance the lender granted. "If both
parties agree, auto-execute" therefore cannot mean *the last caller settles both
sides* — the acting party's transaction carries only the acting party's
signature. Any design where a match conjures the counterparty's authorization at
match time does not work; the counterparty's commitment has to already exist when
the match happens.

**There is no scheduler.** A 72-hour window cannot fire anything when it
elapses. Expiry has to be a property that reads true, not a state change that
something performs.

## Decision

### 1. The offer is the lender's position; the history is the record

`amend_offer` writes the new terms onto the `FinancingOffer` itself. The offer
row is always what the lender is currently offering, so an originator who calls
plain `accept_offer` mid-negotiation accepts the amended terms and there is no
second place where "the current terms" could disagree with the first.

`counter_offer` does **not** write to the offer — the offer belongs to the
lender. It appends the originator's proposal to the negotiation history, which
the lender can then meet.

Every round from either side appends a `NegotiationRecord` to
the key `("negot", offer_id)`, so the full path from opening terms to settlement is
on-chain and ordered.

### 2. Auto-accept is a match of two pre-existing commitments

Agreement is **exact equality of the canonical term tuple**
`(amount, interest_rate, duration)`. No tolerance, no rounding, no normalisation
— "these are the same terms" is never a judgement call, in the contract or in a
dispute about it afterwards.

The two directions are not symmetric, and it matters:

- **Originator counters at the lender's standing terms.** This is just
  acceptance spelled differently. The originator's own `require_auth` is on the
  transaction, and the lender's funds move under the allowance they granted this
  contract — exactly the authorization `accept_offer` runs on today.

- **Lender amends onto the originator's live counter-offer.** Here the settling
  call carries the *lender's* signature, and the originator never signs at match
  time. What authorizes financing them is the counter-offer itself: the
  originator recorded that exact term tuple on-chain in a transaction they
  signed. The record is the commitment. The lender's role is to take it or not.

So neither side is ever bound to terms they did not themselves put on the table,
and no signature is invented at match time. What the second case *does* require
is that the standing commitment be bounded and revocable, which is what the next
two decisions are for.

### 3. A recorded counter-offer is bounded and revocable

- **Bounded:** the negotiation window (default 72 h, admin-configurable within
  1 h – 30 days) is the lifetime of every commitment inside it. An originator who
  proposed terms four days ago is not still on the hook for them; `amend_offer`
  onto an expired counter-offer reverts rather than settles.

- **Frozen at open:** the deadline is computed once, when the first round lands,
  and stored. Later rounds do not push it out — otherwise two parties could keep
  a commitment alive indefinitely by ping-ponging — and a later
  `set_negotiation_window` does not move a negotiation already running.

- **Revocable two ways:** proposing again supersedes (only a party's *most
  recent* record is live, so an abandoned proposal is dead), and
  `close_negotiation` ends the negotiation outright. Either party may close
  before the deadline; that is how an originator withdraws consent.

### 4. Every round is version-guarded

`amend_offer` and `counter_offer` both take `expected_round`: the number of
records the caller believes exist. A mismatch reverts.

Without it, a lender who read the negotiation, then had the originator's
counter-offer land first, would have their amendment applied on top of a
term-set they never saw — and could auto-execute against a counter-offer they
did not know existed. Optimistic concurrency turns that into a revert the caller
can retry from fresh state.

### 5. Expiry is derived on read; the event is emitted by whoever pokes

`get_negotiation_status` computes `Expired` from `now > deadline`. It is correct
whether or not anyone has called anything, which is the only way it can be
correct with no scheduler.

`close_negotiation` is the poke that persists the outcome and emits
`negotiation_closed`. After the deadline it is permissionless — the state is
already determined, so who records it does not matter. Before the deadline it is
restricted to the lender and the originator, because then it *is* a state
change: it revokes a live commitment.

`Closed` and `Accepted` are persisted, since both result from an actual call.
An offer that leaves `Pending` by another route ends its negotiation from that
call, and the two routes are distinguishable: acceptance — outright through
`accept_offer` or by term convergence — records `Accepted`, while a withdrawal
or rejection reads as `Closed`. Both announce the end with `neg_clsd`, so an
indexer never loses a negotiation's final state and can tell a settled one from
a walk-away.

### 6. Amended terms cannot exceed what `create_offer` allows

Both entrypoints validate `(amount, interest_rate, duration)` against the same
bounds `create_offer` enforces. A negotiation must not be a route to terms the
offer could not legally have been created with.

Rounds are capped at `MAX_NEGOTIATION_ROUNDS` (20). The history is one
persistent entry read in full on every round, so the cap is what keeps that
entry — and the cost of touching it — bounded.

### 7. Settlement is one code path

The body of `accept_offer` is extracted into `settle_acceptance`, which
`accept_offer` and the auto-accept path both call. Auto-accept moves money,
flips the invoice to `Financed`, mints the position token and emits `off_acc`
by running the same code, not a parallel copy of it that could drift.

The auto-accept path re-reads the invoice and re-checks both parties against the
blacklist before settling. A negotiation can outlive the invoice's `Pending`
status — it can be cancelled or disputed mid-negotiation — and this is the one
path that settles without the counterparty's live signature, so their
eligibility is verified at settlement rather than assumed from when they
proposed.

## Consequences

- A lender can improve their own offer and an originator can name their price,
  with the whole exchange auditable on-chain.
- Convergence costs one transaction, not three: the call that matches settles.
- An originator's counter-offer is a real commitment for up to the window — they
  should treat it as such, and revoke it if they change their mind. This is the
  sharpest edge in the design and the reason for the window, the supersession
  rule and `close_negotiation`.
- `LenderStats.total_offered` moves by the amendment delta rather than being
  re-credited, so it stays a sum over offers rather than over rounds.
- `negotiation_expired` is not an event. Expiry is a derived read; the
  `negotiation_closed` event carries `NegotiationStatus::Expired` when the poke
  records one, and nothing depends on that poke ever happening.
- The literal event topics are `off_amd`, `ctr_off` and `neg_clsd` — this ADR
  uses the descriptive names, but `symbol_short!` caps a topic at 9 characters.
  The README event table lists the emitted topics.

## Alternatives considered

**Escrow the lender's principal into the contract when they amend.** Would make
the lender's commitment explicit rather than allowance-shaped, and would let a
match settle from contract-held funds. Rejected: it locks capital for the whole
window across every negotiation a lender is in, and the allowance already is a
standing, revocable commitment with the same effect and none of the lockup.

**Let the last caller settle both sides unconditionally.** This is the naive
reading of "auto-execute", and it does not work on Soroban — `transfer_from`
fails without the lender's allowance, so the design would break at the first
match rather than misbehave quietly. Recording commitments and matching them is
the version that runs.

**Reset the deadline on every round.** Friendlier to slow negotiators, but it
makes the commitment window unbounded in practice: two parties trading rounds
keep every recorded proposal live forever. Rejected in favour of a fixed window
that the parties can restart by opening a fresh offer.

**Track a separate "current terms" record instead of amending the offer.**
Rejected: two sources of truth for the same fact, and `accept_offer` would have
to know which one wins.
