#![no_std]

//! On-chain event indexing contract for the InvoFi Soroban protocol.
//!
//! Stores lightweight `EventRecord` summaries that mirror the full Soroban
//! event log, enabling efficient querying by event type, time range, and
//! actor address — with pagination support.
//!
//! Protocol contracts (registry, financing, repayment, insurance, reputation)
//! call `record_event` to maintain the index. The index is append-only;
//! pruning of old events is admin-gated and removes records older than a
//! configurable timestamp threshold.

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol, Vec};

use invofi_common::EventRecord;

// ─── Storage Keys ────────────────────────────────────────────────────────────

/// Global monotonic counter: next event ID to assign.
fn load_next_id(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&symbol_short!("nextid"))
        .unwrap_or(1u64)
}

fn save_next_id(env: &Env, id: u64) {
    env.storage()
        .instance()
        .set(&symbol_short!("nextid"), &id);
}

/// Total count of events in the index (may differ from next_id after pruning).
fn load_event_count(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&symbol_short!("evcnt"))
        .unwrap_or(0u64)
}

fn save_event_count(env: &Env, count: u64) {
    env.storage()
        .instance()
        .set(&symbol_short!("evcnt"), &count);
}

/// Individual event records: stored with composite key `(Symbol, u64)`.
fn load_event(env: &Env, event_id: u64) -> Option<EventRecord> {
    let key = (symbol_short!("evt"), event_id);
    env.storage().persistent().get(&key)
}

fn save_event(env: &Env, record: &EventRecord) {
    let key = (symbol_short!("evt"), record.event_id);
    env.storage().persistent().set(&key, record);
}

/// Type index: `type_idx:{event_type}` -> `Vec<u64>` (event IDs).
fn load_type_index(env: &Env, event_type: &Symbol) -> Vec<u64> {
    let key = (symbol_short!("typidx"), event_type.clone());
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

fn save_type_index(env: &Env, event_type: &Symbol, ids: &Vec<u64>) {
    let key = (symbol_short!("typidx"), event_type.clone());
    env.storage().persistent().set(&key, ids);
}

/// Actor index: `actridx:{actor}` -> `Vec<u64>` (event IDs).
fn load_actor_index(env: &Env, actor: &Address) -> Vec<u64> {
    let key = (symbol_short!("actidx"), actor.clone());
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env))
}

fn save_actor_index(env: &Env, actor: &Address, ids: &Vec<u64>) {
    let key = (symbol_short!("actidx"), actor.clone());
    env.storage().persistent().set(&key, ids);
}

/// Global event ID list: `allids` -> `Vec<u64>` (monotonically ordered).
fn load_all_ids(env: &Env) -> Vec<u64> {
    env.storage()
        .persistent()
        .get(&symbol_short!("allids"))
        .unwrap_or_else(|| Vec::new(env))
}

fn save_all_ids(env: &Env, ids: &Vec<u64>) {
    env.storage()
        .persistent()
        .set(&symbol_short!("allids"), ids);
}

// ─── Admin helpers ───────────────────────────────────────────────────────────

fn assert_admin(env: &Env, caller: &Address) {
    caller.require_auth();
    let current: Address = env
        .storage()
        .instance()
        .get(&symbol_short!("admin"))
        .unwrap_or_else(|| panic!("Not initialized"));
    if current != *caller {
        panic!("Only the current admin can perform this action");
    }
}

// ─── Recorder registry ───────────────────────────────────────────────────────

/// Check if `address` is a registered recorder (a protocol contract allowed
/// to call `record_event`).
fn is_recorder(env: &Env, address: &Address) -> bool {
    let recorders: Vec<Address> = env
        .storage()
        .instance()
        .get(&symbol_short!("recorders"))
        .unwrap_or_else(|| Vec::new(env));
    for r in recorders.iter() {
        if r == *address {
            return true;
        }
    }
    false
}

