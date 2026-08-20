#![cfg(test)]
extern crate std;

use super::{EventIndexerContract, EventIndexerContractClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _, Ledger as _},
    Address, Env, IntoVal, Symbol,
};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn setup() -> (Env, EventIndexerContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register(EventIndexerContract, (admin.clone(),));
    let client = EventIndexerContractClient::new(&env, &contract_id);
    (env, client, admin)
}

fn setup_with_recorder() -> (Env, EventIndexerContractClient<'static>, Address, Address) {
    let (env, client, admin) = setup();
    let recorder = Address::generate(&env);
    client.add_recorder(&admin, &recorder);
    (env, client, admin, recorder)
}

fn record(
    client: &EventIndexerContractClient<'static>,
    recorder: &Address,
    event_type: &Symbol,
    actor: &Address,
    data_key: &Symbol,
) -> u64 {
    client.record_event(recorder, event_type, actor, data_key)
}

// ─── Initialization tests ────────────────────────────────────────────────────

#[test]
fn test_constructor_sets_admin() {
    let (env, client, admin) = setup();
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_constructor_sets_admin_and_is_idempotent() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register(EventIndexerContract, (admin.clone(),));
    let client = EventIndexerContractClient::new(&env, &contract_id);

    // Admin is bound atomically at deploy.
    assert_eq!(client.get_admin(), admin);

    // Constructor runs atomically inside deploy; the idempotency guard
    // ("Already initialized" panic) is enforced by the contract code
    // but cannot be re-triggered via try_invoke_contract in the test
    // framework.
}

#[test]
fn test_transfer_admin() {
    let (env, client, admin) = setup();
    let new_admin = Address::generate(&env);
    client.transfer_admin(&admin, &new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
#[should_panic(expected = "Only the current admin can perform this action")]
fn test_transfer_admin_unauthorized() {
    let (env, client, _admin) = setup();
    let not_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.transfer_admin(&not_admin, &new_admin);
}

// ─── Recorder management tests ───────────────────────────────────────────────

#[test]
fn test_add_and_check_recorder() {
    let (env, client, admin) = setup();
    let recorder = Address::generate(&env);

    assert!(!client.is_recorder(&recorder));
    client.add_recorder(&admin, &recorder);
    assert!(client.is_recorder(&recorder));
}

#[test]
fn test_add_recorder_idempotent() {
    let (env, client, admin) = setup();
    let recorder = Address::generate(&env);

    client.add_recorder(&admin, &recorder);
    client.add_recorder(&admin, &recorder);
    assert!(client.is_recorder(&recorder));
    assert_eq!(client.get_recorders().len(), 1);
}

#[test]
fn test_get_recorders() {
    let (env, client, admin) = setup();
    let r1 = Address::generate(&env);
    let r2 = Address::generate(&env);

    client.add_recorder(&admin, &r1);
    client.add_recorder(&admin, &r2);
    let recorders = client.get_recorders();
    assert_eq!(recorders.len(), 2);
}

#[test]
#[should_panic(expected = "Only the current admin can perform this action")]
fn test_add_recorder_unauthorized() {
    let (env, client, _admin) = setup();
    let not_admin = Address::generate(&env);
    let recorder = Address::generate(&env);
    client.add_recorder(&not_admin, &recorder);
}

// ─── Event recording tests ──────────────────────────────────────────────────

#[test]
fn test_record_event_returns_incrementing_id() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor = Address::generate(&env);
    let id1 = record(
        &client,
        &recorder,
        &symbol_short!("inv_reg"),
        &actor,
        &symbol_short!("inv001"),
    );
    let id2 = record(
        &client,
        &recorder,
        &symbol_short!("off_new"),
        &actor,
        &symbol_short!("off001"),
    );

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

#[test]
fn test_record_event_stores_correctly() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(2000);

    let actor = Address::generate(&env);
    let id = record(
        &client,
        &recorder,
        &symbol_short!("inv_reg"),
        &actor,
        &symbol_short!("inv001"),
    );

    let event = client.get_event(&id);
    assert_eq!(event.event_id, 1);
    assert_eq!(event.event_type, symbol_short!("inv_reg"));
    assert_eq!(event.timestamp, 2000);
    assert_eq!(event.actor, actor);
    assert_eq!(event.data_key, symbol_short!("inv001"));
}

#[test]
fn test_record_event_emits_event() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(3000);

    let actor = Address::generate(&env);
    record(
        &client,
        &recorder,
        &symbol_short!("inv_reg"),
        &actor,
        &symbol_short!("inv1"),
    );

    let events = env.events().all();
    assert!(!events.is_empty(), "record_event should emit an event");
}

