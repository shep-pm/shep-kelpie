use crate::test::ticket;
use serde_json::json;

use super::*;
use crate::usage::baselines::Baseline;
use crate::work_item::{CiFailures, QwenTally};

fn usage(input: u64, cache_write: u64, cache_read: u64, output: u64) -> Usage {
    Usage {
        input,
        cache_write,
        cache_write_5m: 0,
        cache_read,
        output,
    }
}

#[test]
fn units_weigh_each_kind_of_token_as_the_control_room_did() {
    assert_eq!(units(usage(1, 0, 0, 0)), 1);
    assert_eq!(units(usage(0, 1, 0, 0)), 2);
    assert_eq!(units(usage(0, 0, 10, 0)), 1);
    assert_eq!(units(usage(0, 0, 0, 1)), 5);
    assert_eq!(units(usage(100, 1000, 100_000, 10)), 12_150);
    assert_eq!(units(usage(0, 0, 4, 0)), 0, "under half a unit rounds down");
    assert_eq!(units(usage(0, 0, 5, 0)), 1, "half a unit rounds up");
    let split = Usage {
        cache_write_5m: 400,
        ..usage(0, 1000, 0, 0)
    };
    assert_eq!(
        units(split),
        600 * 2 + 500,
        "a five-minute write weighs 1.25"
    );
}

fn finished(issue: u64, merged: bool, worker: u64, reviewer: u64, wall: u64) -> Line {
    let role = |units: u64, cost_usd: f64| RoleLine {
        calls: 1,
        tokens: Usage::default(),
        units,
        cost_usd,
        unpriced_calls: 0,
    };
    Line::Finished(FinishedLine {
        at: Timestamp(1000 + issue),
        issue: ticket(issue),
        title: format!("Issue {issue}"),
        pull_request: Some(issue + 100),
        merged,
        closed: false,
        wall,
        units: worker + reviewer,
        cost_usd: (worker + reviewer) as f64 / 1000.0,
        worker: role(worker, worker as f64 / 1000.0),
        reviewer: role(reviewer, reviewer as f64 / 1000.0),
        qwen: QwenTally::default(),
        review_rounds: 2,
        fix_turns: 1,
        rulings: (issue % 2) as u32,
        agent: None,
        harness: None,
        model: None,
        kind: None,
        tier: None,
        ci_fix_turns: 0,
        ci_failures: CiFailures::default(),
        held: HeldFindings::default(),
        disputed: 0,
        deferred: 0,
    })
}

fn call(at: u64, issue: Option<u64>, role: Role, kind: CallKind, units: u64) -> Line {
    let mut line: CallLine = serde_json::from_value(json!({
        "at": at, "issue": issue, "role": role, "kind": kind,
        "usage": { "input": units, "cache_write": 0, "cache_read": 0, "output": 0 },
        "units": units, "cost_usd": 0.5, "ended": "answered",
    }))
    .unwrap();
    line.pull_request = issue.map(|i| i + 100);
    Line::Call(line)
}

