use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use crate::lease::gpu::{Attempt, Claim, GpuLock};
use crate::pacer::{HoldKind, RECHECK_SECS};
use crate::ports::{Cost, Role, Timestamp, Usage};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Hold, Rig, Scripted};

const AGENTS: &str = "[agents.codex]\nharness = \"stand-in\"\n\
                      model = \"gpt-5-codex\"\neffort = \"medium\"\nusage = \"codex\"\n\
                      [agents.qwen]\nharness = \"stand-in\"\n\
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

#[test]
fn a_local_worker_is_never_paced_and_holds_the_gpu_for_its_whole_turn() {
    let (rig, runner) = named("rotom", "worker = \"qwen\"\n");
    // Both accounts past half their windows: neither is the local model's limit.
    rig.meter.set(Rig::utilization(90, 90));
    rig.codex_meter.set(Rig::utilization(90, 90));
    let lock = GpuLock::under(&rig.home.path().join("tmp"));
    rig.forge.list_ready(7, false);
    assert!(dispatched(&step(&runner).unwrap()));

    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    std::thread::scope(|s| {
        let turn = s.spawn(|| step(&runner).unwrap());
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the turn never began"
        );
        let holder = lock.holder().expect("the GPU is held during the turn");
        assert_eq!(holder.pid, Some(std::process::id()));
        assert!(
            holder.what.starts_with("kelpie worker call for #7 in "),
            "{}",
            holder.what
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["local_leases"]["gpu"]["what"], json!(holder.what));
        assert_eq!(status["pacer"].get("claude"), None);
        hold.release();
        turn.join().unwrap();
    });
    assert_eq!(lock.holder(), None, "let go when the turn ends");
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (0, 0));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["local_leases"], json!({ "gpu": null }));
}

#[test]
fn a_local_turn_waits_while_a_review_round_holds_the_gpu() {
    let (rig, runner) = named("xilriws", "worker = \"qwen\"\n");
    rig.ask(&runner, "add", Some("7"));
    let lock = GpuLock::under(&rig.home.path().join("tmp"));
    let round = Claim {
        pid: std::process::id(),
        what: "qwen-review round 1".into(),
    };
    assert_eq!(lock.try_take(&round).unwrap(), Attempt::Taken);
    rig.claude.script([Scripted::Say("done")]);
    std::thread::scope(|s| {
        let turn = s.spawn(|| step(&runner).unwrap());
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(rig.claude.calls(), [], "the turn waits on the lock");
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["local_leases"]["gpu"]["what"], "qwen-review round 1");
        lock.release(round.pid).unwrap();
        turn.join().unwrap();
    });
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn a_project_with_no_local_agent_shows_no_leases() {
    let (rig, runner) = named("chelone", "");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status.get("local_leases"), None);
}

#[test]
fn spend_without_dollars_shows_tokens_and_says_it_has_none() {
    let (rig, runner) = named("reactmap", "worker = \"qwen\"\n");
    rig.ask(&runner, "add", Some("7"));
    let used = Usage {
        input: 120,
        cache_write: 0,
        cache_read: 2_400,
        output: 35,
    };
    rig.claude.script([Scripted::Tokens(used)]);
    let Some(StepReport::Ended {
        usage, cost_usd, ..
    }) = step(&runner).unwrap()
    else {
        panic!("the turn did not end");
    };
    assert_eq!((usage, cost_usd), (used, None));

    let item = &rig.ask(&runner, "status", None)["work_item"];
    let tokens = json!({ "input": 120, "cache_write": 0, "cache_read": 2400, "output": 35 });
    assert_eq!(
        item["by_role"]["worker"],
        json!({ "calls": 1, "tokens": tokens, "cost_usd": null, "unpriced_calls": 1 })
    );
    let none = json!({ "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 });
    assert_eq!(
        item["by_role"]["judge"],
        json!({ "calls": 0, "tokens": none, "cost_usd": null })
    );
}

#[test]
fn a_role_with_priced_and_unpriced_calls_counts_both() {
    let mut item = crate::test::a_work_item();
    let session = item.session.clone();
    let tokens = |n| Usage {
        input: n,
        ..Usage::default()
    };
    let priced = item.record_call(
        Role::Worker,
        Timestamp(20),
        session.clone(),
        tokens(10),
        Some(Cost(9)),
    );
    let unpriced = item.record_call(Role::Worker, Timestamp(30), session, tokens(5), None);
    assert_eq!((priced, unpriced), (Some(Cost(3)), None));
    let worker = item.spend().worker;
    assert_eq!((worker.calls, worker.unpriced_calls), (3, 1));
    assert_eq!(worker.tokens.input, 1 + 10 + 5);
    assert_eq!(worker.cost_usd, Some(Cost(5 + 3).usd()));
}
