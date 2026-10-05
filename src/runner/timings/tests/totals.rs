//! The finished-item history, the `timings` totals and their table

use serde_json::{Value, json};

use super::{finished, secs, summed, with_history};
use crate::state::{HISTORY_CAP, StateStore};
use crate::test::Rig;
use crate::work_item::TimingPhase;

fn three() -> Vec<crate::state::Finished> {
    vec![
        finished(7, true, &[(TimingPhase::Worker, 60), (TimingPhase::Ci, 40)]),
        finished(
            9,
            false,
            &[(TimingPhase::Worker, 10), (TimingPhase::Ruling, 40)],
        ),
        finished(
            12,
            true,
            &[(TimingPhase::Merge, 5), (TimingPhase::GpuWait, 15)],
        ),
    ]
}

#[test]
fn the_totals_are_the_last_n_items_summed_oldest_first() {
    let (rig, runner) = with_history(three());
    let t = rig.ask(&runner, "timings", Some("2"));
    assert_eq!(t["items"], 2);
    assert_eq!(t["issues"], json!([9, 12]));
    assert_eq!(t["wall"], 70);
    assert_eq!(secs(&t, "worker"), 10);
    assert_eq!(secs(&t, "ruling"), 40);
    assert_eq!(secs(&t, "gpu_wait"), 15);
    assert_eq!(secs(&t, "ci"), 0, "issue 7 is outside the last two");
    assert_eq!(summed(&t), 70);
    assert_eq!(t["project"], "koji");
}

#[test]
fn more_than_have_finished_reads_them_all_and_no_count_reads_ten() {
    let (rig, runner) = with_history(three());
    for params in [Some("500"), None] {
        let t = rig.ask(&runner, "timings", params);
        assert_eq!(
            (&t["items"], &t["issues"]),
            (&json!(3), &json!([7, 9, 12])),
            "{params:?}"
        );
        assert_eq!(t["wall"], 170);
        assert_eq!(summed(&t), 170);
    }
}

#[test]
fn nothing_finished_is_zeros_not_an_error() {
    let (rig, runner) = with_history(vec![]);
    let t = rig.ask(&runner, "timings", None);
    assert_eq!(
        (&t["items"], &t["issues"], &t["wall"]),
        (&json!(0), &json!([]), &json!(0))
    );
    assert_eq!(summed(&t), 0);
    assert_eq!(t["seconds"].as_object().unwrap().len(), 14);
    assert!(
        t["table"]
            .as_str()
            .unwrap()
            .contains("no finished work items")
    );
}

#[test]
fn a_bad_count_is_refused() {
    let (rig, runner) = with_history(three());
    for bad in ["0", "-1", "x", "+2", "1.5"] {
        let reply = rig.ask(&runner, "timings", Some(bad));
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .starts_with("`timings` takes a count"),
            "{bad}: {reply}"
        );
    }
}

#[test]
fn the_table_lists_every_phase_then_the_total() {
    let (rig, runner) = with_history(three()[..2].to_vec());
    let t = rig.ask(&runner, "timings", Some("2"));
    let row = |name: &str, secs: &str, time: &str, share: &str| {
        format!("{name:<20}{secs:>8}  {time:>9}  {share:>6}")
    };
    let expected = [
        "2 work items: #7 and #9".to_owned(),
        row("phase", "seconds", "duration", "share"),
        row("worker", "70", "0h01m10s", "46.7%"),
        row("gpu_wait", "0", "0h00m00s", "0.0%"),
        row("local_round", "0", "0h00m00s", "0.0%"),
        row("claude_round", "0", "0h00m00s", "0.0%"),
        row("deep_round", "0", "0h00m00s", "0.0%"),
        row("judging", "0", "0h00m00s", "0.0%"),
        row("ci", "40", "0h00m40s", "26.7%"),
        row("coderabbit_window", "0", "0h00m00s", "0.0%"),
        row("coderabbit_review", "0", "0h00m00s", "0.0%"),
        row("ruling", "40", "0h00m40s", "26.7%"),
        row("merge", "0", "0h00m00s", "0.0%"),
        row("shots", "0", "0h00m00s", "0.0%"),
        row("paused", "0", "0h00m00s", "0.0%"),
        row("other", "0", "0h00m00s", "0.0%"),
        row("total", "150", "0h02m30s", "100.0%"),
    ]
    .join("\n");
    assert_eq!(t["table"], expected);
}