#[test]
fn a_report_gives_each_merged_pull_request_its_medians_and_the_other_roles() {
    let mut writer = retired(902, "issue-writer", 200);
    writer["kind"] = "issues".into();
    let mut lines = [
        finished(1, true, 3000, 1000, 3600),
        finished(2, true, 1000, 1000, 600),
        finished(3, true, 8000, 2000, 7200),
        finished(4, false, 500, 0, 60),
        finished(5, false, 40, 0, 30),
        call(1000, Some(1), Role::Worker, CallKind::Turn, 4000),
        call(1001, Some(2), Role::Worker, CallKind::Turn, 2000),
        call(1002, Some(3), Role::Worker, CallKind::Turn, 10_000),
        call(1003, Some(4), Role::Worker, CallKind::Turn, 500),
        call(900, None, Role::Pm, CallKind::Wake, 400),
        call(901, None, Role::Pm, CallKind::Compact, 300),
        serde_json::from_value(writer).unwrap(),
        call(903, Some(9), Role::Worker, CallKind::Turn, 70),
    ];
    // Two of #3's worker calls, one of the dropped item's reviewer calls and
    // a retired issue writer's run reported no cost.
    if let Line::Finished(item) = &mut lines[2] {
        item.worker.unpriced_calls = 2;
    }
    if let Line::Finished(item) = &mut lines[3] {
        item.reviewer.unpriced_calls = 1;
    }
    if let Line::Finished(item) = &mut lines[4] {
        (item.closed, item.pull_request) = (true, None);
    }
    if let Line::Call(call) = &mut lines[11] {
        (call.unpriced, call.cost_usd) = (true, None);
    }
    let baselines = [Baseline {
        name: "control room".into(),
        window: Some("a week".into()),
        units_per_merged_pr: 8000.0,
    }];
    let out = report("koji", &lines, None, &baselines);
    assert_eq!(
        out,
        [
            "koji: 3 merged pull requests",
            "pr      issue           units   dollars  unpriced       wall  rulings  worker  \
             reviewer",
            "#101    #1              4,000     $4.00         0   1h00m00s        1     75%       \
             25%",
            "#102    #2              2,000     $2.00         0   0h10m00s        0     50%       \
             50%",
            "#103    #3             10,000    $10.00         2   2h00m00s        1     80%       \
             20%",
            "median                  4,000     $4.00         0   1h00m00s        1     75%       \
             25%",
            "the median is of merged items' own calls",
            "dollars count priced calls only: 2 calls of 1 merged item report no cost, the \
             median's dollars included",
            "loaded: 5,823 units, $1.17 + 1 unpriced call per merged pull request, counting \
             every call: the project manager's, the retired roles', and those of work items \
             dropped or closed with no change too",
            "baseline control room (a week): 8,000 units per merged pull request; kelpie / \
             baseline, below 1 is cheaper: 0.73 loaded, 0.50 median",
            "closed with no change: 1 work item, 40 units, $0.04",
            "dropped: 1 work item, 500 units, $0.50 + 1 unpriced call",
            "",
            "project manager: 2 calls (1 compaction), 700 units, $1.00",
            "retired roles: 1 call, 200 units, $0.00 + 1 unpriced call (issue-writer 1 call)",
            "no finished record, open or from before the ledger:",
            "  #9 (#109): 1 call, 70 units, $0.50",
        ]
    );

    let later = report("koji", &lines, Some(Timestamp(1002)), &[]);
    assert_eq!(later[0], "koji: 2 merged pull requests");
    assert!(
        later.contains(&"project manager: 0 calls, 0 units, $0.00".to_owned()),
        "calls before the date are left out: {later:#?}"
    );
    assert!(
        later[7].starts_with("loaded: 5,250 units, $0.50 per merged pull request"),
        "{later:#?}"
    );
}

#[test]
fn a_report_totals_the_retros_apart_from_the_builds_and_the_project_managers_wakes() {
    let lines = [
        finished(1, true, 3000, 1000, 3600),
        call(1000, Some(1), Role::Worker, CallKind::Turn, 4000),
        call(1000, None, Role::Pm, CallKind::Wake, 90),
        call(1001, None, Role::Pm, CallKind::Retro, 250),
        call(1002, None, Role::Pm, CallKind::Retro, 150),
    ];
    let out = report("koji", &lines, None, &[]);
    assert!(
        out.contains(&"retro: 2 calls, 400 units, $1.00".to_owned()),
        "{out:#?}"
    );
    assert!(
        out.contains(&"project manager: 1 call, 90 units, $0.50".to_owned()),
        "{out:#?}"
    );

    let none = report("koji", &lines[..2], None, &[]);
    assert!(!none.iter().any(|l| l.starts_with("retro")), "{none:#?}");
}