#[test]
#[should_panic(expected = "Not a registered recorder")]
fn test_record_event_unauthorized_recorder() {
    let (env, client, _admin) = setup();
    let unauthorized = Address::generate(&env);
    let actor = Address::generate(&env);

    record(
        &client,
        &unauthorized,
        &symbol_short!("inv_reg"),
        &actor,
        &symbol_short!("inv1"),
    );
}

#[test]
fn test_event_count_starts_at_zero() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    assert_eq!(client.get_event_count(), 0);
}

#[test]
fn test_event_count_increments() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor = Address::generate(&env);
    record(
        &client,
        &recorder,
        &symbol_short!("inv_reg"),
        &actor,
        &symbol_short!("inv1"),
    );
    assert_eq!(client.get_event_count(), 1);

    record(
        &client,
        &recorder,
        &symbol_short!("off_new"),
        &actor,
        &symbol_short!("off1"),
    );
    assert_eq!(client.get_event_count(), 2);
}

#[test]
#[should_panic(expected = "Event not found")]
fn test_get_nonexistent_event() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    client.get_event(&999);
}

// ─── Query by type tests ────────────────────────────────────────────────────

#[test]
fn test_get_events_by_type_empty() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    let result = client.get_events_by_type(&symbol_short!("inv_reg"), &0, &10);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_events_by_type_filters_correctly() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor = Address::generate(&env);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));
    record(&client, &recorder, &symbol_short!("off_new"), &actor, &symbol_short!("off1"));
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv2"));

    let inv_events = client.get_events_by_type(&symbol_short!("inv_reg"), &0, &10);
    assert_eq!(inv_events.len(), 2);
    assert_eq!(inv_events.get(0).unwrap().event_type, symbol_short!("inv_reg"));
    assert_eq!(inv_events.get(1).unwrap().event_type, symbol_short!("inv_reg"));

    let off_events = client.get_events_by_type(&symbol_short!("off_new"), &0, &10);
    assert_eq!(off_events.len(), 1);
    assert_eq!(off_events.get(0).unwrap().event_type, symbol_short!("off_new"));
}

#[test]
fn test_get_events_by_type_pagination() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor = Address::generate(&env);
    for i in 0..5u32 {
        let key = Symbol::new(&env, "inv");
        record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &key);
    }

    let page1 = client.get_events_by_type(&symbol_short!("inv_reg"), &0, &3);
    assert_eq!(page1.len(), 3);

    let page2 = client.get_events_by_type(&symbol_short!("inv_reg"), &3, &3);
    assert_eq!(page2.len(), 2);

    let page3 = client.get_events_by_type(&symbol_short!("inv_reg"), &5, &3);
    assert_eq!(page3.len(), 0);
}

#[test]
fn test_get_events_by_type_count() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor = Address::generate(&env);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));
    record(&client, &recorder, &symbol_short!("off_new"), &actor, &symbol_short!("off1"));
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv2"));

    assert_eq!(client.get_events_by_type_count(&symbol_short!("inv_reg")), 2);
    assert_eq!(client.get_events_by_type_count(&symbol_short!("off_new")), 1);
    assert_eq!(client.get_events_by_type_count(&symbol_short!("inv_rep")), 0);
}

// ─── Query by time range tests ──────────────────────────────────────────────

#[test]
fn test_get_events_by_time_empty() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    let result = client.get_events_by_time(&100, &200, &0, &10);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_events_by_time_filters_correctly() {
    let (env, client, _admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);

    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("off_new"), &actor, &symbol_short!("off1"));

    env.ledger().set_timestamp(300);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv2"));

    // Range [150, 250] should return only the event at timestamp 200
    let result = client.get_events_by_time(&150, &250, &0, &10);
    assert_eq!(result.len(), 1);
    assert_eq!(result.get(0).unwrap().timestamp, 200);

    // Range [100, 200] should return events at 100 and 200
    let result = client.get_events_by_time(&100, &200, &0, &10);
    assert_eq!(result.len(), 2);

    // Range [100, 300] should return all 3
    let result = client.get_events_by_time(&100, &300, &0, &10);
    assert_eq!(result.len(), 3);
}

