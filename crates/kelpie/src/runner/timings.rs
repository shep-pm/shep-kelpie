//! Where the time of the last finished work items went, totalled by phase

use std::fmt::Write as _;

use serde::Serialize;

use super::Runner;
use crate::work_item::{PhaseSeconds, TimingPhase};

/// How many finished work items `timings` totals when it is given no count
pub const DEFAULT_ITEMS: usize = 10;

/// What `timings` answers
#[derive(Debug, Serialize)]
pub struct TimingsReport {
    /// The project
    pub project: String,
    /// How many history entries were totalled
    pub items: usize,
    /// Their issues, oldest first
    pub issues: Vec<u64>,
    /// Their wall time together, in seconds
    pub wall_seconds: u64,
    /// Where it went, by phase
    pub seconds: PhaseSeconds,
    /// The same totals as plain text
    pub table: String,
}

impl Runner {
    /// Totals where the time of the last `n` finished work items went
    ///
    /// Takes fewer when the run history holds fewer, and none from an
    /// empty one.
    pub fn timings_report(&self, n: usize) -> TimingsReport {
        let history = &self.state.history;
        let last = &history[history.len().saturating_sub(n)..];
        let mut seconds = PhaseSeconds::default();
        for entry in last {
            seconds += entry.timings.seconds;
        }
        let wall_seconds = last.iter().map(|e| e.timings.seconds.total()).sum();
        TimingsReport {
            project: self.project.as_str().to_owned(),
            items: last.len(),
            issues: last.iter().map(|e| e.issue).collect(),
            wall_seconds,
            seconds,
            table: table(&seconds, wall_seconds),
        }
    }
}

// The phases, then the total, each with its time and its share of `wall`
fn table(seconds: &PhaseSeconds, wall: u64) -> String {
    let mut rows = vec![["phase".to_owned(), "time".to_owned(), "share".to_owned()]];
    for phase in TimingPhase::ALL {
        let s = seconds.get(phase);
        rows.push([phase.as_str().to_owned(), clock(s), share(s, wall)]);
    }
    rows.push(["total".to_owned(), clock(wall), share(wall, wall)]);
    let width = |col: usize| rows.iter().map(|r| r[col].len()).max().unwrap_or(0);
    let (a, b, c) = (width(0), width(1), width(2));
    let mut out = String::new();
    for [phase, time, share] in &rows {
        writeln!(out, "{phase:<a$}  {time:>b$}  {share:>c$}").expect("writing to a String");
    }
    out
}