// A call line of a role this build no longer has, as a build with the panel
// wrote its experts' votes
fn retired(at: u64, role: &str, units: u64) -> serde_json::Value {
    json!({
        "line": "call", "at": at, "issue": null, "role": role, "kind": "vote",
        "usage": { "input": units, "cache_write": 0, "cache_read": 0, "output": 0 },
        "units": units, "cost_usd": 0.5, "seconds": 5, "ended": "answered",
    })
}

#[test]
fn a_call_of_a_retired_role_still_reads_and_an_unknown_kind_of_line_is_passed_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    let wake = json!({
        "line": "call", "at": 101, "issue": null, "role": "pm", "kind": "wake",
        "usage": { "input": 10, "cache_write": 0, "cache_read": 0, "output": 0 },
        "units": 10, "cost_usd": 0.5, "ended": "answered",
    });
    let future = json!({ "line": "forecast", "at": 102 });
    let text = format!("{}\n{wake}\n{future}\n", retired(100, "advisor", 10));
    std::fs::write(&path, text).unwrap();

    let lines = read(&path).unwrap();

    let calls: Vec<(&LedgerRole, &LedgerKind)> = (lines.iter())
        .filter_map(|line| match line {
            Line::Call(call) => Some((&call.role, &call.kind)),
            Line::Finished(_) => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            (
                &LedgerRole::Retired("advisor".into()),
                &LedgerKind::Retired("vote".into())
            ),
            (&Role::Pm.into(), &CallKind::Wake.into()),
        ],
        "{lines:?}"
    );
}

#[test]
fn a_report_totals_the_retired_roles_calls_by_name_and_counts_them_in_the_load() {
    let old = |at, role, units| serde_json::from_value::<Line>(retired(at, role, units)).unwrap();
    let lines = [
        finished(1, true, 3000, 1000, 3600),
        call(1000, Some(1), Role::Worker, CallKind::Turn, 4000),
        old(900, "advisor", 300),
        old(901, "researcher", 200),
        old(902, "advisor", 100),
    ];

    let out = report("koji", &lines, None, &[]);

    assert!(
        out.contains(
            &"retired roles: 3 calls, 600 units, $1.50 (advisor 2 calls, researcher 1 call)"
                .to_owned()
        ),
        "{out:#?}"
    );
    let loaded = out.iter().find(|l| l.starts_with("loaded: ")).unwrap();
    assert!(
        loaded.starts_with("loaded: 4,600 units, $2.00 "),
        "{loaded}"
    );

    let none = report("koji", &lines[..2], None, &[]);
    assert!(!none.iter().any(|l| l.starts_with("retired")), "{none:#?}");
}

#[test]
fn a_ledger_line_reads_back_and_a_torn_one_is_passed_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("koji").join(FILE);
    let line = call(5, Some(7), Role::Reviewer, CallKind::Review, 12);
    append_to(&path, &line).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"line\":\"call\",\"at\":").unwrap();
    append_to(&path, &line).unwrap();
    // A crash mid-way through a two-byte character, as in a title.
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"line\":\"finished\",\"title\":\"caf\xc3")
        .unwrap();
    append_to(&path, &line).unwrap();
    assert_eq!(read(&path).unwrap(), [line.clone(), line.clone(), line]);
    assert_eq!(read(&dir.path().join("none.jsonl")).unwrap(), []);
}

#[test]
fn baselines_are_read_from_kelpie_s_home_and_skipped_when_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(baselines::load(dir.path()), []);
    let file = dir.path().join(baselines::FILE);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not json").unwrap();
    assert_eq!(baselines::load(dir.path()), []);
    let fixture = json!({
        "units": "cache read 0.1, cache write 2, output 5, input 1",
        "baselines": [{
            "name": "a stand-in", "window": "a test", "units": 2000.0,
            "merged_prs": 2, "units_per_merged_pr": 1000.0, "note": "made up",
        }],
    });
    std::fs::write(&file, fixture.to_string()).unwrap();
    assert_eq!(
        baselines::load(dir.path()),
        [Baseline {
            name: "a stand-in".into(),
            window: Some("a test".into()),
            units_per_merged_pr: 1000.0,
        }]
    );
}

