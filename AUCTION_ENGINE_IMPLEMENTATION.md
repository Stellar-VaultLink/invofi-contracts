# Competitive Auction Engine Implementation

## Summary

Implemented a competitive auction engine for the InvoFi financing contract that allows multiple lenders to submit offers on an invoice with automatic selection of the best offer based on a time-weighted scoring algorithm.

## Features Implemented

### 1. Auction Configuration (Admin-Configurable)
- **Max offers per invoice**: Configurable limit (default: 10, max: 50)
- **Auction deadline**: Time after invoice creation when auction closes (default: 7 days, range: 1 hour - 30 days)
- **Offer expiry**: Time after offer creation when offer expires (default: 48 hours, range: 1 hour - 30 days)

### 2. Multiple Offers Per Invoice
- Each invoice can receive multiple pending offers from different lenders
- System tracks and enforces the configured maximum offers per invoice
- First offer on an invoice initializes auction metadata with start time and close deadline

### 3. Time-Weighted Offer Scoring
The scoring algorithm evaluates offers based on three factors:

```
score = (10,000,000 / rate_bps) * time_bonus * amount_factor / 100,000,000
```

- **Base score**: Inverse of interest rate (lower rate = higher score)
- **Time bonus**: up to 10% bonus for early submissions
  - Formula: `1.0 + (0.1 * (1 - relative_position))`
  - Earliest offers get 1.1x multiplier, latest get 1.0x
- **Amount factor**: Up to 10% bonus for larger amounts
  - Bonus for offering more than invoice amount, capped at 10%

### 4. Auto-Accept Best Offer
- `close_auction()` can be called by anyone after auction deadline
- Automatically selects and accepts the offer with the highest score
- Emits `auction_closed` and `best_offer_selected` events
- Follows same settlement flow as manual acceptance

### 5. Offer Expiry
- `expire_offers()` processes and marks expired offers as Rejected
- Batch processing with configurable limit to prevent unbounded gas costs
- Updates lender stats and emits `offer_expired` events
- Can be called by anyone (permissionless cleanup)

### 6. Manual Accept Option
- Originators can still manually accept any pending offer during the auction
- Provides flexibility to choose specific lenders or terms despite scoring

### 7. Events Emitted
- `off_sub` (offer_submitted): When a new offer is created
- `auc_cls` (auction_closed): When auction deadline is reached and closed
- `best_sel` (best_offer_selected): When best offer is auto-accepted
- `off_exp` (offer_expired): When an offer expires

## Code Changes

### common/src/lib.rs
**New types:**
- `AuctionConfig`: Configuration struct with max_offers, deadline, expiry
- `AuctionMetadata`: Per-invoice auction state tracking

**New constants:**
- Auction configuration bounds (MIN/MAX for deadline, expiry, max offers)
- Defaults: 10 max offers, 7 day deadline, 48 hour expiry

**New functions:**
- `calculate_time_bonus()`: Time-based scoring bonus calculation
- `calculate_amount_factor()`: Amount-based scoring bonus calculation
- `calculate_offer_score()`: Overall offer scoring algorithm

**Modified types:**
- `FinancingOffer`: Added `created_at: u64` field for submission timestamp

### financing/src/lib.rs
**New storage helpers:**
- `load/save_auction_config()`: Auction configuration persistence
- `load/save_auction_metadata()`: Per-invoice auction state

**New methods:**
- `set_auction_config()`: Admin configuration of auction parameters
- `get_auction_config()`: Query current auction configuration
- `get_auction_metadata()`: Query auction state for an invoice
- `get_best_offer()`: Find highest-scoring pending offer for invoice
- `get_offer_score()`: Calculate and return score for specific offer (transparency)
- `close_auction()`: Auto-accept best offer after deadline
- `expire_offers()`: Batch expire old offers
- `count_pending_offers_for_invoice()`: Helper to enforce max offers limit

**Modified methods:**
- `create_offer()`: 
  - Enforces max offers per invoice limit
  - Stores `created_at` timestamp
  - Initializes auction metadata on first offer
  - Emits `offer_submitted` event

### financing/src/test.rs
**New tests (18 comprehensive tests):**
- Auction configuration validation
- Multiple offers per invoice
- Max offers enforcement
- Auction metadata creation
- Best offer selection (lowest rate wins)
- Time-weighted scoring with early bonus
- Close auction auto-accept
- Premature close failure
- Offer expiry
- Manual accept during auction
- Created_at timestamp recording
- Amount factor bonus
- Score transparency query
- Event emissions

## Acceptance Criteria Met

✅ Multiple offers stored per invoice (configurable max, default 10)
✅ Scoring algorithm implemented and tested
  - Time-weighted: earlier offers get bonus
  - Rate-weighted: lower rates score higher  
  - Amount-weighted: larger amounts get bonus
✅ Auto-accept on auction close works
✅ Manual accept still available
✅ Offer expiry enforced
✅ Events emitted for all auction lifecycle
✅ All tests pass
✅ No compilation errors

## Usage Example

```rust
// Admin configures auction parameters
financing.set_auction_config(&admin, &20u32, &(86_400u64 * 7), &(86_400u64 * 2));

// Lenders submit offers
financing.create_offer(&offer_id1, &invoice_id, &lender1, &amount, &currency, &400u32, &duration);
financing.create_offer(&offer_id2, &invoice_id, &lender2, &amount, &currency, &450u32, &duration);

// Query best offer
let best = financing.get_best_offer(&invoice_id);

// After deadline, anyone can close auction
financing.close_auction(&invoice_id, &caller);

// Or originator can manually accept anytime
financing.accept_offer(&offer_id2, &originator);

// Cleanup expired offers
financing.expire_offers(&caller, &50u32);
```

## Related Issues
- Related to: #135 (competitive auction / best-offer selection helper)
- Related to: #140 (offer expiry and cleanup sweep)
- Implements: #169 (competitive auction engine with time-weighted offer scoring)

## Security Considerations
- Time bonuses are capped at 10% to prevent gaming
- Amount bonuses are capped at 10% to prevent spam large offers
- Max offers per invoice prevents DOS attacks
- Batch expiry processing prevents unbounded gas costs
- All state transitions follow CEI pattern
- Cross-contract calls are read-only before state mutations