// `H:MM:SS`, with hours unpadded and unbounded
fn clock(seconds: u64) -> String {
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

// The whole percent of `wall` that `seconds` is, half up; 0% of nothing
fn share(seconds: u64, wall: u64) -> String {
    let percent = if wall == 0 {
        0
    } else {
        (u128::from(seconds) * 200 + u128::from(wall)) / (u128::from(wall) * 2)
    };
    format!("{percent}%")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Timestamp};
    use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
    use crate::state::{FinishedItem, ProjectState, StateStore};
    use crate::test::{Hold, Rig, Scripted, git};
    use crate::work_item::Timings;

    fn saved(rig: &Rig) -> crate::state::ProjectState {
        StateStore::new(rig.paths().state)
            .load()
            .unwrap()
            .expect("a state file")
    }

    // A running project whose worker's first turn is held, while the worker
    // pushes `work.txt` to `kelpie/7` and the clock moves 30 seconds
    fn through_the_first_turn(rig: &Rig, runner: &Mutex<Runner>) {
        rig.ask(runner, "start", None);
        rig.ask(runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        let hold = Hold::default();
        rig.claude
            .script([Scripted::Hold(hold.clone()), Scripted::Text("CLEAN")]);
        std::thread::scope(|s| {
            let turn = s.spawn(|| step(runner).unwrap());
            assert!(
                hold.entered(Duration::from_secs(10)),
                "the turn never began"
            );
            rig.clock.advance(30);
            let tree = rig.worktree_7();
            std::fs::write(tree.join("work.txt"), "work\n").unwrap();
            git(&tree, &["add", "work.txt"]);
            git(&tree, &["commit", "--quiet", "-m", "work"]);
            git(&tree, &["push", "--quiet", "origin", "HEAD"]);
            hold.release();
            turn.join().unwrap();
        });
    }

    fn run_to_a_merge(rig: &Rig, runner: &Mutex<Runner>) -> StepReport {
        through_the_first_turn(rig, runner);
        rig.clock.advance(4);
        step(runner).unwrap(); // review round 1, qwen: clean by default
        rig.clock.advance(5);
        step(runner).unwrap(); // review round 2, claude
        let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
        rig.forge.set_checks(&head, Checks::Passed);
        rig.clock.advance(6);
        assert!(matches!(
            rig.verdict(runner),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        rig.clock.advance(70);
        rig.ask(runner, "rule", Some("1 yes"));
        step(runner).unwrap(); // marks the draft ready
        rig.clock.advance(CHECKS_SETTLE);
        step(runner).unwrap().expect("the merge")
    }

    #[test]
    fn a_merged_work_item_records_a_split_that_sums_to_its_wall_time() {
        let rig = Rig::new("timings-merge");
        let runner = rig.open().unwrap();
        let report = run_to_a_merge(&rig, &runner);
        let StepReport::Finished {
            merged: true,
            timings,
            ..
        } = report
        else {
            panic!("not a finished merge: {report:?}");
        };

        let history = saved(&rig).history;
        let [entry] = history.as_slice() else {
            panic!("one finished item, not {}", history.len());
        };
        assert_eq!(entry.timings, timings);
        assert!(entry.merged && entry.pull_request == Some(71));
        let started = timings.started.expect("counted from the start");
        assert_eq!(timings.seconds.total(), entry.at.0 - started.0);
        let s = timings.seconds;
        assert!(s.worker >= 30 && s.ci > 0 && s.ruling >= 70 && s.merge >= CHECKS_SETTLE);
    }

    #[test]
    fn a_drop_records_an_unmerged_entry() {
        let rig = Rig::new("timings-drop");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.ask(&runner, "pause", None);
        rig.clock.advance(90);
        rig.ask(&runner, "drop", None);
        let history = saved(&rig).history;
        let [
            FinishedItem {
                issue: 7,
                merged: false,
                pull_request: None,
                timings,
                ..
            },
        ] = history.as_slice()
        else {
            panic!("the drop is not recorded");
        };
        assert_eq!(timings.seconds.total(), 90);
        assert_eq!(timings.seconds.other, 90);
    }

    #[test]
    fn timings_survive_a_reopen_and_the_time_across_it_lands_where_the_item_was_saved() {
        let (rig, runner, _) = Rig::parked("timings-restart");
        let ruling = |t: &Timings| t.seconds.ruling;
        let before = saved(&rig).work_items[0].timings;
        assert!(before.started.is_some());
        drop(runner);

        rig.clock.advance(500);
        let reopened = rig.open().unwrap();
        rig.ask(&reopened, "pause", None);
        let after = saved(&rig).work_items[0].timings;
        assert_eq!(after.started, before.started);
        assert_eq!(ruling(&after), ruling(&before) + 500);
        assert_eq!(
            after.seconds.total() - before.seconds.total(),
            500,
            "every second across the restart went to the ruling"
        );
        assert_eq!(after.seconds.worker, before.seconds.worker);
    }

    fn entry(issue: u64, merged: bool, worker: u64, ci: u64, ruling: u64) -> FinishedItem {
        let seconds = PhaseSeconds {
            worker,
            ci,
            ruling,
            ..PhaseSeconds::default()
        };
        FinishedItem {
            issue,
            pull_request: Some(issue + 100),
            merged,
            at: Timestamp(Rig::EPOCH + issue),
            timings: Timings {
                seconds,
                ..Timings::default()
            },
        }
    }

    // A rig whose run history is `history` when the runner opens
    fn with_history(project: &str, history: Vec<FinishedItem>) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let mut state = ProjectState::new(Timestamp(Rig::EPOCH));
        state.history = history;
        StateStore::new(rig.paths().state).save(&state).unwrap();
        let runner = rig.open().unwrap();
        (rig, runner)
    }

    fn three() -> Vec<FinishedItem> {
        vec![
            entry(4, true, 100, 10, 5),
            entry(5, false, 200, 20, 0),
            entry(6, true, 300, 30, 60),
        ]
    }

    #[test]
    fn a_count_totals_the_last_finished_items() {
        let (rig, runner) = with_history("timings-count", three());
        let reply = rig.ask(&runner, "timings", Some("2"));
        assert_eq!(reply["project"], "timings-count");
        assert_eq!(reply["items"], 2);
        assert_eq!(reply["issues"], json!([5, 6]));
        assert_eq!(reply["wall_seconds"], 220 + 390);
        assert_eq!(
            reply["seconds"],
            json!({
                "worker": 500, "gpu_wait": 0, "local_round": 0, "claude_round": 0,
                "judging": 0, "ci": 50, "coderabbit_window": 0,
                "coderabbit_review": 0, "ruling": 60, "merge": 0, "shots": 0,
                "other": 0,
            })
        );
    }

    #[test]
    fn no_count_and_a_count_past_the_history_total_all_of_it() {
        let (rig, runner) = with_history("timings-all", three());
        for params in [None, Some("50"), Some(" 3 ")] {
            let reply = rig.ask(&runner, "timings", params);
            assert_eq!(reply["items"], 3, "{params:?}");
            assert_eq!(reply["issues"], json!([4, 5, 6]));
            assert_eq!(reply["wall_seconds"], 115 + 220 + 390);
            assert_eq!(reply["seconds"]["worker"], 600);
        }
    }

    #[test]
    fn the_default_count_is_ten() {
        let history = (1..=12).map(|i| entry(i, true, 1, 0, 0)).collect();
        let (rig, runner) = with_history("timings-ten", history);
        let reply = rig.ask(&runner, "timings", None);
        assert_eq!(reply["items"], DEFAULT_ITEMS);
        assert_eq!(reply["issues"], json!((3..=12).collect::<Vec<u64>>()));
    }

    #[test]
    fn a_count_that_is_not_one_is_refused_and_changes_nothing() {
        let (rig, runner) = with_history("timings-refused", three());
        let before = std::fs::read(rig.paths().state).unwrap();
        for bad in ["0", "x", "+2", "#2", "-1", "2 3"] {
            assert_eq!(
                rig.ask(&runner, "timings", Some(bad)),
                json!({ "error": format!("{bad:?} is not a count of finished work items") })
            );
        }
        assert_eq!(std::fs::read(rig.paths().state).unwrap(), before);
    }

    #[test]
    fn an_empty_history_totals_to_zeros() {
        let (rig, runner) = with_history("timings-empty", Vec::new());
        let reply = rig.ask(&runner, "timings", None);
        assert_eq!(reply["items"], 0);
        assert_eq!(reply["issues"], json!([]));
        assert_eq!(reply["wall_seconds"], 0);
        assert!(
            reply["seconds"]
                .as_object()
                .unwrap()
                .values()
                .all(|v| v == 0)
        );
        let table = reply["table"].as_str().unwrap();
        let rows: Vec<&str> = table.lines().collect();
        assert_eq!(rows.len(), 14);
        assert!(rows[1..].iter().all(|r| r.ends_with("0:00:00     0%")));
    }

    #[test]
    fn the_table_is_pinned() {
        let (rig, runner) = with_history(
            "timings-table",
            vec![entry(4, true, 3564, 36, 0), entry(5, true, 0, 0, 3600)],
        );
        let reply = rig.ask(&runner, "timings", None);
        assert_eq!(reply["wall_seconds"], 7200);
        assert_eq!(
            reply["table"],
            "\
phase                 time  share
worker             0:59:24    50%
gpu_wait           0:00:00     0%
local_round        0:00:00     0%
claude_round       0:00:00     0%
judging            0:00:00     0%
ci                 0:00:36     1%
coderabbit_window  0:00:00     0%
coderabbit_review  0:00:00     0%
ruling             1:00:00    50%
merge              0:00:00     0%
shots              0:00:00     0%
other              0:00:00     0%
total              2:00:00   100%
"
        );
    }

    #[test]
    fn a_share_rounds_half_up() {
        assert_eq!(share(1, 200), "1%");
        assert_eq!(share(99, 200), "50%");
        assert_eq!(share(199, 200), "100%");
        assert_eq!(share(1, 201), "0%");
        assert_eq!(share(5, 0), "0%");
    }

    #[test]
    fn hours_are_unpadded_and_unbounded() {
        assert_eq!(clock(0), "0:00:00");
        assert_eq!(clock(3661), "1:01:01");
        assert_eq!(clock(100 * 3600 + 59), "100:00:59");
    }

    #[test]
    fn a_merged_work_item_is_totalled_as_its_entry_has_it() {
        let rig = Rig::new("timings-rig");
        let runner = rig.open().unwrap();
        run_to_a_merge(&rig, &runner);
        let history = saved(&rig).history;
        let [entry] = history.as_slice() else {
            panic!("one finished item, not {}", history.len());
        };
        let reply = rig.ask(&runner, "timings", Some("1"));
        assert_eq!(reply["items"], 1);
        assert_eq!(reply["issues"], json!([7]));
        assert_eq!(reply["wall_seconds"], entry.timings.seconds.total());
        assert_eq!(
            reply["seconds"],
            serde_json::to_value(entry.timings.seconds).unwrap()
        );
    }
}