#[test]
fn a_ledger_that_cannot_be_written_fails_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the project's folder would be, so no ledger can go in it.
    let folder = dir.path().join("koji");
    std::fs::write(&folder, "").unwrap();
    let mut ledger = Ledger::in_folder(&folder);
    let session = SessionId("pm-1".into());
    assert_eq!(ledger.session_cost(&session), None);
    let Line::Call(mut line) = call(5, None, Role::Pm, CallKind::Wake, 1) else {
        unreachable!()
    };
    line.session = Some("pm-1".into());
    line.session_cost_usd = Some(1.5);
    ledger.append(&Line::Call(line));
    ledger.append(&call(6, None, Role::Pm, CallKind::Wake, 1));
    assert!(ledger.failing);
    assert_eq!(
        ledger.session_cost(&session),
        None,
        "a line not written leaves its session's cost where the file has it"
    );
}

#[test]
fn a_session_s_cost_is_its_last_line_s() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = Ledger::in_folder(dir.path());
    let session = SessionId("pm-1".into());
    assert_eq!(ledger.session_cost(&session), None);
    let costing = |at: u64, usd: f64| {
        let Line::Call(mut line) = call(at, None, Role::Pm, CallKind::Wake, 1) else {
            unreachable!()
        };
        (line.session, line.session_cost_usd) = (Some("pm-1".into()), Some(usd));
        Line::Call(line)
    };
    ledger.append(&costing(5, 1.5));
    assert_eq!(ledger.session_cost(&session), Cost::from_usd(1.5));
    ledger.append(&costing(6, 2.25));
    assert_eq!(ledger.session_cost(&session), Cost::from_usd(2.25));
    let mut fresh = Ledger::in_folder(dir.path());
    assert_eq!(
        fresh.session_cost(&session),
        Cost::from_usd(2.25),
        "a restarted runner reads the last from the file"
    );
}

#[test]
fn a_ledger_that_cannot_be_read_is_read_again_for_a_session_s_cost() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the ledger would be, so reading it fails.
    std::fs::create_dir_all(dir.path().join(FILE)).unwrap();
    let mut ledger = Ledger::in_folder(dir.path());
    let session = SessionId("pm-1".into());
    assert_eq!(ledger.session_cost(&session), None);
    std::fs::remove_dir(dir.path().join(FILE)).unwrap();
    let Line::Call(mut line) = call(5, None, Role::Pm, CallKind::Wake, 1) else {
        unreachable!()
    };
    (line.session, line.session_cost_usd) = (Some("pm-1".into()), Some(1.5));
    append_to(&dir.path().join(FILE), &Line::Call(line)).unwrap();
    assert_eq!(ledger.session_cost(&session), Cost::from_usd(1.5));
}

#[test]
fn a_report_totals_the_representatives_posts_apart_from_the_work_items() {
    let lines = [
        call(1, None, Role::Representative, CallKind::Post, 300),
        call(2, None, Role::Representative, CallKind::Post, 200),
    ];

    let report = report("koji", &lines, None, &[]);

    let total = report.iter().find(|l| l.starts_with("representative:"));
    let total = total.unwrap_or_else(|| panic!("no representative total in {report:#?}"));
    assert!(
        total.starts_with("representative: 2 calls, 500 units, $1.00"),
        "{total}"
    );
}

#[test]
fn triager_calls_total_on_a_line_of_their_own() {
    let lines = [
        call(1, None, Role::Triager, CallKind::Triage, 300),
        call(2, None, Role::Triager, CallKind::Triage, 200),
        call(3, None, Role::Representative, CallKind::Post, 100),
    ];

    let report = report("koji", &lines, None, &[]);

    let total = report.iter().find(|l| l.starts_with("triager:"));
    let total = total.unwrap_or_else(|| panic!("no triager total in {report:#?}"));
    assert!(
        total.starts_with("triager: 2 calls, 500 units, $1.00"),
        "{total}"
    );
}

