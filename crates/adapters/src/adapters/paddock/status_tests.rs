//! Paddock's load on a model, from its status

use super::load_in;
use crate::ports::ModelLoad;

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