fn add_recorder(env: &Env, address: &Address) {
    let mut recorders: Vec<Address> = env
        .storage()
        .instance()
        .get(&symbol_short!("recorders"))
        .unwrap_or_else(|| Vec::new(env));
    // Idempotent
    for r in recorders.iter() {
        if r == *address {
            return;
        }
    }
    recorders.push_back(address.clone());
    env.storage()
        .instance()
        .set(&symbol_short!("recorders"), &recorders);
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct EventIndexerContract;

#[contractimpl]
impl EventIndexerContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// One-time setup. Sets the admin address.
    ///
    /// Runs as the contract **constructor**: it is executed atomically as part
    /// of the deploy operation, which only the deployer can authorize. There
    /// is therefore no separate initialize() call to front-run (issue #75).
    pub fn __constructor(env: Env, admin: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("Already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap_or_else(|| panic!("Not initialized"))
    }

    /// Transfers admin rights. Only current admin.
    pub fn transfer_admin(env: Env, admin: Address, new_admin: Address) {
        assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &new_admin);
    }

    // ── Recorder management ──────────────────────────────────────────────────

    /// Register a protocol contract as an event recorder. Admin only.
    ///
    /// Only registered recorders may call `record_event`. This prevents
    /// arbitrary contracts from polluting the index.
    pub fn add_recorder(env: Env, admin: Address, recorder: Address) {
        assert_admin(&env, &admin);
        add_recorder(&env, &recorder);
    }

    /// Check if an address is a registered recorder.
    pub fn is_recorder(env: Env, address: Address) -> bool {
        is_recorder(&env, &address)
    }

    /// Read all registered recorders.
    pub fn get_recorders(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&symbol_short!("recorders"))
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ── Event recording ──────────────────────────────────────────────────────

    /// Record an event in the on-chain index.
    ///
    /// The caller must be a registered recorder contract. In cross-contract
    /// calls, the calling contract passes its own address as `caller`; the
    /// indexer verifies it against the registered recorder list. For direct
    /// test invocations, the test passes its own address.
    ///
    /// Assigns a monotonic event ID and updates three indices:
    /// - **type index**: `event_type` -> list of event IDs
    /// - **actor index**: `actor` -> list of event IDs
    /// - **global list**: all event IDs in insertion order
    ///
    /// Returns the assigned event ID.
    pub fn record_event(
        env: Env,
        caller: Address,
        event_type: Symbol,
        actor: Address,
        data_key: Symbol,
    ) -> u64 {
        // Only registered recorders can record events.
        caller.require_auth();
        if !is_recorder(&env, &caller) {
            panic!("Not a registered recorder");
        }

        let event_id = load_next_id(&env);
        let timestamp = env.ledger().timestamp();
        let contract_id = env.current_contract_address();

        let record = EventRecord {
            event_id,
            event_type: event_type.clone(),
            timestamp,
            actor: actor.clone(),
            contract_id,
            data_key,
        };

        // 1. Save the event record
        save_event(&env, &record);

        // 2. Update type index
        let mut type_ids = load_type_index(&env, &event_type);
        type_ids.push_back(event_id);
        save_type_index(&env, &event_type, &type_ids);

        // 3. Update actor index
        let mut actor_ids = load_actor_index(&env, &actor);
        actor_ids.push_back(event_id);
        save_actor_index(&env, &actor, &actor_ids);

        // 4. Update global ID list
        let mut all_ids = load_all_ids(&env);
        all_ids.push_back(event_id);
        save_all_ids(&env, &all_ids);

        // 5. Increment counter
        save_next_id(&env, event_id + 1);

        // 6. Increment total count
        let count = load_event_count(&env);
        save_event_count(&env, count + 1);

        env.events().publish(
            (symbol_short!("evt_rec"), event_id),
            (event_type, actor, timestamp),
        );

        event_id
    }

    // ── Query: single event ──────────────────────────────────────────────────

    /// Read a single event record by its ID.
    pub fn get_event(env: Env, event_id: u64) -> EventRecord {
        load_event(&env, event_id).unwrap_or_else(|| panic!("Event not found"))
    }

    // ── Query: by type ───────────────────────────────────────────────────────

    /// Get all events of a specific type, with pagination.
    ///
    /// Uses the type index for O(1) lookup. Results are returned in
    /// chronological (insertion) order.
    pub fn get_events_by_type(
        env: Env,
        event_type: Symbol,
        offset: u32,
        limit: u32,
    ) -> Vec<EventRecord> {
        let ids = load_type_index(&env, &event_type);
        let mut result: Vec<EventRecord> = Vec::new(&env);
        let mut collected = 0u32;
        for (idx, id) in ids.iter().enumerate() {
            if (idx as u32) >= offset && collected < limit {
                if let Some(record) = load_event(&env, id) {
                    result.push_back(record);
                    collected += 1;
                }
            }
            if collected >= limit {
                break;
            }
        }
        result
    }

    /// Count events of a specific type.
    pub fn get_events_by_type_count(env: Env, event_type: Symbol) -> u64 {
        load_type_index(&env, &event_type).len() as u64
    }

    // ── Query: by time range ─────────────────────────────────────────────────

    /// Get events within a time range [start, end], with pagination.
    ///
    /// Scans the global ID list (which is in chronological order) and
    /// filters by timestamp. For large datasets, prefer the type index
    /// combined with a time filter.
    pub fn get_events_by_time(
        env: Env,
        start: u64,
        end: u64,
        offset: u32,
        limit: u32,
    ) -> Vec<EventRecord> {
        assert!(start <= end, "start must be <= end");
        let all_ids = load_all_ids(&env);
        let mut result: Vec<EventRecord> = Vec::new(&env);
        let mut skipped = 0u32;
        let mut collected = 0u32;
        for id in all_ids.iter() {
            if let Some(record) = load_event(&env, id) {
                if record.timestamp >= start && record.timestamp <= end {
                    if skipped < offset {
                        skipped += 1;
                    } else if collected < limit {
                        result.push_back(record);
                        collected += 1;
                    }
                }
            }
            if collected >= limit {
                break;
            }
        }
        result
    }

    // ── Query: by actor ──────────────────────────────────────────────────────

    /// Get all events for a specific actor address, with pagination.
    ///
    /// Uses the actor index for O(1) lookup. Results are returned in
    /// chronological (insertion) order.
    pub fn get_events_by_actor(
        env: Env,
        actor: Address,
        offset: u32,
        limit: u32,
    ) -> Vec<EventRecord> {
        let ids = load_actor_index(&env, &actor);
        let mut result: Vec<EventRecord> = Vec::new(&env);
        let mut collected = 0u32;
        for (idx, id) in ids.iter().enumerate() {
            if (idx as u32) >= offset && collected < limit {
                if let Some(record) = load_event(&env, id) {
                    result.push_back(record);
                    collected += 1;
                }
            }
            if collected >= limit {
                break;
            }
        }
        result
    }

    /// Count events for a specific actor.
    pub fn get_events_by_actor_count(env: Env, actor: Address) -> u64 {
        load_actor_index(&env, &actor).len() as u64
    }

    // ── Query: totals ────────────────────────────────────────────────────────

    /// Total number of events in the index.
    pub fn get_event_count(env: Env) -> u64 {
        load_event_count(&env)
    }

    // ── Pruning ──────────────────────────────────────────────────────────────

    /// Prune events older than `before_timestamp`. Admin only.
    ///
    /// Removes all event records with `timestamp < before_timestamp` from:
    /// - The global ID list
    /// - The type index
    /// - The actor index
    /// - The event store (individual records)
    ///
    /// Returns the number of events pruned.
    ///
    /// **Warning**: This is an irreversible operation. Pruned events cannot
    /// be recovered. Admin should verify the timestamp threshold carefully.
    pub fn prune_events(env: Env, admin: Address, before_timestamp: u64) -> u64 {
        assert_admin(&env, &admin);

        let all_ids = load_all_ids(&env);
        let mut pruned_count: u64 = 0;
        let mut kept_ids: Vec<u64> = Vec::new(&env);

        // Collect IDs to prune
        let mut to_prune: Vec<u64> = Vec::new(&env);
        for id in all_ids.iter() {
            if let Some(record) = load_event(&env, id) {
                if record.timestamp < before_timestamp {
                    to_prune.push_back(id);
                } else {
                    kept_ids.push_back(id);
                }
            }
        }

        // Remove pruned events from the store and indices
        for id in to_prune.iter() {
            if let Some(record) = load_event(&env, id) {
                // Remove from type index
                let type_ids = load_type_index(&env, &record.event_type);
                let mut new_type_ids: Vec<u64> = Vec::new(&env);
                for tid in type_ids.iter() {
                    if tid != id {
                        new_type_ids.push_back(tid);
                    }
                }
                save_type_index(&env, &record.event_type, &new_type_ids);

                // Remove from actor index
                let actor_ids = load_actor_index(&env, &record.actor);
                let mut new_actor_ids: Vec<u64> = Vec::new(&env);
                for aid in actor_ids.iter() {
                    if aid != id {
                        new_actor_ids.push_back(aid);
                    }
                }
                save_actor_index(&env, &record.actor, &new_actor_ids);

                // Remove the event record itself
                let key = (symbol_short!("evt"), id);
                env.storage().persistent().remove(&key);

                pruned_count += 1;
            }
        }

        // Save the updated global ID list
        save_all_ids(&env, &kept_ids);

        // Decrement the total event count
        let count = load_event_count(&env);
        save_event_count(&env, count - pruned_count);

        env.events()
            .publish((symbol_short!("evt_prn"),), (pruned_count, before_timestamp));

        pruned_count
    }

    // ── Metadata ─────────────────────────────────────────────────────────────

    pub fn version(env: Env) -> soroban_sdk::String {
        soroban_sdk::String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod test;