fn ledger_line(text: &str) -> Line {
    serde_json::from_str(text).unwrap()
}

#[test]
fn the_ledger_comes_out_as_the_counters_the_runner_sends_each_at_its_lines_time() {
    let call = |at: u64, input: u64| {
        ledger_line(&format!(
            r#"{{"line":"call","at":{at},"issue":7,"role":"worker","kind":"turn","agent":"sonnet-high",
            "harness":"claude-code","model":"sonnet","effort":"high",
            "usage":{{"input":{input},"cache_write":0,"cache_read":0,"output":0}},"units":{input},
            "cost_usd":0.25,"compactions":1,"ended":"answered"}}"#
        ))
    };
    let item = ledger_line(
        r#"{"line":"finished","at":200,"issue":7,"title":"t","pull_request":71,"merged":true,
        "wall":90,"units":30,"cost_usd":0.5,
        "worker":{"calls":2,"tokens":{"input":30,"cache_write":0,"cache_read":0,"output":0},"units":30,"cost_usd":0.5},
        "reviewer":{"calls":0,"tokens":{"input":0,"cache_write":0,"cache_read":0,"output":0},"units":0,"cost_usd":0.0},
        "qwen":{"rounds":0,"seconds":0},"review_rounds":2,"fix_turns":1,"rulings":0,
        "held":{"high":1,"medium":2}}"#,
    );
    // Imports come last in the file and are not in order
    let out = openmetrics(&[("koji".to_owned(), vec![call(150, 20), item, call(100, 10)])]);
    let calls = r#"kelpie_calls_total{sheep="koji",agent="sonnet-high",effort="high",ended="answered",harness="claude-code",kind="turn",role="worker"}"#;
    assert!(out.contains(&format!("{calls} 1 100")), "{out:#?}");
    assert!(out.contains(&format!("{calls} 2 150")), "{out:#?}");
    let merged = r#"kelpie_work_items_finished_total{sheep="koji",agent="unknown",kind="none",outcome="merged"} 1 200"#;
    assert!(out.contains(&merged.to_owned()), "{out:#?}");
    let dollars =
        r#"kelpie_work_item_cost_usd_total{sheep="koji",agent="unknown",kind="none"} 0.5 200"#;
    assert!(out.contains(&dollars.to_owned()), "{out:#?}");
    let high = r#"kelpie_findings_total{sheep="koji",severity="high"} 1 200"#;
    assert!(out.contains(&high.to_owned()), "{out:#?}");
    let moves =
        r#"kelpie_compactions_total{sheep="koji",harness="claude-code",role="worker"} 2 150"#;
    assert!(out.contains(&moves.to_owned()), "{out:#?}");
    assert!(out.contains(&"# TYPE kelpie_calls counter".to_owned()));
    assert_eq!(out.last().unwrap(), "# EOF");
}

#[test]
fn a_report_names_an_unrecorded_work_item_by_its_ticket_id() {
    let mut lines = vec![
        finished(1, true, 3000, 1000, 3600),
        call(1000, Some(1), Role::Worker, CallKind::Turn, 4000),
        call(1001, Some(9), Role::Worker, CallKind::Turn, 70),
    ];
    // A call line for a ticket from another source, with no finished line for
    // it: open still, or run before the ledger kept finished lines.
    let local: CallLine = serde_json::from_value(json!({
        "at": 1002, "issue": "k12", "role": "worker", "kind": "turn",
        "usage": { "input": 60, "cache_write": 0, "cache_read": 0, "output": 0 },
        "units": 60, "cost_usd": 0.3, "ended": "answered",
    }))
    .unwrap();
    lines.push(Line::Call(local));
    let out = report("koji", &lines, None, &[]);
    assert!(
        out.contains(&"  #9 (#109): 1 call, 70 units, $0.50".to_owned()),
        "{out:#?}"
    );
    assert!(
        out.contains(&"  k12: 1 call, 60 units, $0.30".to_owned()),
        "{out:#?}"
    );
}
