//! Shared helpers for the InvoFi invoice registry contract.
//!
//! Cross-cutting concerns live here so the contract stays small and auditable:
//! the currency → SEP-41 token registry is the single source of truth for
//! which token contract moves funds for a given currency symbol. Adding a
//! third currency means one `register_currency` call — never a new branch in
//! every function that touches money.

use soroban_sdk::{symbol_short, Address, Env, Map, Symbol};

/// Load the currency registry (an empty map if none has been configured).
pub fn load_currency_registry(env: &Env) -> Map<Symbol, Address> {
    env.storage()
        .instance()
        .get(&symbol_short!("curtok"))
        .unwrap_or_else(|| Map::new(env))
}

/// Persist the currency registry.
pub fn save_currency_registry(env: &Env, registry: &Map<Symbol, Address>) {
    env.storage()
        .instance()
        .set(&symbol_short!("curtok"), registry);
}

/// Register (or overwrite) the SEP-41 token contract that settles `currency`.
/// The admin authorization check lives in the contract entry point that calls
/// this — this module only owns registry state.
pub fn register_currency(env: &Env, currency: &Symbol, token: &Address) {
    let mut registry = load_currency_registry(env);
    registry.set(currency.clone(), token.clone());
    save_currency_registry(env, &registry);
}

/// Look up the token contract registered for `currency`, if any.
pub fn get_currency_token(env: &Env, currency: &Symbol) -> Option<Address> {
    load_currency_registry(env).get(currency.clone())
}

/// Resolve the token contract that moves funds for `currency`.
///
/// Prefers the currency registry; falls back to the legacy single token set
/// at `initialize()` so existing single-currency deployments keep working
/// unchanged. A multi-currency deployment registers each currency once via
/// `register_currency` — no per-function branches.
pub fn resolve_token(env: &Env, currency: &Symbol) -> Address {
    if let Some(addr) = get_currency_token(env, currency) {
        return addr;
    }
    env.storage()
        .instance()
        .get(&symbol_short!("token"))
        .unwrap_or_else(|| panic!("Not initialized"))
}
