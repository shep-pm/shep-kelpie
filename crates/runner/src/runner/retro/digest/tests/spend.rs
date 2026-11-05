//! What the batch cost: the ledger lines behind the digest spend notes

use super::*;

#[test]
fn an_unpriced_call_and_a_timed_out_turn_never_show_zero_dollars() {
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 600)];
    let ledger = [
        unpriced(1500, Some(11), "worker", "turn", 1_200_000),
        timed_out(1600, Some(11), "worker", "turn"),
        line(json!({
            "line": "finished", "at": 2000, "issue": 11, "title": "fix(a): one",
            "pull_request": 71, "merged": true, "wall": 600, "units": 1200000, "cost_usd": 0.0,
            "worker": {"calls": 2, "units": 1200000, "cost_usd": 0.0, "unpriced_calls": 2,
                       "tokens": {"input": 0, "cache_write": 0, "cache_read": 0, "output": 0}},
            "reviewer": role(0, 0, 0.0),
            "qwen": {"rounds": 0, "seconds": 0},
            "review_rounds": 0, "fix_turns": 0, "rulings": 0,
        })),
    ];

    let digest = built(&batch, &[], &ledger, 1000, NO_GAPS);

    let item = section(&digest, "## #11:");
    assert!(
        item.contains(
            "- Ledger: 1200000 units, 1 unpriced; 1 timed out, usage not read; \
             worker 2 calls 1 unpriced; 1 timed out, usage not read; \
             reviewer 0 calls $0.00; 0 review rounds, 0 fix turns, 0 rulings"
        ),
        "{item}"
    );
    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains(
            "- worker turn: 2 calls, 1200000 units, 1 unpriced; 1 timed out, usage not read"
        ),
        "{totals}"
    );
    assert!(!totals.contains("units, $0.00"), "{totals}");
}

#[test]
fn a_local_models_calls_show_as_local_unpriced_and_never_zero_dollars() {
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 600)];
    let ledger = [
        unpriced(1500, Some(11), "worker", "turn", 600_000),
        unpriced(1600, Some(11), "worker", "turn", 600_000),
    ];

    let digest = built(&batch, &[], &ledger, 1000, NO_GAPS);

    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains("- worker turn: 2 calls, 1200000 units, local, unpriced"),
        "{totals}"
    );
}

#[test]
fn a_priced_call_shows_its_dollars_and_names_the_one_that_is_unpriced() {
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 600)];
    let ledger = [
        call(1500, Some(11), "worker", "turn", 100, 1.50),
        unpriced(1600, Some(11), "worker", "turn", 40),
    ];

    let digest = built(&batch, &[], &ledger, 1000, NO_GAPS);

    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains("- worker turn: 2 calls, 140 units, $1.50; 1 unpriced"),
        "{totals}"
    );
}

// A worker turn on a local model, which used `wh` watt-hours and, where its host is priced, cost `cost`
fn metered(at: u64, wh: f64, cost: Option<f64>) -> Line {
    line(json!({
        "line": "call", "at": at, "issue": 11, "role": "worker", "kind": "turn",
        "usage": {"input": 0, "cache_write": 0, "cache_read": 0, "output": 0},
        "units": 600_000, "cost_usd": cost, "unpriced": cost.is_none(),
        "energy_wh": wh, "energy_source": "wall", "ended": "answered",
    }))
}

#[test]
fn an_energy_priced_call_reads_as_local_with_its_electricity() {
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 600)];
    let ledger = [metered(1500, 12.0, Some(0.04))];

    let digest = built(&batch, &[], &ledger, 1000, NO_GAPS);

    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains("- worker turn: 1 calls, 600000 units, local, $0.04 of electricity"),
        "{totals}"
    );
}

#[test]
fn a_metered_call_with_no_price_reads_as_local_with_its_watt_hours_unpriced() {
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 600)];
    let ledger = [metered(1500, 12.0, None)];

    let digest = built(&batch, &[], &ledger, 1000, NO_GAPS);

    let totals = section(&digest, "## Batch totals");
    assert!(
        totals.contains("- worker turn: 1 calls, 600000 units, local, 12 Wh, unpriced"),
        "{totals}"
    );
}

#[test]
fn energy_priced_calls_beside_dollar_calls_keep_their_own_notes() {
    let batch = [finished(11, "fix(a): one", Some(71), 2000, 600)];
    let ledger = [
        call(1500, Some(11), "worker", "turn", 100, 1.50),
        metered(1600, 12.0, Some(0.04)),
    ];

    let digest = built(&batch, &[], &ledger, 1000, NO_GAPS);

    let totals = section(&digest, "## Batch totals");
    assert!(
        totals
            .contains("- worker turn: 2 calls, 600100 units, $1.50; 1 local, $0.04 of electricity"),
        "{totals}"
    );
}
