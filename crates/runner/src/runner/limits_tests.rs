use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use crate::pacer::{HoldKind, RECHECK_SECS};
use crate::ports::{CLEAN, Cost, MeterError, Role, Timestamp, Usage};
use crate::runner::{Runner, StepReport, step};
use crate::settings::AgentHarness;
use crate::test::ticket;
use crate::test::{Hold, Rig, Scripted, StandInEndpoint};

const CODEX: &str = Rig::CODEX_IMPLEMENTER;
const QWEN: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                    effort: low\nusage: none\n---\n";

/// The example's own `[agents]` table, which `named` replaces
const LISTED: &str = "[app.dogs.kelpie.agents]\nimplementers = [\"sonnet-high\"]\n";

const CODEX_REVIEW: &str = "---\nrole: reviewer\nharness: stand-in\nmodel: gpt-5-codex\n\
                            effort: medium\nusage: codex\n---\nReview {{DIFF}}.\n";
const CODER_REVIEW: &str = "---\nrole: reviewer\nharness: stand-in\nmodel: qwen3-coder\n\
                            effort: low\nusage: none\n---\nReview {{DIFF}}.\n";

const ALL_CODEX: &str = "implementers = [\"codex\"]\nreviewers = [\"qwen\", \"codex-review\"]\n";
const BESIDE_QWEN: &str = "implementers = [\"sonnet-high\", \"coder\"]\n";
const ALL_QWEN: &str = "implementers = [\"sonnet-high\", \"coder\"]\nreviewers = [\"qwen\"]\n";

// A running project whose `[agents]` table is `names`, over kelpie's agent
// files and four more: `codex` on the Codex account and `coder` on a local model,
// which build, and a reviewer on each, `codex-review` and `coder-review`.
fn named(project: &str, names: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.write_agent("codex", CODEX);
    rig.write_agent("coder", QWEN);
    rig.write_agent("codex-review", CODEX_REVIEW);
    rig.write_agent("coder-review", CODER_REVIEW);
    rig.edit_settings(|s| {
        assert!(s.contains(LISTED), "the example's agents table moved");
        let s = s.replace(LISTED, &format!("[app.dogs.kelpie.agents]\n{names}"));
        match names.contains("reviewers = ") {
            true => s.replace(crate::test::RIG_REVIEWERS, ""),
            false => s,
        }
    });
    let runner = rig.open().unwrap();
    (rig, runner)
}

fn held(report: Option<StepReport>) -> (HoldKind, String) {
    match report {
        Some(StepReport::Held { kind, reason, .. }) => (kind, reason),
        other => panic!("expected a hold, got {other:?}"),
    }
}

fn dispatched(report: &Option<StepReport>) -> bool {
    matches!(report, Some(StepReport::Dispatched { issue, .. }) if *issue == ticket(7))
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
fn an_item_on_an_unlisted_implementer_paces_on_its_account_too() {
    let (rig, runner) = named("shep", "");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    rig.forge.label(7, "agent:codex!");
    rig.ask(&runner, "add", Some("7"));
    rig.codex_meter.set(Rig::utilization(0, 55));
    rig.forge.list_ready(8, false);

    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Window);
    assert!(
        reason.starts_with("Codex's 5-hour window is at 55%"),
        "{reason}"
    );
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (0, 1));
    let pacer = rig.ask(&runner, "status", None)["pacer"].clone();
    assert_eq!(pacer["codex"]["holding"]["kind"], "window");
    assert!(
        pacer.get("claude").is_some(),
        "the listed implementer's account is read too: {pacer}"
    );
}

// The `named("shep", "")` rig, reopened on a Codex day that began with `used` percent of
// its week spent, with a ready issue 7 pinned to `codex`, which the project does not list.
fn codex_day_began(used: u32) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = named("shep", "");
    let runner = reopened(&rig, runner, || rig.codex_day_began(used));
    rig.forge.list_ready(7, false);
    rig.forge.label(7, "agent:codex!");
    (rig, runner)
}

