#![no_std]

//! Insurance pool contract (Tasks 9 + 10).
//!
//! A flat-pool coverage reserve: stakers deposit the staking token and can
//! withdraw anytime. When an invoice defaults, the repayment contract (the
//! configured payout caller) calls `pay_out`, which compensates the lender
//! from the pool up to the pool's available balance and reduces every
//! staker's claim pro-rata so accounting stays exactly consistent (unstake
//! can never exceed actual pool funds). Yield-rate calculation stays out of
//! scope.
//!
//! Task 10 security: pay_out verifies invoice.status == Defaulted on-chain.

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Map, Symbol, Vec};

use invofi_common::{assert_not_paused, InvoiceStatus, RegistryClient};

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
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("Already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("token"), &token);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Transfers admin rights. Only current admin.
    pub fn transfer_admin(env: Env, admin: Address, new_admin: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can transfer admin rights");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &new_admin);
    }

    /// Swap the staking token. Admin only. Existing stakes are not migrated —
    /// set this before opening the pool to stakers.
    pub fn set_staking_token(env: Env, admin: Address, token: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can set the staking token");
        }
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
    pub fn set_payout_caller(env: Env, admin: Address, payout_caller: Address) {
        assert_not_paused(&env);
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can set the payout caller");
        }
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
    pub fn set_registry(env: Env, admin: Address, registry: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only the current admin can set the registry");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("registry"), &registry);
    }

    pub fn get_registry(env: Env) -> Option<Address> {
        env.storage().instance().get(&symbol_short!("registry"))
    }

    // ── Pause / unpause (Task 4A circuit breaker) ───────────────────────────

    pub fn pause(env: Env, admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only admin can pause");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("paused"), &true);
    }

    pub fn unpause(env: Env, admin: Address) {
        admin.require_auth();
        let current: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"));
        if current != admin {
            panic!("Only admin can unpause");
        }
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
    pub fn stake(env: Env, staker: Address, amount: i128) {
        assert_not_paused(&env);
        staker.require_auth();
        assert!(amount > 0, "stake amount must be greater than zero");

        let token_addr = load_token(&env);
        let token_client = token::TokenClient::new(&env, &token_addr);
        // CEI: External interaction before state mutations. Safe because the token is a trusted standard Soroban token without reentrant hooks.
        token_client.transfer_from(
            &env.current_contract_address(),
            &staker,
            &env.current_contract_address(),
            &amount,
        );

        let mut stakes = load_stakes(&env);
        let balance = stakes.get(staker.clone()).unwrap_or(0);
        stakes.set(staker.clone(), balance + amount);
        save_stakes(&env, &stakes);

        let mut total = load_pool_total(&env);
        total += amount;
        save_pool_total(&env, total);

        env.events()
            .publish((symbol_short!("pool_stk"), staker.clone()), amount);
    }

    /// Withdraw `amount` back to the staker. Reduces the staker's balance and
    /// the pool total; the pool pays the staker directly from its holdings.
    pub fn unstake(env: Env, staker: Address, amount: i128) {
        assert_not_paused(&env);
        staker.require_auth();
        assert!(amount > 0, "unstake amount must be greater than zero");

        let mut stakes = load_stakes(&env);
        let balance = stakes.get(staker.clone()).unwrap_or(0);
        assert!(balance >= amount, "Insufficient stake");

        let new_balance = balance - amount;
        if new_balance == 0 {
            stakes.remove(staker.clone());
        } else {
            stakes.set(staker.clone(), new_balance);
        }
        save_stakes(&env, &stakes);

        let mut total = load_pool_total(&env);
        total -= amount;
        save_pool_total(&env, total);

        let token_addr = load_token(&env);
        let token_client = token::TokenClient::new(&env, &token_addr);
        // CEI: External interaction after state mutations (Effects before Interactions). Compliant.
        token_client.transfer(&env.current_contract_address(), &staker, &amount);

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
        assert!(amount > 0, "payout amount must be greater than zero");

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
            panic!("Invoice is not Defaulted");
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

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod test;
