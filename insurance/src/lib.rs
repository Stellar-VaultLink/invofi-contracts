#![no_std]

//! Insurance pool contract (Tasks 9 + 10).
//!
//! A flat-pool coverage reserve: stakers deposit the staking token and can
//! withdraw anytime. When an invoice defaults, the repayment contract (the
//! configured payout caller) calls `pay_out`, which compensates the lender
//! from the pool up to the pool's available balance and reduces every
//! staker's claim pro-rata so accounting stays exactly consistent (unstake
//! can never exceed actual pool funds).
//!
//! # Flat yield (issue #130)
//!
//! An admin-configurable annual yield rate (in basis points) accrues linearly
//! on each staker's principal from the moment they stake. Yield is computed
//! and paid out when the staker calls `unstake`.
//!
//! Key design decisions:
//! - Rate is stored as a `u32` annual basis-points value (`"yldrate"`,
//!   instance storage). Default is 0 — the pool is economically inert until
//!   an admin sets the rate.
//! - Per-staker stake start-time is tracked in `"stk_ts"` (persistent
//!   `Map<Address, u64>`, ledger timestamp in seconds).
//! - Banked yield (accrued under prior rate slabs) is stored separately in
//!   `"yld_acc"` (persistent `Map<Address, i128>`). When a staker top-ups
//!   their stake or the admin changes the rate, all existing stakers'
//!   outstanding yield is computed at the *current* rate and banked, then the
//!   clock resets. This ensures rate changes are **strictly prospective**.
//! - Formula: `yield = principal × rate_bps × elapsed_secs
//!             / (10_000 × SECONDS_PER_YEAR)`
//!   where `SECONDS_PER_YEAR = 31_536_000`.
//! - No compounding; no per-staker rate differentiation.
//! - Yield is paid out separately from the pool principal — it is *not*
//!   included in `pool_total` bookkeeping — so payouts and unstake accounting
//!   are unaffected unless the pool has enough balance to cover the yield
//!   component on top of the principal.
//! - The `pool_yld` event is emitted on every yield payout.

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, BytesN, Env, Map, String,
    Symbol, Vec,
};

use invofi_common::{assert_not_paused, AdminConfig, ContractError, InvoiceStatus, RegistryClient};

/// Threshold-gated admin check (ADR-0010). See `invofi_common::assert_threshold`.
fn assert_admin(env: &Env, signers: &Vec<Address>) {
    let cfg = invofi_common::load_admin_config(env);
    invofi_common::assert_threshold(env, &cfg, signers);
}

fn pre_upgrade(_env: &Env) {}
fn post_upgrade(_env: &Env) {}

// ─── Constants ────────────────────────────────────────────────────────────────

/// Seconds in a 365-day year. Mirrors `MAX_OFFER_DURATION_SECS` in common.
const SECONDS_PER_YEAR: u64 = 31_536_000;
const BPS_DENOMINATOR: i128 = 10_000;
const RISK_AGE_SECS: u64 = 30 * 86_400;
const RISK_LARGE_INVOICE_AMOUNT: i128 = 1_000_000_000;

/// The three fixed insurance products.  This is deliberately separate from
/// the registry's legacy `RiskTier`: that type controls lender-offer rates,
/// whereas this type is part of the insurance contract's public ABI.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum InsuranceTier {
    Conservative = 0,
    Balanced = 1,
    Aggressive = 2,
}

/// Independently accounted reserve and immutable economics for one tier.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolRecord {
    pub balance: i128,
    pub reserved: i128,
    pub apy_bps: u32,
    pub payout_cap_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