#[test]
fn a_first_pick_the_pacer_holds_gives_the_boards_second() {
    let (rig, runner) = named("shep", "");
    let runner = reopened(&rig, runner, || rig.codex_day_began(0));
    rig.forge.list_ready(7, false);
    rig.agent_board.ranks(&["codex", "sonnet-high"]);
    rig.codex_meter.set(Rig::utilization(15, 0));

    let report = step(&runner).unwrap();

    assert!(
        matches!(&report, Some(StepReport::Dispatched { issue, agent, .. })
            if *issue == ticket(7) && agent.as_str() == "sonnet-high"),
        "{report:?}"
    );
    assert_eq!(
        rig.codex_meter.reads(),
        1,
        "the first pick's account is read"
    );
}

// A local implementer listed alone, so no account is read before the board's ranking is.
const ONLY_QWEN: &str = "implementers = [\"coder\"]\nreviewers = [\"qwen\"]\n";

#[test]
fn a_later_pick_whose_account_is_unread_is_read_before_the_walk_gives_up() {
    let (rig, runner) = named("shep", ONLY_QWEN);
    let runner = reopened(&rig, runner, || rig.codex_day_began(0));
    rig.forge.list_ready(7, false);
    rig.agent_board.ranks(&["codex", "sonnet-high"]);
    rig.codex_meter.set(Rig::utilization(15, 0));
    rig.meter.set(Rig::utilization(0, 0));

    let report = step(&runner).unwrap();

    assert!(
        matches!(&report, Some(StepReport::Dispatched { issue, agent, .. })
            if *issue == ticket(7) && agent.as_str() == "sonnet-high"),
        "{report:?}"
    );
    assert_eq!((rig.codex_meter.reads(), rig.meter.reads()), (1, 1));
}

#[test]
fn a_paced_pin_waits_and_opens_on_no_other_agent() {
    let (rig, runner) = codex_day_began(0);
    rig.agent_board.ranks(&["codex", "sonnet-high"]);
    rig.codex_meter.set(Rig::utilization(15, 0));

    let (kind, _) = held(step(&runner).unwrap());

    assert_eq!(kind, HoldKind::Allowance);
    let names: Vec<_> = rig
        .agent_board
        .asks()
        .iter()
        .map(|a| a.pin.clone())
        .collect();
    assert_eq!(names, [Some(crate::settings::AgentName::kelpies("codex"))]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_items"], json!([]));
}

#[test]
fn a_ready_issue_on_an_unlisted_labelled_implementer_is_held_by_its_account() {
    let (rig, runner) = codex_day_began(0);
    // 15% of Codex's week since the day began, past its allowance of 100 / 7.
    rig.codex_meter.set(Rig::utilization(15, 0));

    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Allowance);
    assert!(
        reason.starts_with("today's Codex allowance of 14.3% of the week is spent (15%"),
        "{reason}"
    );
    // The listed implementer's account is read as ever, and the labelled one's once.
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (1, 1));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_items"], json!([]));
    let hold = json!([{ "reason": "paced", "issue": "7", "hold": reason }]);
    assert_eq!(status["skipped"], hold);
}

#[test]
fn a_pick_pinning_an_unlisted_implementer_is_held_by_its_account() {
    let (rig, runner) = named("shep", "implementers = [\"sonnet-high\"]\npm = \"pm\"\n");
    let runner = reopened(&rig, runner, || rig.codex_day_began(0));
    rig.forge.list_ready(7, false);
    rig.forge.list_ready(9, false);
    rig.codex_meter.set(Rig::utilization(15, 0));
    rig.claude.script([Scripted::Say(
        "{\"pick\": {\"issue\": 7, \"agent\": \"codex\"}, \"why\": \"w\"}",
    )]);
    let answered = step(&runner).unwrap();
    assert!(
        matches!(&answered, Some(StepReport::PmAnswered { acted, .. }) if acted == &["picked #7"]),
        "{answered:?}"
    );

    let opened = step(&runner).unwrap();

    assert!(
        !matches!(opened, Some(StepReport::Dispatched { issue, .. }) if issue == ticket(7)),
        "{opened:?}"
    );
    assert_eq!(
        rig.codex_meter.reads(),
        1,
        "the pinned agent's account is read"
    );
    let status = rig.ask(&runner, "status", None);
    let skipped = status["skipped"].as_array().unwrap();
    let paced = skipped.iter().find(|s| s["issue"] == "7").unwrap();
    assert_eq!(paced["reason"], "paced");
    let hold = paced["hold"].as_str().unwrap();
    assert!(
        hold.starts_with("today's Codex allowance of 14.3% of the week is spent (15%"),
        "{hold}"
    );
}