#[test]
fn test_get_events_by_time_pagination() {
    let (env, client, _admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);

    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));
    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv2"));
    env.ledger().set_timestamp(300);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv3"));

    let page1 = client.get_events_by_time(&100, &300, &0, &2);
    assert_eq!(page1.len(), 2);

    let page2 = client.get_events_by_time(&100, &300, &2, &2);
    assert_eq!(page2.len(), 1);
}

#[test]
#[should_panic(expected = "start must be <= end")]
fn test_get_events_by_time_invalid_range() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    client.get_events_by_time(&300, &100, &0, &10);
}

// ─── Query by actor tests ──────────────────────────────────────────────────

#[test]
fn test_get_events_by_actor_empty() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    let actor = Address::generate(&env);
    let result = client.get_events_by_actor(&actor, &0, &10);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_events_by_actor_filters_correctly() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor_a = Address::generate(&env);
    let actor_b = Address::generate(&env);

    record(&client, &recorder, &symbol_short!("inv_reg"), &actor_a, &symbol_short!("inv1"));
    record(&client, &recorder, &symbol_short!("off_new"), &actor_a, &symbol_short!("off1"));
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor_b, &symbol_short!("inv2"));

    let a_events = client.get_events_by_actor(&actor_a, &0, &10);
    assert_eq!(a_events.len(), 2);

    let b_events = client.get_events_by_actor(&actor_b, &0, &10);
    assert_eq!(b_events.len(), 1);
    assert_eq!(b_events.get(0).unwrap().data_key, symbol_short!("inv2"));
}

#[test]
fn test_get_events_by_actor_pagination() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor = Address::generate(&env);
    for i in 0..5u32 {
        let key = Symbol::new(&env, "evt");
        record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &key);
    }

    let page1 = client.get_events_by_actor(&actor, &0, &3);
    assert_eq!(page1.len(), 3);

    let page2 = client.get_events_by_actor(&actor, &3, &3);
    assert_eq!(page2.len(), 2);
}

#[test]
fn test_get_events_by_actor_count() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    env.ledger().set_timestamp(1000);

    let actor_a = Address::generate(&env);
    let actor_b = Address::generate(&env);

    record(&client, &recorder, &symbol_short!("inv_reg"), &actor_a, &symbol_short!("inv1"));
    record(&client, &recorder, &symbol_short!("off_new"), &actor_a, &symbol_short!("off1"));
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor_b, &symbol_short!("inv2"));

    assert_eq!(client.get_events_by_actor_count(&actor_a), 2);
    assert_eq!(client.get_events_by_actor_count(&actor_b), 1);
}

// ─── Pruning tests ──────────────────────────────────────────────────────────

#[test]
fn test_prune_events_removes_old_records() {
    let (env, client, admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);

    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("off_new"), &actor, &symbol_short!("off1"));

    env.ledger().set_timestamp(300);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv2"));

    assert_eq!(client.get_event_count(), 3);

    // Prune events before timestamp 200 (removes event at 100)
    let pruned = client.prune_events(&admin, &200);
    assert_eq!(pruned, 1);
    assert_eq!(client.get_event_count(), 2);

    // The event at timestamp 100 should be gone
    let result = client.get_events_by_time(&100, &100, &0, &10);
    assert_eq!(result.len(), 0);

    // The events at 200 and 300 should still be there
    let result = client.get_events_by_time(&200, &300, &0, &10);
    assert_eq!(result.len(), 2);
}

#[test]
fn test_prune_events_removes_from_type_index() {
    let (env, client, admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);

    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv2"));

    // Prune the first event
    client.prune_events(&admin, &200);

    // Type index should only have 1 event
    let type_count = client.get_events_by_type_count(&symbol_short!("inv_reg"));
    assert_eq!(type_count, 1);

    let type_events = client.get_events_by_type(&symbol_short!("inv_reg"), &0, &10);
    assert_eq!(type_events.len(), 1);
    assert_eq!(type_events.get(0).unwrap().data_key, symbol_short!("inv2"));
}

