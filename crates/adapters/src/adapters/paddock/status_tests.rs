//! Paddock's shared lease-turn availability

use super::{load_in, queued_in};
use crate::ports::ModelLoad;
use serde_json::{Value, json};

const FULL: &str = r#"{
    "models": [{"model":"coder","turns":{"lease":2,"request":1}}],
    "leases": [
        {"model":"coder","revoked":null,"reclaimable":false},
        {"model":"coder","revoked":null,"reclaimable":false}
    ],
    "waiters": [{"model":"coder","kind":"lease"}]
}"#;

#[test]
fn two_held_lease_turns_and_a_waiter_are_queued_despite_the_extra_request_turn() {
    assert!(queued_in(FULL, "coder"));
}

#[test]
fn a_free_lease_turn_is_available_even_with_a_waiter() {
    let mut status: Value = serde_json::from_str(FULL).unwrap();
    status["leases"].as_array_mut().unwrap().pop();
    assert!(!queued_in(&status.to_string(), "coder"));
}

#[test]
fn held_turns_without_a_waiter_do_not_move_new_work() {
    let mut status: Value = serde_json::from_str(FULL).unwrap();
    status["waiters"] = json!([]);
    assert!(!queued_in(&status.to_string(), "coder"));
}

#[test]
fn leases_that_are_revoked_reclaimable_or_on_other_models_take_no_turn() {
    for (field, value) in [
        ("revoked", json!({"by": "admin", "note": "done"})),
        ("reclaimable", json!(true)),
        ("model", json!("other")),
    ] {
        let mut status: Value = serde_json::from_str(FULL).unwrap();
        status["leases"][0][field] = value;
        assert!(!queued_in(&status.to_string(), "coder"), "{field}");
    }
}

#[test]
fn a_waiter_on_another_model_does_not_count() {
    let mut status: Value = serde_json::from_str(FULL).unwrap();
    status["waiters"][0]["model"] = json!("other");
    assert!(!queued_in(&status.to_string(), "coder"));
    assert!(!queued_in(FULL, "other"));
}

#[test]
fn a_model_without_lease_turns_and_an_unreadable_status_count_as_free() {
    for turns in [Value::Null, json!({"request": 1}), json!({"lease": 0})] {
        let mut status: Value = serde_json::from_str(FULL).unwrap();
        status["models"][0]["turns"] = turns;
        assert!(!queued_in(&status.to_string(), "coder"));
    }
    for body in ["not json", "{}", "[]"] {
        assert!(!queued_in(body, "coder"));
    }
}

// Two models, with leases a revoke or a reclaim has ended and a request turn's waiter.
const BUSY: &str = r#"{
    "models": [
        {"model":"coder","turns":{"lease":2,"request":1}},
        {"model":"other","turns":{"lease":1}},
        {"model":"plain","turns":{"request":4}}
    ],
    "leases": [
        {"model":"coder","revoked":null,"reclaimable":false},
        {"model":"coder","revoked":{"by":"admin","note":"done"},"reclaimable":false},
        {"model":"coder","revoked":null,"reclaimable":true},
        {"model":"other","revoked":null,"reclaimable":false}
    ],
    "waiters": [
        {"model":"coder","kind":"lease"},
        {"model":"coder","kind":"lease"},
        {"model":"coder","kind":"request"},
        {"model":"other","kind":"lease"}
    ]
}"#;

#[test]
fn a_models_load_counts_its_live_leases_and_its_lease_waiters() {
    let load = |slots, held, waiting| ModelLoad {
        slots,
        held,
        waiting,
    };
    assert_eq!(load_in(BUSY, "coder"), Ok(load(2, 1, 2)));
    assert_eq!(load_in(BUSY, "other"), Ok(load(1, 1, 1)));
}

#[test]
fn a_model_without_lease_turns_or_unlisted_has_no_slots() {
    let none = ModelLoad {
        slots: 0,
        held: 0,
        waiting: 0,
    };
    assert_eq!(load_in(BUSY, "plain"), Ok(none));
    assert_eq!(load_in(BUSY, "absent"), Ok(none));
}

#[test]
fn a_status_with_no_model_list_is_no_load() {
    for body in ["not json", "{}", "[]", r#"{"models":{}}"#] {
        assert!(load_in(body, "coder").is_err(), "{body}");
    }
}