#[test]
fn a_ready_issue_on_an_unlisted_labelled_implementer_dispatches_when_its_account_is_clear() {
    let (rig, runner) = codex_day_began(0);
    rig.codex_meter.set(Rig::utilization(5, 0));

    assert!(dispatched(&step(&runner).unwrap()));
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (1, 1));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["agent"], "codex");
}

#[test]
fn an_issue_held_by_its_labelled_account_never_holds_a_ready_issue_behind_it() {
    let (rig, runner) = codex_day_began(0);
    rig.codex_meter.set(Rig::utilization(15, 0));
    rig.forge.list_ready(8, false);

    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)),
        "{report:?}"
    );
    let skipped = rig.ask(&runner, "status", None)["skipped"].clone();
    assert_eq!(skipped[0]["reason"], "paced");
    assert_eq!(skipped[0]["issue"], "7");
}

#[test]
fn a_project_all_on_codex_paces_on_codexs_own_windows() {
    let (rig, runner) = named("koji", ALL_CODEX);
    rig.forge.list_ready(7, false);
    // Claude's account is past half its window: no role of this project spends it.
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
    let (rig, runner) = named("golbat", ALL_CODEX);
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
fn a_new_work_item_waits_on_any_account_its_roles_spend() {
    // A local implementer, with the Claude reviewer every project has by default.
    let (rig, runner) = named("shep", BESIDE_QWEN);
    rig.forge.list_ready(7, false);
    rig.forge.label(7, "agent:coder!");
    assert!(dispatched(&step(&runner).unwrap()));
    assert_eq!(rig.meter.reads(), 1);

    // 20% of Claude's week since the day began, past its allowance of 100 / 7.
    rig.meter.set(Rig::utilization(20, 0));
    rig.ask(&runner, "drop", Some("7"));
    rig.forge.list_ready(8, false);
    rig.forge.label(8, "agent:coder!");
    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Allowance);
    assert!(
        reason.starts_with("today's allowance of 14.3% of the week is spent (20%"),
        "{reason}"
    );
    let pacer = &rig.ask(&runner, "status", None)["pacer"];
    assert_eq!(pacer["claude"]["holding"]["kind"], "allowance");
}

#[test]
fn codex_usage_that_cannot_be_read_holds_until_it_can() {
    let (rig, runner) = named("rotom", ALL_CODEX);
    rig.forge.list_ready(7, false);
    rig.codex_meter.fail(MeterError::Codex(
        "codex refused: 402 Payment Required".into(),
    ));
    let (kind, reason) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Unreadable);
    assert_eq!(
        reason,
        "cannot read Codex usage: codex refused: 402 Payment Required"
    );
    assert_eq!(rig.meter.reads(), 0);

    rig.clock.advance(RECHECK_SECS);
    rig.codex_meter.set(Rig::utilization(0, 0));
    assert!(dispatched(&step(&runner).unwrap()));
}

#[test]
fn a_review_round_waits_on_its_reviewers_account_while_the_worker_works_on() {
    let names = "implementers = [\"codex\"]\nreviewers = [\"claude\"]\n";
    let (rig, runner) = named("chelone", names);
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
    rig.claude.script([Scripted::Text(CLEAN)]);
    for _ in 0..5 {
        rig.verdict(&runner);
    }
    let roles: Vec<Role> = rig.claude.all_calls().iter().map(|c| c.role).collect();
    assert_eq!(roles, [Role::Worker, Role::Reviewer]);
}