struct TierStakeKey {
    staker: Address,
    tier: InsuranceTier,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
struct TierOfferKey {
    offer_id: Symbol,
    tier: InsuranceTier,
}

fn tier_parameters(tier: InsuranceTier) -> (u32, u32) {
    match tier {
        InsuranceTier::Conservative => (200, 5_000),
        InsuranceTier::Balanced => (500, 7_500),
        InsuranceTier::Aggressive => (1_000, 10_000),
    }
}

fn tier_key(staker: &Address, tier: InsuranceTier) -> TierStakeKey {
    TierStakeKey {
        staker: staker.clone(),
        tier,
    }
}

fn load_tier_pools(env: &Env) -> Map<InsuranceTier, PoolRecord> {
    env.storage()
        .persistent()
        .get(&symbol_short!("pools"))
        .unwrap_or_else(|| Map::new(env))
}

fn load_tier_pool(env: &Env, tier: InsuranceTier) -> PoolRecord {
    let (apy_bps, payout_cap_bps) = tier_parameters(tier);
    load_tier_pools(env).get(tier).unwrap_or(PoolRecord {
        balance: 0,
        reserved: 0,
        apy_bps,
        payout_cap_bps,
    })
}

fn tier_offer_key(offer_id: &Symbol, tier: InsuranceTier) -> TierOfferKey {
    TierOfferKey {
        offer_id: offer_id.clone(),
        tier,
    }
}

fn load_tier_reservations(env: &Env) -> Map<TierOfferKey, i128> {
    env.storage()
        .persistent()
        .get(&symbol_short!("tresrv"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_tier_reservations(env: &Env, reservations: &Map<TierOfferKey, i128>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("tresrv"), reservations);
}

fn load_tier_paid(env: &Env) -> Map<TierOfferKey, i128> {
    env.storage()
        .persistent()
        .get(&symbol_short!("tpaid"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_tier_paid(env: &Env, paid: &Map<TierOfferKey, i128>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("tpaid"), paid);
}

fn save_tier_pool(env: &Env, tier: InsuranceTier, pool: &PoolRecord) {
    let mut pools = load_tier_pools(env);
    pools.set(tier, pool.clone());
    env.storage()
        .persistent()
        .set(&symbol_short!("pools"), &pools);
}

fn load_tier_stakes(env: &Env) -> Map<TierStakeKey, i128> {
    env.storage()
        .persistent()
        .get(&symbol_short!("tstakes"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_tier_stakes(env: &Env, stakes: &Map<TierStakeKey, i128>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("tstakes"), stakes);
}

fn load_tier_timestamps(env: &Env) -> Map<TierStakeKey, u64> {
    env.storage()
        .persistent()
        .get(&symbol_short!("tstk_ts"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_tier_timestamps(env: &Env, timestamps: &Map<TierStakeKey, u64>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("tstk_ts"), timestamps);
}

fn load_tier_accruals(env: &Env) -> Map<TierStakeKey, i128> {
    env.storage()
        .persistent()
        .get(&symbol_short!("tyld_acc"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_tier_accruals(env: &Env, accruals: &Map<TierStakeKey, i128>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("tyld_acc"), accruals);
}

fn tier_yield(principal: i128, apy_bps: u32, elapsed_secs: u64) -> i128 {
    if principal == 0 || elapsed_secs == 0 {
        return 0;
    }
    principal * apy_bps as i128 * elapsed_secs as i128
        / (BPS_DENOMINATOR * SECONDS_PER_YEAR as i128)
}

fn bank_tier_yield(
    key: &TierStakeKey,
    pool: &PoolRecord,
    stakes: &Map<TierStakeKey, i128>,
    timestamps: &mut Map<TierStakeKey, u64>,
    accruals: &mut Map<TierStakeKey, i128>,
    now: u64,
) {
    let principal = stakes.get(key.clone()).unwrap_or(0);
    let start = timestamps.get(key.clone()).unwrap_or(now);
    let earned = tier_yield(principal, pool.apy_bps, now.saturating_sub(start));
    if earned > 0 {
        accruals.set(key.clone(), accruals.get(key.clone()).unwrap_or(0) + earned);
    }
    timestamps.set(key.clone(), now);
}

// ─── Storage Helpers ─────────────────────────────────────────────────────────

fn load_stakes(env: &Env) -> Map<Address, i128> {
    env.storage()
        .persistent()
        .get(&symbol_short!("stakes"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_stakes(env: &Env, map: &Map<Address, i128>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("stakes"), map);
}

fn load_pool_total(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&symbol_short!("pooltot"))
        .unwrap_or(0)
}

fn save_pool_total(env: &Env, total: i128) {
    env.storage()
        .instance()
        .set(&symbol_short!("pooltot"), &total);
}

fn load_token(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&symbol_short!("token"))
        .unwrap_or_else(|| panic!("Not initialized"))
}

// ── Yield storage helpers ────────────────────────────────────────────────────

/// Annual yield rate in basis points (e.g. 500 = 5.00%). Default 0.
fn load_yield_rate(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&symbol_short!("yldrate"))
        .unwrap_or(0)
}

fn save_yield_rate(env: &Env, rate_bps: u32) {
    env.storage()
        .instance()
        .set(&symbol_short!("yldrate"), &rate_bps);
}

/// Stake-start timestamps: `Address -> ledger timestamp (seconds)`.
fn load_stake_ts(env: &Env) -> Map<Address, u64> {
    env.storage()
        .persistent()
        .get(&symbol_short!("stk_ts"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_stake_ts(env: &Env, map: &Map<Address, u64>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("stk_ts"), map);
}

/// Banked (already-accrued) yield waiting for the staker to claim.
fn load_yield_acc(env: &Env) -> Map<Address, i128> {
    env.storage()
        .persistent()
        .get(&symbol_short!("yld_acc"))
        .unwrap_or_else(|| Map::new(env))
}

fn save_yield_acc(env: &Env, map: &Map<Address, i128>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("yld_acc"), map);
}

// ── Insurance claim storage helpers (Issue #137) ───────────────────────────

fn load_total_outstanding(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&symbol_short!("tot_out"))
        .unwrap_or(0)
}

fn save_total_outstanding(env: &Env, total: i128) {
    env.storage()
        .instance()
        .set(&symbol_short!("tot_out"), &total);
}

fn load_reserved(env: &Env, offer_id: &Symbol) -> i128 {
    let map: Map<Symbol, i128> = env
        .storage()
        .persistent()
        .get(&symbol_short!("resrvd"))
        .unwrap_or_else(|| Map::new(env));
    map.get(offer_id.clone()).unwrap_or(0)
}

fn save_reserved(env: &Env, offer_id: &Symbol, amount: i128) {
    let mut map: Map<Symbol, i128> = env
        .storage()
        .persistent()
        .get(&symbol_short!("resrvd"))
        .unwrap_or_else(|| Map::new(env));
    if amount == 0 {
        map.remove(offer_id.clone());
    } else {
        map.set(offer_id.clone(), amount);
    }
    env.storage()
        .persistent()
        .set(&symbol_short!("resrvd"), &map);
}

fn load_paid(env: &Env, offer_id: &Symbol) -> i128 {
    let map: Map<Symbol, i128> = env
        .storage()
        .persistent()
        .get(&symbol_short!("paid"))
        .unwrap_or_else(|| Map::new(env));
    map.get(offer_id.clone()).unwrap_or(0)
}

fn save_paid(env: &Env, offer_id: &Symbol, amount: i128) {
    let mut map: Map<Symbol, i128> = env
        .storage()
        .persistent()
        .get(&symbol_short!("paid"))
        .unwrap_or_else(|| Map::new(env));
    if amount == 0 {
        map.remove(offer_id.clone());
    } else {
        map.set(offer_id.clone(), amount);
    }
    env.storage().persistent().set(&symbol_short!("paid"), &map);
}

// ── Yield math ────────────────────────────────────────────────────────────────

/// Compute the yield earned by `principal` at `rate_bps` over `elapsed_secs`.
///
/// Formula: `principal * rate_bps * elapsed_secs / (10_000 * SECONDS_PER_YEAR)`
///
/// Integer truncation means yield rounds **down** (in the protocol's favour).
///
/// **Overflow analysis** (all multiplications happen before any division):
/// - `principal` for any realistic token supply ≤ ~10^18 stroops
/// - `elapsed_secs` ≤ 31_536_000 (~3.15 × 10^7)
/// - `rate_bps` ≤ 10_000 (100 %)
/// - Worst-case intermediate: 10^18 × 3.15×10^7 × 10^4 = 3.15 × 10^29
/// - i128 max ≈ 1.7 × 10^38  →  comfortable headroom, no overflow
///
/// Multiplying before dividing avoids the precision loss that "divide before
/// multiply" introduces on sub-year or sub-full-principal stakes.
fn compute_yield(principal: i128, rate_bps: u32, elapsed_secs: u64) -> i128 {
    if principal == 0 || rate_bps == 0 || elapsed_secs == 0 {
        return 0;
    }
    // All multiplications first, single division at the end.
    // This preserves maximum precision and avoids the divide-before-multiply
    // precision loss that Scout's detector flags.
    let denominator = SECONDS_PER_YEAR as i128 * 10_000;
    principal * elapsed_secs as i128 * rate_bps as i128 / denominator
}

/// Bank the yield accrued since the staker's last timestamp, then reset the
/// timestamp to `now`. Returns the amount just banked.
///
/// This is called:
/// - On `stake` top-up (so the new principal doesn't inflate historical yield)
/// - On `set_yield_rate` for all existing stakers (so the new rate applies
///   only from this point forward)
/// - On `unstake` just before paying out
fn bank_accrued_yield(
    env: &Env,
    staker: &Address,
    stakes: &Map<Address, i128>,
    timestamps: &mut Map<Address, u64>,
    accruals: &mut Map<Address, i128>,
    now: u64,
) -> i128 {
    let principal = stakes.get(staker.clone()).unwrap_or(0);
    if principal == 0 {
        return 0;
    }
    let start = timestamps.get(staker.clone()).unwrap_or(now);
    let elapsed = now.saturating_sub(start);
    let rate = load_yield_rate(env);
    let earned = compute_yield(principal, rate, elapsed);
    if earned > 0 {
        let prev = accruals.get(staker.clone()).unwrap_or(0);
        accruals.set(staker.clone(), prev + earned);
    }
    // Reset the timestamp so future accrual starts from now.
    timestamps.set(staker.clone(), now);
    earned
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct InsuranceContract;

#[contractimpl]
impl InsuranceContract {
    // ── Initialization / admin ──────────────────────────────────────────────

    /// One-time setup. Sets the admin and the staking token (the SEP-41
    /// contract that stakers deposit).
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run (issue #75).
    pub fn __constructor(env: Env, admin: Address, token: Address) {
        invofi_common::init_admin_config(&env, &admin);
        invofi_common::initialize_contract_version(&env, env!("CARGO_PKG_VERSION"));
        env.storage()
            .instance()
            .set(&symbol_short!("token"), &token);
    }

    /// Returns the primary admin address (the first configured signer). See
    /// `RegistryContract::get_admin` for the same caveat under true M-of-N.
    pub fn get_admin(env: Env) -> Address {
        invofi_common::load_admin_config(&env)
            .signers
            .get(0)
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// The full M-of-N admin governance config. See ADR-0010.
    pub fn get_admin_config(env: Env) -> AdminConfig {
        invofi_common::load_admin_config(&env)
    }

    /// The current signer set.
    pub fn get_signers(env: Env) -> Vec<Address> {
        invofi_common::load_admin_config(&env).signers
    }

    /// The current approval threshold.
    pub fn get_threshold(env: Env) -> u32 {
        invofi_common::load_admin_config(&env).threshold
    }

    /// Reconfigure the admin signer set and threshold. See
    /// `RegistryContract::set_signers`.
    pub fn set_signers(
        env: Env,
        signers: Vec<Address>,
        new_signers: Vec<Address>,
        new_threshold: u32,
    ) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        invofi_common::validate_signers(&env, &new_signers, new_threshold);
        invofi_common::save_admin_config(
            &env,
            &AdminConfig {
                signers: new_signers,
                threshold: new_threshold,
            },
        );
    }

    /// Transfers admin rights, collapsing the config back to a single new
    /// admin. See `RegistryContract::transfer_admin`.
    pub fn transfer_admin(env: Env, signers: Vec<Address>, new_admin: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        let mut new_signers = Vec::new(&env);
        new_signers.push_back(new_admin);
        invofi_common::save_admin_config(
            &env,
            &AdminConfig {
                signers: new_signers,
                threshold: 1,
            },
        );
    }

    /// Swap the staking token. Admin only. Existing stakes are not migrated —
    /// set this before opening the pool to stakers.
    pub fn set_staking_token(env: Env, signers: Vec<Address>, token: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("token"), &token);
    }

    pub fn get_staking_token(env: Env) -> Address {
        load_token(&env)
    }

    /// Configure the address allowed to trigger payouts — this is the
    /// repayment contract. Admin only. Payouts are disabled until a caller
    /// is configured (fail-closed).
    pub fn set_payout_caller(env: Env, signers: Vec<Address>, payout_caller: Address) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("paycall"), &payout_caller);
    }

    pub fn get_payout_caller(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("paycall"))
    }

    /// Configure the registry contract used to verify invoice status in
    /// `pay_out`. Admin only. Payouts on Defaulted invoices are rejected
    /// with a clear error if the registry is not configured or if the
    /// invoice is not in the Defaulted state (fail-closed).
    pub fn set_registry(env: Env, signers: Vec<Address>, registry: Address) {
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("registry"), &registry);
    }

    pub fn get_registry(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("registry"))
    }

    // ── Yield rate (issue #130) ─────────────────────────────────────────────

    /// Set the annual flat yield rate for all stakers, expressed in basis
    /// points (e.g. 500 = 5.00 %).
    ///
    /// **Prospective-only**: before the new rate takes effect, every existing
    /// staker's yield accrued at the *old* rate is computed and banked. From
    /// this point forward the new rate applies. Stakers who unstake after the
    /// rate change receive their previously banked yield (at old rate) plus
    /// yield accrued at the new rate since this call.
    ///
    /// Admin only. Rate may be set to 0 to disable yield entirely.
    pub fn set_yield_rate(env: Env, signers: Vec<Address>, rate_bps: u32) {
        assert_not_paused(&env);
        assert_admin(&env, &signers);

        // Bank outstanding yield for every staker at the current rate before
        // the new rate takes effect. This is the key invariant: rate changes
        // are strictly prospective.
        let stakes = load_stakes(&env);
        let mut timestamps = load_stake_ts(&env);
        let mut accruals = load_yield_acc(&env);
        let now = env.ledger().timestamp();
        let keys: Vec<Address> = stakes.keys();
        for key in keys.iter() {
            bank_accrued_yield(&env, &key, &stakes, &mut timestamps, &mut accruals, now);
        }
        save_stake_ts(&env, &timestamps);
        save_yield_acc(&env, &accruals);

        save_yield_rate(&env, rate_bps);
    }

    /// Read the current annual yield rate in basis points. Default is 0.
    pub fn get_yield_rate(env: Env) -> u32 {
        load_yield_rate(&env)
    }

    /// Returns the fixed economics and isolated balance for `tier`.
    pub fn get_pool(env: Env, tier: InsuranceTier) -> PoolRecord {
        load_tier_pool(&env, tier)
    }

    /// Deterministically recommends a product from the three on-chain risk
    /// inputs. A staker's explicit `stake_tier` choice is never overridden;
    /// this view is for clients presenting the risk information consistently.
    ///
    /// One point is assigned for an invoice older than 30 days, an originator
    /// with more defaults than repayments, and an invoice of at least 1,000
    /// settlement-token units (the protocol's existing 10 XLM/USDC minimum
    /// multiplied by 100). Zero points is Conservative, one is Balanced, and
    /// two or three is Aggressive.
    pub fn assess_risk(
        _env: Env,
        invoice_age_secs: u64,
        originator_repayments: u32,
        originator_defaults: u32,
        invoice_amount: i128,
    ) -> InsuranceTier {
        if invoice_amount <= 0 {
            return InsuranceTier::Aggressive;
        }
        let mut points = 0u32;
        if invoice_age_secs >= RISK_AGE_SECS {
            points += 1;
        }
        if originator_defaults > originator_repayments {
            points += 1;
        }
        if invoice_amount >= RISK_LARGE_INVOICE_AMOUNT {
            points += 1;
        }
        match points {
            0 => InsuranceTier::Conservative,
            1 => InsuranceTier::Balanced,
            _ => InsuranceTier::Aggressive,
        }
    }

    /// Deposit into a selected insurance tier. The existing `stake` entrypoint
    /// remains the legacy flat-pool API; new integrations must use this
    /// tier-aware entrypoint.
    pub fn stake_tier(env: Env, staker: Address, tier: InsuranceTier, amount: i128) {
        assert_not_paused(&env);
        staker.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        let token_addr = load_token(&env);
        token::TokenClient::new(&env, &token_addr).transfer_from(
            &env.current_contract_address(),
            &staker,
            &env.current_contract_address(),
            &amount,
        );

        let key = tier_key(&staker, tier);
        let mut stakes = load_tier_stakes(&env);
        let mut timestamps = load_tier_timestamps(&env);
        let mut accruals = load_tier_accruals(&env);
        let mut pool = load_tier_pool(&env, tier);
        let now = env.ledger().timestamp();
        let existing = stakes.get(key.clone()).unwrap_or(0);
        if existing > 0 {
            bank_tier_yield(&key, &pool, &stakes, &mut timestamps, &mut accruals, now);
        } else {
            timestamps.set(key.clone(), now);
        }
        stakes.set(key.clone(), existing + amount);
        pool.balance += amount;
        save_tier_stakes(&env, &stakes);
        save_tier_timestamps(&env, &timestamps);
        save_tier_accruals(&env, &accruals);
        save_tier_pool(&env, tier, &pool);
        env.events()
            .publish((symbol_short!("pool_stk"), staker, tier), amount);
    }

    /// Withdraw principal and accrued fixed-tier yield from the selected pool.
    pub fn unstake_tier(env: Env, staker: Address, tier: InsuranceTier, amount: i128) {
        assert_not_paused(&env);
        staker.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        let key = tier_key(&staker, tier);
        let mut stakes = load_tier_stakes(&env);
        let balance = stakes.get(key.clone()).unwrap_or(0);
        if balance < amount {
            env.panic_with_error(ContractError::InsufficientBalance);
        }
        let mut pool = load_tier_pool(&env, tier);
        let mut timestamps = load_tier_timestamps(&env);
        let mut accruals = load_tier_accruals(&env);
        let now = env.ledger().timestamp();
        bank_tier_yield(&key, &pool, &stakes, &mut timestamps, &mut accruals, now);
        let yield_payout = accruals.get(key.clone()).unwrap_or(0);
        let remaining = balance - amount;
        if remaining == 0 {
            stakes.remove(key.clone());
            timestamps.remove(key.clone());
            accruals.remove(key.clone());
        } else {
            stakes.set(key.clone(), remaining);
            accruals.set(key.clone(), 0);
        }
        pool.balance -= amount;
        save_tier_stakes(&env, &stakes);
        save_tier_timestamps(&env, &timestamps);
        save_tier_accruals(&env, &accruals);
        save_tier_pool(&env, tier, &pool);
        let token_client = token::TokenClient::new(&env, &load_token(&env));
        token_client.transfer(&env.current_contract_address(), &staker, &amount);
        if yield_payout > 0 {
            token_client.transfer(&env.current_contract_address(), &staker, &yield_payout);
            env.events().publish(
                (symbol_short!("pool_yld"), staker.clone(), tier),
                yield_payout,
            );
        }
        env.events()
            .publish((symbol_short!("pool_un"), staker, tier), amount);
    }

    pub fn get_tier_stake(env: Env, staker: Address, tier: InsuranceTier) -> i128 {
        load_tier_stakes(&env)
            .get(tier_key(&staker, tier))
            .unwrap_or(0)
    }

    /// Preview a selected tier's accrued yield. Uses deterministic integer
    /// arithmetic and rounds down, like the legacy yield preview.
    pub fn accrued_tier_yield(env: Env, staker: Address, tier: InsuranceTier) -> i128 {
        let key = tier_key(&staker, tier);
        let stakes = load_tier_stakes(&env);
        let banked = load_tier_accruals(&env).get(key.clone()).unwrap_or(0);
        let principal = stakes.get(key.clone()).unwrap_or(0);
        let start = load_tier_timestamps(&env)
            .get(key)
            .unwrap_or(env.ledger().timestamp());
        banked
            + tier_yield(
                principal,
                load_tier_pool(&env, tier).apy_bps,
                env.ledger().timestamp().saturating_sub(start),
            )
    }

    // ── Pause / unpause (Task 4A circuit breaker) ───────────────────────────

    pub fn pause(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &true);
    }

    pub fn unpause(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &false);
    }

    pub fn contract_is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("paused"))
            .unwrap_or(false)
    }

    // ── Stake / unstake ─────────────────────────────────────────────────────

    /// Deposit `amount` of the staking token into the insurance pool.
    /// The staker must first approve this contract as a spender (the same
    /// approve + transfer_from pattern accept_offer uses on the financing
    /// contract). Credits the staker's balance and the pool total.
    ///
    /// If the staker already has an existing balance, the yield accrued since
    /// the last stake is banked at the current rate before the new principal
    /// is added, and the clock resets. This ensures top-ups do not
    /// retroactively inflate historical yield.
    pub fn stake(env: Env, staker: Address, amount: i128) {
        assert_not_paused(&env);
        staker.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let token_addr = load_token(&env);
        let token_client = token::TokenClient::new(&env, &token_addr);
        // CEI: External interaction before state mutations. Safe because the
        // token is a trusted standard Soroban token without reentrant hooks.
        token_client.transfer_from(
            &env.current_contract_address(),
            &staker,
            &env.current_contract_address(),
            &amount,
        );

        let mut stakes = load_stakes(&env);
        let mut timestamps = load_stake_ts(&env);
        let mut accruals = load_yield_acc(&env);
        let now = env.ledger().timestamp();

        // If staker already has a balance, bank their yield before top-up so
        // the new principal doesn't inflate historical accrual.
        let existing = stakes.get(staker.clone()).unwrap_or(0);
        if existing > 0 {
            bank_accrued_yield(&env, &staker, &stakes, &mut timestamps, &mut accruals, now);
        } else {
            // First stake — just record the start time.
            timestamps.set(staker.clone(), now);
        }

        stakes.set(staker.clone(), existing + amount);
        save_stakes(&env, &stakes);
        save_stake_ts(&env, &timestamps);
        save_yield_acc(&env, &accruals);

        let mut total = load_pool_total(&env);
        total += amount;
        save_pool_total(&env, total);

        env.events()
            .publish((symbol_short!("pool_stk"), staker.clone()), amount);
    }

    /// Withdraw `amount` back to the staker. Reduces the staker's balance and
    /// the pool total; the pool pays the staker directly from its holdings.
    ///
    /// Any yield accrued since the last bank is computed at the current rate,
    /// added to the banked amount, and transferred to the staker together with
    /// the requested principal. The `pool_yld` event is emitted for the yield
    /// component if non-zero.
    ///
    /// **Emergency withdrawal path (issue #67):** deliberately *not* guarded by
    /// `assert_not_paused`. Withdrawing is a safety valve for stakers — it only
    /// unwinds a staker's own position and cannot mutate the wider protocol —
    /// so it stays available during an emergency pause. `stake` and `pay_out`
    /// remain paused. See ADR-0008.
    pub fn unstake(env: Env, staker: Address, amount: i128) {
        staker.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let mut stakes = load_stakes(&env);
        let balance = stakes.get(staker.clone()).unwrap_or(0);
        if balance < amount {
            env.panic_with_error(ContractError::InsufficientBalance);
        }

        // ── Yield accounting ──────────────────────────────────────────────
        // Bank any yield accrued since the last checkpoint, then flush the
        // entire banked amount to the staker.
        let mut timestamps = load_stake_ts(&env);
        let mut accruals = load_yield_acc(&env);
        let now = env.ledger().timestamp();

        bank_accrued_yield(&env, &staker, &stakes, &mut timestamps, &mut accruals, now);
        let yield_payout = accruals.get(staker.clone()).unwrap_or(0);

        // ── Principal accounting ───────────────────────────────────────────
        let new_balance = balance - amount;
        if new_balance == 0 {
            stakes.remove(staker.clone());
            timestamps.remove(staker.clone());
            accruals.remove(staker.clone());
        } else {
            stakes.set(staker.clone(), new_balance);
            // Clock already reset by bank_accrued_yield; clear banked yield
            // since we're paying it all out now.
            accruals.set(staker.clone(), 0);
        }
        save_stakes(&env, &stakes);
        save_stake_ts(&env, &timestamps);
        save_yield_acc(&env, &accruals);

        let mut total = load_pool_total(&env);
        total -= amount;
        save_pool_total(&env, total);

        let token_addr = load_token(&env);
        let token_client = token::TokenClient::new(&env, &token_addr);
        // CEI: External interactions after all state mutations (Checks-Effects-Interactions).

        // Transfer principal.
        token_client.transfer(&env.current_contract_address(), &staker, &amount);

        // Transfer yield separately (if any). The yield comes from the pool's
        // token balance — the protocol must ensure the contract holds enough
        // tokens to cover accrued yield on top of staked principal.
        if yield_payout > 0 {
            token_client.transfer(&env.current_contract_address(), &staker, &yield_payout);
            env.events()
                .publish((symbol_short!("pool_yld"), staker.clone()), yield_payout);
        }

        env.events()
            .publish((symbol_short!("pool_un"), staker.clone()), amount);
    }

    // ── Payout on default (Task 10) ────────────────────────────────────────

    /// Pay `amount` to `beneficiary` from the pool, capped at the pool's
    /// available balance. Only callable by the configured payout caller (the
    /// repayment contract), authorized via implicit contract-invoker auth.
    ///
    /// **Safety invariants (both checked on-chain before any funds move):**
    ///
    /// 1. The invoice identified by `invoice_id` must be in `Defaulted` status
    ///    in the registry — `Overdue` alone is not sufficient. This is verified
    ///    via a cross-contract read of the registry, so the check cannot be
    ///    spoofed by the caller.
    /// 2. The payout is capped at the pool's available balance. When the pool
    ///    has insufficient funds the contract pays whatever is left and returns
    ///    that amount; it never moves more than `pool_total`. A subsequent call
    ///    against an empty pool returns 0 immediately.
    ///
    /// Every staker's claim is reduced pro-rata by the payout ratio, and the
    /// pool total drops by exactly the amount paid — so get_stake sums always
    /// equal get_pool_total and unstake can never overdraw the pool.
    ///
    /// Returns the amount actually paid (may be less than `amount` when the
    /// pool is short).
    pub fn pay_out(env: Env, invoice_id: Symbol, beneficiary: Address, amount: i128) -> i128 {
        assert_not_paused(&env);
        let payout_caller: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("paycall"))
            .unwrap_or_else(|| panic!("No payout caller configured"));
        payout_caller.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }

        // ── Safety check 1: invoice must be Defaulted, not merely Overdue ──
        // Cross-contract read from the registry provides on-chain proof that
        // the credit-loss event has been finalized before any staked funds move.
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Registry not configured"));
        let registry_client = RegistryClient::new(&env, &registry_addr);
        let invoice = registry_client.get_invoice(&invoice_id);
        if invoice.status != InvoiceStatus::Defaulted {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        // ── Safety check 2: payout ≤ available pool balance ───────────────
        // `amount.min(pool_total)` ensures we never transfer more than the
        // pool holds. An empty pool (pool_total == 0) returns 0 immediately —
        // the `off_def` protocol event on the caller side carries payout=0 so
        // indexers can track the shortfall.
        let pool_total = load_pool_total(&env);
        let payout = amount.min(pool_total);
        if payout <= 0 {
            // Pool depleted — nothing to pay out.
            return 0;
        }

        // Pro-rata reduction of staker balances. Iterates a key snapshot so
        // the reduction math is deterministic; the final staker absorbs any
        // integer-division remainder so reductions sum exactly to `payout`.
        let mut stakes = load_stakes(&env);
        let keys: Vec<Address> = stakes.keys();
        let n = keys.len() as usize;
        if n > 0 {
            let mut reductions: i128 = 0;
            for (i, key) in keys.iter().enumerate() {
                let balance = stakes.get(key.clone()).unwrap_or(0);
                let reduction = if i == n - 1 {
                    payout - reductions
                } else {
                    (balance * payout / pool_total).min(payout - reductions)
                };
                let new_balance = balance - reduction;
                if new_balance == 0 {
                    stakes.remove(key.clone());
                } else {
                    stakes.set(key.clone(), new_balance);
                }
                reductions += reduction;
            }
            save_stakes(&env, &stakes);
        }
        save_pool_total(&env, pool_total - payout);

        let token_addr = load_token(&env);
        // CEI: External interaction after state mutations (Effects before Interactions). Compliant.
        token::TokenClient::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &payout,
        );

        env.events()
            .publish((symbol_short!("pool_pay"), beneficiary.clone()), payout);
        payout
    }

