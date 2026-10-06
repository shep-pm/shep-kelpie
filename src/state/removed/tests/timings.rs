//! A state file saved while timings had eleven phases

use serde_json::json;

use super::saved_with;
use crate::work_item::TimingPhase;

const REVIEW: [&str; 5] = [
    "gpu_wait",
    "local_round",
    "claude_round",
    "coderabbit_window",
    "coderabbit_review",
];

// The work item's seconds in `name` and the finished one's, with the
// seconds in other and ci they already hold.
fn with_seconds(value: &mut serde_json::Value, name: &str) {
    let seconds = &mut value["work_items"][0]["timings"]["seconds"];
    seconds["ci"] = json!(1);
    seconds[name] = json!(2);
    let seconds = &mut value["history"][0]["seconds"];
    seconds["ci"] = json!(4);
    seconds["ruling"] = json!(1);
    seconds[name] = json!(5);
}

fn folds_into(name: &str, into: TimingPhase) {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| with_seconds(value, name));
    let state = store.load().unwrap().unwrap();
    let open = &state.work_items[0].timings.as_ref().unwrap().seconds;
    assert_eq!(open.get(into), 2, "{name}");
    assert_eq!(open.get(TimingPhase::Ci), 1, "{name}");
    assert_eq!(open.total(), 7, "{name}: the seconds since it was created");
    let finished = &state.history[0].seconds;
    assert_eq!(finished.get(into), 5, "{name}");
    assert_eq!(finished.total(), state.history[0].wall, "{name}");
}

#[test]
fn a_gpu_waits_time_counts_as_review() {
    folds_into("gpu_wait", TimingPhase::Review);
}

#[test]
fn a_local_rounds_time_counts_as_review() {
    folds_into("local_round", TimingPhase::Review);
}

#[test]
fn a_claude_rounds_time_counts_as_review() {
    folds_into("claude_round", TimingPhase::Review);
}

#[test]
fn a_review_bots_window_counts_as_review() {
    folds_into("coderabbit_window", TimingPhase::Review);
}

#[test]
fn a_review_bots_review_counts_as_review() {
    folds_into("coderabbit_review", TimingPhase::Review);
}

#[test]
fn a_paused_projects_time_counts_as_other() {
    folds_into("paused", TimingPhase::Other);
}

#[test]
fn a_file_holding_every_old_phase_keeps_its_total() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let old = ["worker", "ci", "ruling", "merge", "paused", "other"];
        let named = REVIEW.iter().chain(old.iter());
        let open = &mut value["work_items"][0]["timings"]["seconds"];
        for (n, name) in named.clone().enumerate() {
            open[*name] = json!(n as u64 + 1);
        }
        let history = &mut value["history"][0];
        history["seconds"] = json!({});
        for (n, name) in named.enumerate() {
            history["seconds"][*name] = json!(n as u64 + 1);
        }
        history["wall"] = json!(66);
    });
    let state = store.load().unwrap().unwrap();
    let open = &state.work_items[0].timings.as_ref().unwrap().seconds;
    let finished = &state.history[0].seconds;
    for seconds in [open, finished] {
        assert_eq!(seconds.total(), 66);
        assert_eq!(seconds.get(TimingPhase::Review), 15);
        assert_eq!(seconds.get(TimingPhase::Other), 10 + 11);
        assert_eq!(seconds.get(TimingPhase::Worker), 6);
    }
}