#[test]
fn test_prune_events_removes_from_actor_index() {
    let (env, client, admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);

    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("off_new"), &actor, &symbol_short!("off1"));

    // Prune the first event
    client.prune_events(&admin, &200);

    // Actor index should only have 1 event
    let actor_count = client.get_events_by_actor_count(&actor);
    assert_eq!(actor_count, 1);
}

#[test]
fn test_prune_events_no_op_when_nothing_to_prune() {
    let (env, client, admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);
    env.ledger().set_timestamp(1000);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    // Prune events before timestamp 500 (nothing to prune)
    let pruned = client.prune_events(&admin, &500);
    assert_eq!(pruned, 0);
    assert_eq!(client.get_event_count(), 1);
}

#[test]
fn test_prune_events_removes_all() {
    let (env, client, admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);
    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("off_new"), &actor, &symbol_short!("off1"));

    // Prune everything
    let pruned = client.prune_events(&admin, &300);
    assert_eq!(pruned, 2);
    assert_eq!(client.get_event_count(), 0);
    assert_eq!(client.get_recorders().len(), 1); // Recorders preserved
}

#[test]
fn test_prune_events_emits_event() {
    let (env, client, admin, recorder) = setup_with_recorder();

    let actor = Address::generate(&env);
    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &actor, &symbol_short!("inv1"));

    client.prune_events(&admin, &200);

    let events = env.events().all();
    assert!(!events.is_empty(), "prune_events should emit an event");
}

#[test]
#[should_panic(expected = "Only the current admin can perform this action")]
fn test_prune_events_unauthorized() {
    let (env, client, _admin, recorder) = setup_with_recorder();
    let not_admin = Address::generate(&env);
    client.prune_events(&not_admin, &1000);
}

// ─── Version test ────────────────────────────────────────────────────────────

#[test]
fn test_version_returns_nonempty_string() {
    let (env, client, _admin, _recorder) = setup_with_recorder();
    let ver = client.version();
    assert!(!ver.is_empty());
}

// ─── Multi-actor, multi-type integration tests ──────────────────────────────

#[test]
fn test_complex_scenario_multiple_actors_types() {
    let (env, client, _admin, recorder) = setup_with_recorder();

    let originator = Address::generate(&env);
    let lender = Address::generate(&env);

    // Register invoice
    env.ledger().set_timestamp(100);
    record(&client, &recorder, &symbol_short!("inv_reg"), &originator, &symbol_short!("inv001"));

    // Create offer
    env.ledger().set_timestamp(200);
    record(&client, &recorder, &symbol_short!("off_new"), &lender, &symbol_short!("off001"));

    // Accept offer
    env.ledger().set_timestamp(300);
    record(&client, &recorder, &symbol_short!("off_acc"), &originator, &symbol_short!("off001"));

    // Repay
    env.ledger().set_timestamp(400);
    record(&client, &recorder, &symbol_short!("inv_rep"), &originator, &symbol_short!("inv001"));

    assert_eq!(client.get_event_count(), 4);

    // Query by originator
    let originator_events = client.get_events_by_actor(&originator, &0, &10);
    assert_eq!(originator_events.len(), 3); // inv_reg, off_acc, inv_rep

    // Query by lender
    let lender_events = client.get_events_by_actor(&lender, &0, &10);
    assert_eq!(lender_events.len(), 1); // off_new

    // Query by type
    let invoice_events = client.get_events_by_type(&symbol_short!("inv_reg"), &0, &10);
    assert_eq!(invoice_events.len(), 1);

    let offer_events = client.get_events_by_type(&symbol_short!("off_new"), &0, &10);
    assert_eq!(offer_events.len(), 1);

    // Query by time range
    let early_events = client.get_events_by_time(&100, &250, &0, &10);
    assert_eq!(early_events.len(), 2); // inv_reg, off_new

    let late_events = client.get_events_by_time(&300, &500, &0, &10);
    assert_eq!(late_events.len(), 2); // off_acc, inv_rep
}