// The reply as the maintainer's scripts read it, field for field.
#[test]
fn the_timings_reply_is_pinned() {
    let table = [
        "3 work items: #7, #9 and #12",
        "phase                seconds   duration   share",
        "worker                    70   0h01m10s   41.2%",
        "gpu_wait                  15   0h00m15s    8.8%",
        "local_round                0   0h00m00s    0.0%",
        "claude_round               0   0h00m00s    0.0%",
        "deep_round                 0   0h00m00s    0.0%",
        "judging                    0   0h00m00s    0.0%",
        "ci                        40   0h00m40s   23.5%",
        "coderabbit_window          0   0h00m00s    0.0%",
        "coderabbit_review          0   0h00m00s    0.0%",
        "ruling                    40   0h00m40s   23.5%",
        "merge                      5   0h00m05s    2.9%",
        "shots                      0   0h00m00s    0.0%",
        "paused                     0   0h00m00s    0.0%",
        "other                      0   0h00m00s    0.0%",
        "total                    170   0h02m50s  100.0%",
    ]
    .join("\n");
    let (rig, runner) = with_history(three());
    assert_eq!(
        rig.ask(&runner, "timings", None),
        json!({
            "project": "koji",
            "items": 3,
            "issues": [7, 9, 12],
            "wall": 170,
            "seconds": {
                "worker": 70, "gpu_wait": 15, "local_round": 0, "claude_round": 0, "deep_round": 0,
                "judging": 0, "ci": 40, "coderabbit_window": 0, "coderabbit_review": 0,
                "ruling": 40, "merge": 5, "shots": 0, "paused": 0, "other": 0,
            },
            "table": table,
        })
    );
}

#[test]
fn a_state_file_from_before_history_answers_timings_with_zeros() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    drop(runner);
    let file = rig.paths().state;
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("history");
    std::fs::write(&file, serde_json::to_string(&state).unwrap()).unwrap();
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "timings", None)["items"], 0);
}

#[test]
fn a_dropped_work_item_is_recorded_with_what_it_spent() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.clock.advance(30);
    let status = rig.ask(&runner, "drop", None);
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(status["history"][0]["issue"], 7);
    assert_eq!(status["history"][0]["merged"], false);
    let t = rig.ask(&runner, "timings", None);
    assert_eq!((&t["issues"], &t["wall"]), (&json!([7]), &json!(30)));
    assert_eq!(secs(&t, "paused"), 30);
}

#[test]
fn status_lists_only_the_ten_most_recent_finished_work_items() {
    let history = (1..=12).map(|n| finished(n, true, &[])).collect();
    let (rig, runner) = with_history(history);
    let status = rig.ask(&runner, "status", None);
    let issues: Vec<_> = status["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["issue"].as_u64().unwrap())
        .collect();
    assert_eq!(issues, (3..=12).collect::<Vec<_>>());
    assert_eq!(rig.ask(&runner, "timings", Some("12"))["items"], 12);
}

// The record that would push the history past its cap and the removal of the
// work item it describes reach the file in one write.
#[test]
fn the_oldest_record_goes_in_the_write_that_removes_the_work_item() {
    let history = (101..101 + HISTORY_CAP as u64)
        .map(|n| finished(n, true, &[]))
        .collect();
    let (rig, runner) = with_history(history);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "drop", None);

    let saved = StateStore::new(rig.paths().state).load().unwrap().unwrap();
    assert!(saved.work_items.is_empty());
    let issues: Vec<_> = saved.history.iter().map(|f| f.issue).collect();
    assert_eq!(issues.len(), HISTORY_CAP);
    assert_eq!((issues.first(), issues.last()), (Some(&102), Some(&7)));
}
