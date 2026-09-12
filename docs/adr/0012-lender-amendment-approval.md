# ADR-0012: Lender approval for amendments on financed invoices

**Status:** Accepted

## Context

PR #185 introduced the invoice amendment lifecycle (request, approve, reject, audit trail) for `Pending` invoices, where an amendment auto-approves because no lender has committed capital to the invoice yet.

Once an invoice is `Financed`, however, a lender has already disbursed principal to the originator against the invoice's specified amount, due date, and terms. Allowing an originator to unilaterally alter the invoice's due date or face value would undermine the lender's position and create credit risk.

A mechanism is needed to allow originators to propose amendments on `Financed` invoices while granting the active lender sole authority to approve or reject them.

## Decision

1. **Pending Status for Financed Amendments:**
   When `request_amendment` is invoked on an invoice with `InvoiceStatus::Financed`, the amendment record is created with `AmendmentStatus::Pending` rather than being auto-approved. It is recorded in the audit trail without modifying the invoice state.

2. **Lender Authorization via Cross-Contract Lookup:**
   For `Financed` invoices, `approve_amendment` and `reject_amendment` must be authorized by the lender of the active accepted offer:
   - The registry retrieves its configured `financing` contract address.
   - The registry performs a read-only cross-contract call using `FinancingClient::get_offers_by_invoice(invoice_id)`.
   - The active offer is resolved by locating the offer with `status == OfferStatus::Accepted` or `status == OfferStatus::Financed`.
   - The caller must match `active_offer.lender` and provide cryptographic authorization via `caller.require_auth()`. Any other caller (including the invoice originator) reverts with `ContractError::Unauthorized`.

3. **Amount Amendment Bounds:**
   When an `Amount` amendment is approved on a `Financed` invoice:
   - The new amount must be greater than or equal to `MIN_INVOICE_AMOUNT`.
   - The new amount must not be less than the disbursed financing principal (`active_offer.amount`). This invariant ensures that the invoice face value always fully backs the outstanding financing claim.

4. **Event Emission:**
   - `request_amendment` emits `amd_req` carrying `(field, status, timestamp)`.
   - `approve_amendment` emits `amd_apr` carrying `(amendment_index, approver)`.
   - `reject_amendment` emits `amd_rj` carrying `(amendment_index, rejector)`.

## Consequences

- Originators can request extensions or face value adjustments on financed invoices without disrupting live financing state.
- Lenders have explicit, on-chain approval authority over modifications to contracts they have financed.
- Cross-contract reads keep the registry and financing contracts properly decoupled without requiring duplicate storage of offer states inside the registry.
