use std::sync::Mutex;

use serde_json::json;

use crate::pacer::{HoldKind, RECHECK_SECS};
use crate::ports::Role;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted};

const AGENTS: &str = "[agents.codex]\nharness = \"claude-code\"\n\
                      model = \"gpt-5-codex\"\neffort = \"medium\"\nusage = \"codex\"\n\
                      [agents.qwen]\nharness = \"claude-code\"\n\
                      model = \"qwen3-coder\"\neffort = \"low\"\nusage = \"none\"\n";

// A running project whose roles name kelpie's agents as `names` says.
fn named(project: &str, names: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!("{kelpie}\n{AGENTS}"));
    rig.edit_settings(|s| {
        let gate = "\n[app.dogs.kelpie.coderabbit]\n";
        let agents = format!("[app.dogs.kelpie.agents]\n{names}");
        s.replacen(gate, &format!("\n{agents}{gate}"), 1)
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    (rig, runner)
}

fn held(report: Option<StepReport>) -> (HoldKind, String) {
    match report {
        Some(StepReport::Held { kind, reason, .. }) => (kind, reason),
        other => panic!("expected a hold, got {other:?}"),
    }
}

fn dispatched(report: &Option<StepReport>) -> bool {
    matches!(report, Some(StepReport::Dispatched { issue: 7, .. }))
}

#[test]
fn a_claude_worker_waits_on_claudes_window_and_never_reads_codexs() {
    let (rig, runner) = named("shep", "");
    rig.forge.list_ready(7, false);
    rig.meter.set(Rig::utilization(0, 55));
    rig.codex_meter.set(Rig::utilization(0, 0));

    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Window);
    assert!(
        reason.starts_with("the 5-hour window is at 55%"),
        "{reason}"
    );
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (1, 0));
}

#[test]
fn a_codex_worker_paces_on_codexs_own_windows() {
    let (rig, runner) = named("koji", "worker = \"codex\"\n");
    rig.forge.list_ready(7, false);
    // Claude's account is past half its window: nothing of Codex's.
    rig.meter.set(Rig::utilization(30, 90));
    assert!(dispatched(&step(&runner).unwrap()));
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (0, 1));

    rig.codex_meter.set(Rig::utilization(0, 55));
    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Window);
    assert_eq!(
        reason,
        "Codex's 5-hour window is at 55%, past the 50% mark, so no turn starts until it resets"
    );
    assert_eq!(rig.claude.calls(), []);
    let pacer = &rig.ask(&runner, "status", None)["pacer"];
    assert_eq!(pacer["codex"]["holding"]["kind"], "window");
    assert_eq!(pacer["codex"]["reading"]["session"]["used_pct"], 55);
    assert_eq!(
        pacer.get("claude"),
        None,
        "no Claude agent, no Claude reading"
    );

    rig.clock.advance(RECHECK_SECS);
    rig.codex_meter.set(Rig::utilization(0, 3));
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 1);
    assert_eq!(rig.meter.reads(), 0);
}

#[test]
fn each_account_keeps_its_own_day() {
    let (rig, runner) = named("golbat", "worker = \"codex\"\n");
    rig.forge.list_ready(7, false);
    rig.codex_meter.set(Rig::utilization(20, 0));
    assert!(dispatched(&step(&runner).unwrap()));
    let state = crate::state::StateStore::new(rig.paths().state)
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(state.pacing, None);
    assert_eq!(state.codex_pacing.map(|d| d.week_used_pct), Some(20));

    // 15% more of Codex's week, past its allowance of 80 / 7.
    rig.codex_meter.set(Rig::utilization(35, 0));
    let reading = rig.ask(&runner, "status", None)["pacer"]["codex"]["reading"].clone();
    assert_eq!(
        reading["spent_today_pct"],
        json!(0),
        "read at dispatch only"
    );
    rig.ask(&runner, "drop", Some("7"));
    rig.forge.list_ready(8, false);
    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Allowance);
    assert!(
        reason.starts_with("today's Codex allowance of 11.4% of the week is spent (15%"),
        "{reason}"
    );
}

#[test]
fn a_review_round_waits_on_its_reviewers_account_while_the_worker_works_on() {
    let (rig, first) = named("chelone", "worker = \"codex\"\n");
    drop(first);
    let only_claude = "loop_guard = 8\nreviewers = [\"claude\"]\n";
    rig.edit_settings(|s| s.replace("loop_guard = 8\n", only_claude));
    let runner = rig.open().unwrap();
    rig.meter.set(Rig::utilization(0, 60));
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    let mut hold = None;
    for _ in 0..10 {
        if let Some(StepReport::Held { kind, reason, .. }) = rig.verdict(&runner) {
            hold = Some((kind, reason));
            break;
        }
    }
    let (kind, reason) = hold.expect("the Claude round was held");
    assert_eq!(kind, HoldKind::Window);
    assert!(
        reason.starts_with("the 5-hour window is at 60%"),
        "{reason}"
    );
    let roles: Vec<Role> = rig.claude.all_calls().iter().map(|c| c.role).collect();
    assert_eq!(roles, [Role::Worker]);
    let pacer = &rig.ask(&runner, "status", None)["pacer"];
    assert_eq!(pacer["claude"]["holding"]["kind"], "window");
    assert_eq!(pacer["codex"]["holding"], json!(null));

    rig.clock.advance(RECHECK_SECS);
    rig.meter.set(Rig::utilization(0, 1));
    rig.claude.script([Scripted::Text("CLEAN")]);
    for _ in 0..5 {
        rig.verdict(&runner);
    }
    let roles: Vec<Role> = rig.claude.all_calls().iter().map(|c| c.role).collect();
    assert_eq!(roles, [Role::Worker, Role::Reviewer]);
}
