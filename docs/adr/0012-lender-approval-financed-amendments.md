# ADR-0012 — Lender approval for amendments on Financed invoices

**Status:** Accepted (2026-09-12)

## Context

PR #185 introduced the invoice amendment flow (`request_amendment`, `approve_amendment`, `reject_amendment`, `get_amendments`), scoped to `Pending` invoices where the amendment auto-approves immediately because no lender or active funding commitment exists yet.

Once an invoice is `Financed`, financial obligations exist between the originator and the active lender (the provider of financing capital). Changes to core commercial invoice terms (such as face-value `Amount` or `DueDate`) directly impact the debtor's repayment terms, timeline, and risk profile. Therefore, amendments on `Financed` invoices require explicit authorization by the active lender.

## Decision

1. **Two-Stage Workflow on Financed Invoices**:
   - `request_amendment`: Originator can request an amendment on `InvoiceStatus::Financed` invoices. Unlike `Pending` invoices, the amendment is recorded with `AmendmentStatus::Pending` without mutating invoice state.
   - `approve_amendment` / `reject_amendment`: Only the active lender who financed the invoice can approve or reject the amendment.

2. **Lender Resolution Mechanism (Dual Strategy)**:
   - *Primary Registry Storage*: A persistent registry mapping `symbol_short!("invlend")` stores `(invoice_id -> lender)`. This enables $O(1)$ lookup and isolated contract unit testing.
   - *Cross-Contract Lookup Fallback*: If not cached locally, the registry queries the configured financing contract via `FinancingClient::get_offers_by_invoice(invoice_id)`. The active accepted/financed offer (`status == OfferStatus::Accepted || status == OfferStatus::Financed`) yields the authoritative lender address, which is cached for future checks.

3. **Re-Validation and Face-Value vs Loan Reconciliation**:
   - At `approve_amendment`, the proposed value is strictly re-validated:
     - `Amount`: Must satisfy `new_amount >= MIN_INVOICE_AMOUNT`.
     - `DueDate`: Must satisfy `new_due_date > ledger.timestamp()`.
   - *Commercial Face-Value Choice*: Amending the invoice `amount` reflects commercial adjustments between originator and customer/debtor. It does **not** alter the already-funded offer loan terms (principal disbursed or position tokens minted 1:1 at `accept_offer` per ADR-0002). Reconciling position-token supply is deliberately avoided to keep tokenomics immutable post-funding, while requiring lender approval gives the lender complete sovereignty over whether to permit face-value reductions.

4. **Event Emission & Audit Trail**:
   - `amd_req`: Emits `(field, status, timestamp)`. For `Financed` invoices, `status` is `AmendmentStatus::Pending` (distinguishing it from the `Approved` status emitted for `Pending` invoices).
   - `amd_apr` & `amd_rj`: Emits `(amendment_index, approver/rejector)`, carrying the lender's address on `Financed` invoices.
   - Complete audit trail is preserved in `get_amendments`.

## Alternatives Considered

- **Changing the `Invoice` struct** — Rejected: Prohibited by schema versioning (#66); adding fields requires storage migration.
- **Auto-adjusting offer principal on invoice amount changes** — Rejected: The loan was already disbursed on-chain; adjusting principal after funds have transferred creates credit risk and violates position token invariants (ADR-0002).
- **Sole cross-contract read without registry caching** — Rejected: Fails unit test suites where external contracts are mocked via dummy addresses.