#[test]
fn a_local_worker_is_never_paced_and_shows_no_lease() {
    let (rig, runner) = named("rotom", ALL_QWEN);
    // A new item may run on Claude, unlabelled, so its window is read once.
    rig.meter.set(Rig::utilization(0, 1));
    rig.codex_meter.set(Rig::utilization(90, 90));
    rig.forge.list_ready(7, false);
    rig.forge.label(7, "agent:coder!");
    assert!(dispatched(&step(&runner).unwrap()));
    // Both accounts past half their windows: neither is the local model's limit.
    rig.clock.advance(RECHECK_SECS);
    rig.meter.set(Rig::utilization(90, 90));

    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    std::thread::scope(|s| {
        let turn = s.spawn(|| step(&runner).unwrap());
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the turn never began"
        );
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status.get("local_leases"), None);
        assert_eq!(status["pacer"]["claude"]["holding"], json!(null));
        hold.release();
        turn.join().unwrap();
    });
    assert_eq!((rig.meter.reads(), rig.codex_meter.reads()), (1, 0));
    let call = &rig.claude.all_calls()[0];
    assert_eq!(
        (call.harness.clone(), call.model.as_str()),
        (AgentHarness::StandIn, "qwen3-coder")
    );
}

// The gateway's key is read from `CARGO_PKG_NAME`, which `cargo test` sets for its tests.
const BEHIND_PADDOCK: &str = "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
                              gateway: paddock\ncontext: 65536\n---\n";

#[test]
fn status_shows_who_holds_the_paddock_lease_on_a_gateway_model() {
    let server = StandInEndpoint::start([])
        .like_paddock(env!("CARGO_PKG_NAME"), &["coder"])
        .with_paddock_status(
            r#"{"leases":[
                {"id":"L3","client":"batch","model":"other","since":"2026-10-13T08:00:00Z","note":"not ours","revoked":null},
                {"id":"L4","client":"maintainer","model":"coder","since":"2026-10-13T09:30:00Z","note":"shep kelpie lease run: cargo build","revoked":null}
            ],"waiters":[]}"#,
        );
    let rig = Rig::new("rotom");
    rig.write_agent("mine", BEHIND_PADDOCK);
    rig.implementers(&["sonnet-high", "mine"]);
    let settings = std::fs::read_to_string(rig.kelpie_settings_file()).unwrap();
    let gateway = format!(
        "[gateways.paddock]\nurl = \"{}\"\nkey_env = \"CARGO_PKG_NAME\"\n",
        server.host()
    );
    rig.set_kelpie_settings(&format!("{settings}{gateway}"));
    let runner = rig.open().unwrap();

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["local_leases"],
        json!({ "coder": {
            "lease": "L4",
            "client": "maintainer",
            "what": "shep kelpie lease run: cargo build",
            "since": "2026-10-13T09:30:00Z",
        } })
    );
}

// The runner for `named`'s rig, reopened once `change` has been made.
fn reopened(rig: &Rig, runner: Mutex<Runner>, change: impl FnOnce()) -> Mutex<Runner> {
    drop(runner);
    change();
    rig.open().unwrap()
}

#[test]
fn a_work_item_keeps_its_agent_and_runs_its_file_as_it_now_stands() {
    let (rig, runner) = named("rotom", BESIDE_QWEN);
    rig.forge.label(7, "agent:coder!");
    rig.ask(&runner, "add", Some("7"));
    let runner = reopened(&rig, runner, || {
        rig.write_agent(
            "coder",
            &QWEN.replace("qwen3-coder\neffort: low", "next\neffort: high"),
        );
        rig.implementers(&["sonnet-high"]);
    });
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let call = &rig.claude.calls()[0];
    assert_eq!(call.harness, AgentHarness::StandIn);
    assert_eq!(
        (call.model.as_str(), call.effort),
        ("next", crate::settings::Effort::High),
        "the item keeps its agent, unlisted now, and runs the agent's file as it is"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["agent"], "coder");
}

#[test]
fn a_work_item_whose_agent_file_is_gone_fails_its_turn_saying_so() {
    let (rig, runner) = named("rotom", BESIDE_QWEN);
    rig.forge.label(7, "agent:coder!");
    rig.ask(&runner, "add", Some("7"));
    let runner = reopened(&rig, runner, || {
        std::fs::remove_file(rig.paths().agents.join("coder.md")).unwrap();
        rig.implementers(&["sonnet-high"]);
    });
    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("the turn did not fail");
    };
    assert!(
        question.contains(
            "issue #7 runs on agent coder, which has no agent file now: write \
             `agents/coder.md` in kelpie's home again, or drop and add the issue"
        ),
        "{question}"
    );
    assert_eq!(rig.claude.calls(), []);
}