    /// Pay a default claim from one selected tier. The payout is capped by
    /// both the tier's configured coverage percentage and that tier's own
    /// balance; no other tier can fund or absorb this loss.
    pub fn pay_out_tier(
        env: Env,
        invoice_id: Symbol,
        tier: InsuranceTier,
        beneficiary: Address,
        amount: i128,
    ) -> i128 {
        assert_not_paused(&env);
        let payout_caller: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("paycall"))
            .unwrap_or_else(|| panic!("No payout caller configured"));
        payout_caller.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        let registry_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("registry"))
            .unwrap_or_else(|| panic!("Registry not configured"));
        if RegistryClient::new(&env, &registry_addr)
            .get_invoice(&invoice_id)
            .status
            != InvoiceStatus::Defaulted
        {
            env.panic_with_error(ContractError::InvalidTransition);
        }

        let mut pool = load_tier_pool(&env, tier);
        let capped_claim = amount * pool.payout_cap_bps as i128 / BPS_DENOMINATOR;
        let payout = capped_claim.min(pool.balance);
        if payout <= 0 {
            return 0;
        }
        let mut stakes = load_tier_stakes(&env);
        let all_keys: Vec<TierStakeKey> = stakes.keys();
        let mut keys = Vec::new(&env);
        for key in all_keys.iter() {
            if key.tier == tier {
                keys.push_back(key);
            }
        }
        let count = keys.len() as usize;
        let original_balance = pool.balance;
        let mut reductions = 0;
        for (index, key) in keys.iter().enumerate() {
            let balance = stakes.get(key.clone()).unwrap_or(0);
            let reduction = if index == count - 1 {
                payout - reductions
            } else {
                (balance * payout / original_balance).min(payout - reductions)
            };
            if reduction == balance {
                stakes.remove(key.clone());
            } else {
                stakes.set(key.clone(), balance - reduction);
            }
            reductions += reduction;
        }
        // A tier pool can only hold funds represented by tier stakes; this
        // guard fails closed if a corrupted record would violate that invariant.
        if reductions != payout {
            env.panic_with_error(ContractError::InsufficientBalance);
        }
        pool.balance -= payout;
        save_tier_stakes(&env, &stakes);
        save_tier_pool(&env, tier, &pool);
        token::TokenClient::new(&env, &load_token(&env)).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &payout,
        );
        env.events()
            .publish((symbol_short!("pool_pay"), beneficiary, tier), payout);
        payout
    }

    /// Reserve capacity from one tier for an offer. The cap is applied when
    /// reserving, so several partial claims cannot cumulatively exceed the
    /// selected tier's coverage percentage of the original request.
    pub fn reserve_payout_tier(
        env: Env,
        offer_id: Symbol,
        tier: InsuranceTier,
        amount: i128,
    ) -> i128 {
        assert_not_paused(&env);
        let caller: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("paycall"))
            .unwrap_or_else(|| panic!("No payout caller configured"));
        caller.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        let mut pool = load_tier_pool(&env, tier);
        let key = tier_offer_key(&offer_id, tier);
        let mut reservations = load_tier_reservations(&env);
        let already_reserved = reservations.get(key.clone()).unwrap_or(0);
        let capped_total = amount * pool.payout_cap_bps as i128 / BPS_DENOMINATOR;
        let reserve = capped_total
            .saturating_sub(already_reserved)
            .min(pool.balance - pool.reserved);
        if reserve <= 0 {
            return 0;
        }
        reservations.set(key, already_reserved + reserve);
        pool.reserved += reserve;
        save_tier_reservations(&env, &reservations);
        save_tier_pool(&env, tier, &pool);
        env.events()
            .publish((symbol_short!("ins_rsrv"), offer_id, tier), reserve);
        reserve
    }

    /// Consume a tier-specific reservation. The reservation and all principal
    /// reductions are scoped by the same tier key, preventing cross-pool use.
    pub fn claim_payout_tier(
        env: Env,
        offer_id: Symbol,
        tier: InsuranceTier,
        lender: Address,
        amount: i128,
    ) -> (i128, i128) {
        assert_not_paused(&env);
        let caller: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("paycall"))
            .unwrap_or_else(|| panic!("No payout caller configured"));
        caller.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }
        let key = tier_offer_key(&offer_id, tier);
        let reservations = load_tier_reservations(&env);
        let reserved = reservations.get(key.clone()).unwrap_or(0);
        let mut paid_map = load_tier_paid(&env);
        let paid_before = paid_map.get(key.clone()).unwrap_or(0);
        let remaining = reserved - paid_before;
        if remaining <= 0 {
            env.panic_with_error(ContractError::InsufficientBalance);
        }
        let mut pool = load_tier_pool(&env, tier);
        let paid = amount.min(remaining).min(pool.balance);
        if paid <= 0 {
            return (0, remaining);
        }
        let mut stakes = load_tier_stakes(&env);
        let all_keys: Vec<TierStakeKey> = stakes.keys();
        let mut keys = Vec::new(&env);
        for stake_key in all_keys.iter() {
            if stake_key.tier == tier {
                keys.push_back(stake_key);
            }
        }
        let original_balance = pool.balance;
        let mut reductions = 0;
        let count = keys.len() as usize;
        for (index, stake_key) in keys.iter().enumerate() {
            let balance = stakes.get(stake_key.clone()).unwrap_or(0);
            let reduction = if index == count - 1 {
                paid - reductions
            } else {
                (balance * paid / original_balance).min(paid - reductions)
            };
            if reduction == balance {
                stakes.remove(stake_key);
            } else {
                stakes.set(stake_key, balance - reduction);
            }
            reductions += reduction;
        }
        if reductions != paid {
            env.panic_with_error(ContractError::InsufficientBalance);
        }
        paid_map.set(key, paid_before + paid);
        pool.balance -= paid;
        pool.reserved -= paid;
        save_tier_stakes(&env, &stakes);
        save_tier_paid(&env, &paid_map);
        save_tier_pool(&env, tier, &pool);
        token::TokenClient::new(&env, &load_token(&env)).transfer(
            &env.current_contract_address(),
            &lender,
            &paid,
        );
        let remaining_after = remaining - paid;
        env.events().publish(
            (symbol_short!("ins_pay"), offer_id, tier),
            (paid, remaining_after),
        );
        (paid, remaining_after)
    }

    // ── Reserve + partial claim (Issue #137) ──────────────────────────────

    /// Reserve `amount` from the pool for a specific offer's insurance claim.
    /// This locks funds so they cannot be claimed by other offers. The reserved
    /// amount is tracked per-offer and reduces the effective pool balance for
    /// other operations.
    ///
    /// `amount` must be <= pool available (pool_total - total_reserved).
    /// Only callable by the configured payout caller (the repayment contract).
    ///
    /// Returns the amount actually reserved.
    pub fn reserve_payout(env: Env, offer_id: Symbol, amount: i128) -> i128 {
        assert_not_paused(&env);
        let payout_caller: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("paycall"))
            .unwrap_or_else(|| panic!("No payout caller configured"));
        payout_caller.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }

        let pool_total = load_pool_total(&env);
        let total_outstanding = load_total_outstanding(&env);
        let available = pool_total - total_outstanding;
        let reserved = amount.min(available);
        if reserved <= 0 {
            return 0;
        }

        save_reserved(&env, &offer_id, load_reserved(&env, &offer_id) + reserved);
        save_total_outstanding(&env, total_outstanding + reserved);

        env.events()
            .publish((symbol_short!("ins_rsrv"), offer_id.clone()), reserved);
        reserved
    }

    /// Claim a partial payout from the insurance pool for a specific offer.
    /// The caller must be the configured payout caller. The claim amount is
    /// bounded by:
    ///   - `amount` (requested)
    ///   - pool available (pool_total - other reservations)
    ///   - reserved for this offer
    ///
    /// Returns (paid, remaining_reserved) where paid is the amount actually
    /// transferred and remaining_reserved is what's still locked for this offer.
    pub fn claim_payout(env: Env, offer_id: Symbol, lender: Address, amount: i128) -> (i128, i128) {
        assert_not_paused(&env);
        let payout_caller: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("paycall"))
            .unwrap_or_else(|| panic!("No payout caller configured"));
        payout_caller.require_auth();
        if amount <= 0 {
            env.panic_with_error(ContractError::InvalidInput);
        }

        // ── Bounded claim: amount <= reserved for this offer ────────────
        let reserved = load_reserved(&env, &offer_id);
        let already_paid = load_paid(&env, &offer_id);
        let remaining_reserved = reserved - already_paid;
        if remaining_reserved <= 0 {
            env.panic_with_error(ContractError::InsufficientBalance);
        }
        let claim_amount = amount.min(remaining_reserved);

        // ── Bounded claim: amount <= pool available ─────────────────────
        let total_outstanding = load_total_outstanding(&env);
        let pool_total = load_pool_total(&env);
        let pool_available = pool_total - (total_outstanding - remaining_reserved);
        let paid = claim_amount.min(pool_available);
        if paid <= 0 {
            return (0, remaining_reserved);
        }

        // ── Update paid tracking ────────────────────────────────────────
        save_paid(&env, &offer_id, already_paid + paid);
        save_total_outstanding(&env, total_outstanding - paid);

        // ── Pro-rata reduction of staker balances ───────────────────────
        let mut stakes = load_stakes(&env);
        let keys: Vec<Address> = stakes.keys();
        let n = keys.len() as usize;
        if n > 0 {
            let mut reductions: i128 = 0;
            for (i, key) in keys.iter().enumerate() {
                let balance = stakes.get(key.clone()).unwrap_or(0);
                let reduction = if i == n - 1 {
                    paid - reductions
                } else {
                    (balance * paid / pool_total).min(paid - reductions)
                };
                let new_balance = balance - reduction;
                if new_balance == 0 {
                    stakes.remove(key.clone());
                } else {
                    stakes.set(key.clone(), new_balance);
                }
                reductions += reduction;
            }
            save_stakes(&env, &stakes);
        }
        save_pool_total(&env, pool_total - paid);

        // ── Transfer tokens ─────────────────────────────────────────────
        let token_addr = load_token(&env);
        token::TokenClient::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &lender,
            &paid,
        );

        // ── Emit event ──────────────────────────────────────────────────
        let new_remaining = remaining_reserved - paid;
        env.events().publish(
            (symbol_short!("ins_pay"), offer_id.clone()),
            (paid, new_remaining),
        );

        (paid, new_remaining)
    }

    /// Get the reserved amount for an offer.
    pub fn get_reserved(env: Env, offer_id: Symbol) -> i128 {
        load_reserved(&env, &offer_id)
    }

    /// Get the amount already paid out for an offer.
    pub fn get_paid(env: Env, offer_id: Symbol) -> i128 {
        load_paid(&env, &offer_id)
    }

    // ── Query helpers ───────────────────────────────────────────────────────

    /// The staker's current staked balance (0 if never staked).
    pub fn get_stake(env: Env, staker: Address) -> i128 {
        load_stakes(&env).get(staker).unwrap_or(0)
    }

    /// The accounting total of all staked tokens in the pool.
    pub fn get_pool_total(env: Env) -> i128 {
        load_pool_total(&env)
    }

    /// Number of addresses currently holding a non-zero stake.
    pub fn get_stakers_count(env: Env) -> u32 {
        load_stakes(&env).len()
    }

    /// Audit helper: the actual token balance this contract holds. Should
    /// equal get_pool_total whenever stake accounting is correct.
    pub fn get_contract_token_balance(env: Env) -> i128 {
        let token_addr = load_token(&env);
        // CEI: Read-only cross-contract call.
        token::TokenClient::new(&env, &token_addr).balance(&env.current_contract_address())
    }

    /// Compute the total accrued yield for `staker` at the current moment:
    /// previously banked yield plus yield earned since the last checkpoint at
    /// the current rate. This is a read-only preview; no state is mutated.
    pub fn accrued_yield(env: Env, staker: Address) -> i128 {
        let stakes = load_stakes(&env);
        let timestamps = load_stake_ts(&env);
        let accruals = load_yield_acc(&env);

        let principal = stakes.get(staker.clone()).unwrap_or(0);
        let banked = accruals.get(staker.clone()).unwrap_or(0);
        if principal == 0 {
            return banked;
        }
        let now = env.ledger().timestamp();
        let start = timestamps.get(staker.clone()).unwrap_or(now);
        let elapsed = now.saturating_sub(start);
        let rate = load_yield_rate(&env);
        banked + compute_yield(principal, rate, elapsed)
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        invofi_common::contract_version(&env)
    }

    pub fn upgrade(
        env: Env,
        signers: Vec<Address>,
        current_wasm_hash: BytesN<32>,
        new_wasm_hash: BytesN<32>,
        new_version: String,
    ) {
        assert_admin(&env, &signers);
        invofi_common::begin_upgrade(&env, &current_wasm_hash, &new_wasm_hash, &new_version);
        pre_upgrade(&env);
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    pub fn post_upgrade(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        post_upgrade(&env);
        invofi_common::complete_upgrade(&env);
    }

    pub fn rollback(env: Env, signers: Vec<Address>) {
        assert_admin(&env, &signers);
        let (wasm_hash, version) = invofi_common::rollback_target(&env);
        invofi_common::commit_rollback(&env, &version);
        env.deployer().update_current_contract_wasm(wasm_hash);
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
