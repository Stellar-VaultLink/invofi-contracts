#![no_std]

//! Insurance pool contract (Task 9).
//!
//! A flat-pool coverage reserve: stakers deposit the staking token and can
//! withdraw anytime. Payout logic (Task 10) and yield-rate calculation are
//! intentionally out of scope — this contract owns only stake accounting.

use soroban_sdk::{contract, contractimpl, symbol_short, token, Address, Env, Map};

use invofi_common::assert_not_paused;

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
    /// contract that stakers deposit). Must be called once after deployment.
    pub fn initialize(env: Env, admin: Address, token: Address) {
        admin.require_auth();
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
        token_client.transfer(&env.current_contract_address(), &staker, &amount);

        env.events()
            .publish((symbol_short!("pool_un"), staker.clone()), amount);
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
        token::TokenClient::new(&env, &token_addr).balance(&env.current_contract_address())
    }

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod test;