#[test]
fn a_review_round_on_a_local_agent_runs_on_that_agent() {
    let names = "implementers = [\"sonnet-high\"]\nreviewers = [\"coder-review\"]\n";
    let (rig, runner) = named("chelone", names);
    rig.meter.set(Rig::utilization(0, 1));
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.verdict(&runner);
    rig.claude.script([Scripted::Text(CLEAN)]);
    for _ in 0..5 {
        rig.verdict(&runner);
    }
    let calls = rig.claude.all_calls();
    let review = calls.iter().find(|c| c.role == Role::Reviewer).unwrap();
    assert_eq!(review.harness, AgentHarness::StandIn);
    assert_eq!(
        calls[0].harness,
        AgentHarness::ClaudeCode,
        "the worker stays on Claude"
    );
}

#[test]
fn beside_a_local_implementer_listed_first_an_issue_labelled_for_sonnet_is_paced_on_claude() {
    let (rig, runner) = named("rotom", "implementers = [\"coder\", \"sonnet-high\"]\n");
    rig.forge.list_ready(7, false);
    rig.forge.label(7, "agent:sonnet-high!");
    rig.meter.set(Rig::utilization(0, 1));
    assert!(dispatched(&step(&runner).unwrap()));

    rig.clock.advance(RECHECK_SECS);
    rig.meter.set(Rig::utilization(0, 55));
    let (kind, _) = held(step(&runner).unwrap());
    assert_eq!(kind, HoldKind::Window);

    rig.clock.advance(RECHECK_SECS);
    rig.meter.set(Rig::utilization(0, 1));
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let call = &rig.claude.calls()[0];
    assert_eq!(call.model, "claude-sonnet-5-5");
    assert_eq!(call.harness, AgentHarness::ClaudeCode);
}

#[test]
fn a_project_with_no_local_agent_shows_no_leases() {
    let (rig, runner) = named("chelone", "");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status.get("local_leases"), None);
}

#[test]
fn spend_without_dollars_shows_tokens_and_says_it_has_none() {
    let (rig, runner) = named("reactmap", BESIDE_QWEN);
    rig.forge.label(7, "agent:coder!");
    rig.ask(&runner, "add", Some("7"));
    let used = Usage {
        input: 120,
        cache_write: 0,
        cache_write_5m: 0,
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
    assert_eq!(
        (&item["cost_usd"], &item["unpriced_calls"]),
        (&json!(0.0), &json!(1)),
        "the total says it leaves the unpriced call out"
    );
    let tokens = json!({ "input": 120, "cache_write": 0, "cache_read": 2400, "output": 35 });
    assert_eq!(
        item["by_role"]["worker"],
        json!({ "calls": 1, "tokens": tokens, "cost_usd": null, "unpriced_calls": 1 })
    );
    let none = json!({ "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 });
    assert_eq!(
        item["by_role"]["reviewer"],
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

// `/usage` takes seconds, so a step reads it with the runner's lock let go: a trigger sent
// meanwhile is answered.
#[test]
fn a_trigger_is_answered_while_a_new_work_items_usage_is_read() {
    let (rig, runner) = named("shep", "");
    rig.forge.list_ready(7, false);
    let read = rig.meter.hold_next_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    assert!(reply.is_some(), "the trigger waited for the usage read");
    pass.unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], "7", "{status}");
    assert_eq!(rig.meter.reads(), 1);
}

#[test]
fn a_trigger_is_answered_while_a_turns_usage_is_read() {
    let (rig, runner) = named("shep", "");
    rig.forge.list_ready(7, false);
    assert!(dispatched(&step(&runner).unwrap()));
    let read = rig.meter.hold_next_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    assert!(reply.is_some(), "the trigger waited for the usage read");
    pass.unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["working"], json!(["7"]), "{status}");
    assert_eq!(rig.meter.reads(), 2);
}

#[test]
fn a_board_with_nothing_ready_reads_no_usage() {
    let (rig, runner) = named("shep", "");

    step(&runner).unwrap();

    assert_eq!(rig.meter.reads(), 0);
}
