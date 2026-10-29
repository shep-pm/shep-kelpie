//! The batch retro the project manager is asked for, through the runner's
//! stand-ins

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

use crate::ports::{AgentError, Forge, Role, Session, Timestamp, Tools};
use crate::runner::flight::advance;
use crate::runner::retro::journal;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::settings::Harness;
use crate::state::{Finished, ProjectState, StateStore};
use crate::test::{CALL_PATIENCE, Hold, NO_RETRO, Rig, Scripted, Seen, ticket};
use crate::usage::{FILE, read};
use crate::work_item::Seconds;

const REPORT: &str = "## Waits\n\nThe same conflict woke the project manager four times.\n";

// Names the project manager in `rig`'s settings, before its runner is opened
fn name_pm(rig: &Rig) {
    rig.edit_settings(|s| {
        let listed = "[app.dogs.kelpie.agents]\n";
        assert!(s.contains(listed), "the example's agents table moved");
        s.replace(listed, "[app.dogs.kelpie.agents]\npm = \"pm\"\n")
    });
}

// Work items that finished before the rig's clock began, 101 on
fn earlier(count: u64) -> Vec<Finished> {
    (101..101 + count)
        .map(|issue| Finished {
            issue: ticket(issue),
            title: format!("Issue {issue}"),
            pull_request: Some(issue + 60),
            merged: true,
            closed: false,
            at: Timestamp(Rig::EPOCH - 1000 + issue),
            wall: 600,
            seconds: Seconds::default(),
            spend: None,
        })
        .collect()
}

// Writes a state file that already holds `count` finished work items
fn seed(rig: &Rig, count: u64) {
    let mut state = ProjectState::new();
    state.history = earlier(count);
    let file = rig.paths().state;
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    StateStore::new(file).save(&state).unwrap();
}

// A pull request parked on its merge ruling, in a project whose retro runs
// with a named project manager, after `count` other work items finished
fn parked_after(count: u64) -> (Rig, Mutex<Runner>) {
    parked_after_set(count, |_| {})
}

// [`parked_after`], after `setup` changed the rig's settings
fn parked_after_set(count: u64, setup: impl FnOnce(&Rig)) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        name_pm(rig);
        rig.retro_on();
        seed(rig, count);
        setup(rig);
    });
    (rig, runner)
}

// Merges the parked pull request and steps until the work item is gone,
// then once more for what the runner starts after it
fn merge(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    rig.ask(runner, "rule", Some("71 yes"));
    let mut reports = Vec::new();
    for _ in 0..6 {
        rig.clock.advance(CHECKS_SETTLE);
        reports.extend(step(runner).unwrap());
        if matches!(reports.last(), Some(StepReport::Finished { .. })) {
            reports.extend(step(runner).unwrap());
            return reports;
        }
    }
    panic!("the work item never finished: {reports:#?}");
}

// Steps until the retro the drain started has been heard and saved. The call
// runs on a thread of its own from the drain, so a step that comes first finds
// it still in flight and has nothing to wait on.
fn step_until_saved(rig: &Rig, runner: &Mutex<Runner>) {
    for _ in 0..10_000 {
        step(runner).unwrap();
        if !saved(rig).is_empty() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("the retro was never saved");
}

fn retros(rig: &Rig) -> Vec<Seen> {
    let all = rig.claude.all_seen().into_iter();
    all.filter(|s| s.call.tools == Tools::Retro).collect()
}

// The reports kept for the rig's project, oldest name first
fn saved(rig: &Rig) -> Vec<PathBuf> {
    let folder = rig.paths().kelpie_home.join("retro-reports/unverified");
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}

fn ledger(rig: &Rig) -> Vec<Value> {
    let lines = read(&rig.paths().folder.join(FILE)).unwrap();
    lines
        .iter()
        .map(|line| serde_json::to_value(line).unwrap())
        .collect()
}

fn notes(runner: &Mutex<Runner>) -> String {
    runner.lock().unwrap().take_notes().join("\n")
}

#[test]
fn the_tenth_finished_work_item_starts_one_retro_with_the_project_managers_agent() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let seen = retros(&rig);
    let [retro] = seen.as_slice() else {
        panic!("one retro call: {seen:#?}")
    };
    assert_eq!(retro.call.role, Role::Pm);
    assert_eq!(retro.call.tools, Tools::Retro);
    assert!(retro.call.prompt.contains("10 work items of koji"));
    assert!(retro.call.prompt.contains("#101"), "{}", retro.call.prompt);
    assert!(retro.call.prompt.contains("#7"), "{}", retro.call.prompt);
    let denied: Vec<&str> = retro.settings["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for tool in [
        "Bash",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Agent",
    ] {
        assert!(denied.contains(&tool), "{tool}: {denied:?}");
    }
}

#[test]
fn the_retros_skill_command_comes_with_the_plugin_folder_it_is_in() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let [retro] = retros(&rig).try_into().unwrap();
    assert!(
        retro.call.prompt.starts_with("/mattpocock:retro"),
        "{}",
        retro.call.prompt
    );
    assert_eq!(
        retro.call.plugin_dirs,
        [rig.paths().skills.join("mattpocock")]
    );
}

#[test]
fn the_ninth_finished_work_item_starts_none() {
    let (rig, runner) = parked_after(8);
    merge(&rig, &runner);

    assert!(retros(&rig).is_empty());
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
}

#[test]
fn a_retro_is_not_asked_for_again_until_ten_more_finish() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);
    step(&runner).unwrap();
    step(&runner).unwrap();

    assert_eq!(retros(&rig).len(), 1);
}

#[test]
fn a_batch_retro_resumes_no_workers_session() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let seen = rig.claude.all_seen();
    let worker = seen.iter().find(|s| s.call.role == Role::Worker).unwrap();
    let [retro] = retros(&rig).try_into().unwrap();
    assert!(matches!(retro.call.session, Session::New(_)));
    assert_ne!(retro.call.session.id(), worker.call.session.id());
    assert_ne!(retro.call.cwd, worker.call.cwd);
    for s in seen.iter().filter(|s| s.call.role == Role::Worker) {
        assert!(
            !s.call.prompt.starts_with("/mattpocock:retro"),
            "{}",
            s.call.prompt
        );
    }
}

#[test]
fn a_drain_with_finished_work_items_since_the_last_retro_starts_one() {
    let rig = Rig::new("koji");
    name_pm(&rig);
    rig.retro_on();
    seed(&rig, 3);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    rig.ask(&runner, "drain", None);
    step_until_saved(&rig, &runner);

    let seen = retros(&rig);
    let [retro] = seen.as_slice() else {
        panic!("one retro call: {seen:#?}")
    };
    assert_eq!(retro.call.role, Role::Pm);
    assert!(retro.call.prompt.contains("3 work items of koji"));
    assert_eq!(saved(&rig).len(), 1);
}

#[test]
fn a_drain_with_none_finished_since_the_last_retro_starts_none() {
    let rig = Rig::new("koji");
    name_pm(&rig);
    rig.retro_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "drain", None);
    step(&runner).unwrap();

    assert!(retros(&rig).is_empty());
}

#[test]
fn a_drain_after_a_retro_counts_only_the_work_items_finished_since() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);
    rig.ask(&runner, "drain", None);
    step(&runner).unwrap();

    assert_eq!(retros(&rig).len(), 1);
}

#[test]
fn a_project_that_sets_the_retro_to_none_asks_for_none() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        name_pm(rig);
        seed(rig, 9);
    });
    merge(&rig, &runner);

    assert!(retros(&rig).is_empty());
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
}

#[test]
fn a_project_that_leaves_the_retro_out_asks_for_none() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        name_pm(rig);
        rig.retro_left_out();
        seed(rig, 9);
    });
    merge(&rig, &runner);

    assert!(retros(&rig).is_empty());
}

#[test]
fn a_project_with_no_project_manager_asks_for_none() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        rig.retro_on();
        seed(rig, 9);
    });
    merge(&rig, &runner);

    assert!(retros(&rig).is_empty());
}

#[test]
fn the_report_is_saved_unverified_with_the_batchs_issues_in_its_header() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let files = saved(&rig);
    let [file] = files.as_slice() else {
        panic!("one report: {files:?}")
    };
    let name = file.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("koji-batch-") && name.ends_with(".md"),
        "{name}"
    );
    let text = fs::read_to_string(file).unwrap();
    let first = text.lines().next().unwrap();
    assert_eq!(
        first,
        "# Batch retro: koji #101, #102, #103, #104, #105, #106, #107, #108, #109, #7"
    );
    assert!(text.contains(REPORT), "{text}");
    let saved = notes(&runner);
    assert!(saved.contains("saved unverified"), "{saved}");
}

#[test]
fn the_retros_call_lands_in_the_ledger_as_kind_retro_with_role_pm() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let lines: Vec<Value> = ledger(&rig)
        .into_iter()
        .filter(|line| line["kind"] == "retro")
        .collect();
    let [line] = lines.as_slice() else {
        panic!("one retro line: {lines:#?}")
    };
    assert_eq!(line["role"], "pm");
}

#[test]
fn a_retro_that_fails_is_logged_and_skipped_and_the_batch_is_not_asked_again() {
    let (rig, runner) = parked_after(9);
    let broke = AgentError::Failed(Harness::ClaudeCode, "the model is down".into());
    rig.claude.script_for(Role::Pm, [Scripted::Fail(broke)]);
    merge(&rig, &runner);
    step(&runner).unwrap();

    assert_eq!(retros(&rig).len(), 1, "it is not retried");
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    let notes = notes(&runner);
    assert!(notes.contains("the batch retro is skipped"), "{notes}");
    assert!(notes.contains("the model is down"), "{notes}");
}

#[test]
fn a_retro_whose_call_panics_is_skipped_and_does_not_end_the_runner() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Kill]);
    merge(&rig, &runner);

    assert_eq!(retros(&rig).len(), 1);
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    let notes = notes(&runner);
    assert!(notes.contains("panicked"), "{notes}");
}

#[test]
fn a_retro_that_comes_back_empty_saves_nothing() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say("  \n")]);
    merge(&rig, &runner);

    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
    assert!(notes(&runner).contains("came back empty"));
}

#[test]
fn a_retro_cut_short_by_a_stop_is_not_asked_for_again_after_a_restart() {
    let (rig, runner) = parked_after(9);
    let hold = Hold::default();
    rig.claude
        .script_for(Role::Pm, [Scripted::Hold(hold.clone())]);
    rig.ask(&runner, "rule", Some("71 yes"));
    std::thread::scope(|scope| {
        let looking = scope.spawn(|| {
            let mut reports = Vec::new();
            for _ in 0..6 {
                rig.clock.advance(CHECKS_SETTLE);
                reports.extend(step(&runner).unwrap());
            }
            reports
        });
        let began = hold.entered(CALL_PATIENCE);
        rig.claude.stop();
        assert!(began, "the retro never began");
        looking.join().unwrap();
    });
    drop(runner);

    let runner = rig.open().unwrap();
    step(&runner).unwrap();
    step(&runner).unwrap();
    assert_eq!(retros(&rig).len(), 1);
    assert_eq!(saved(&rig), Vec::<PathBuf>::new());
}

#[test]
fn a_retro_skill_that_cannot_load_runs_kelpies_own_prompt() {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        name_pm(rig);
        seed(rig, 9);
        rig.edit_settings(|s| {
            let s = s.replace(NO_RETRO, "");
            format!("{s}\n[app.dogs.kelpie.skills]\nretro = {{ kind = \"path\", path = \"/no/such/skill\" }}\n")
        });
    });
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let seen = retros(&rig);
    let [retro] = seen.as_slice() else {
        panic!("one retro call: {seen:#?}")
    };
    assert!(
        retro.call.prompt.starts_with("10 work items of koji"),
        "{}",
        retro.call.prompt
    );
    assert_eq!(saved(&rig).len(), 1);
}

#[test]
fn a_retro_in_flight_holds_no_slot_so_the_next_issue_opens() {
    let (rig, runner) = parked_after(9);
    let hold = Hold::default();
    rig.claude
        .script_for(Role::Pm, [Scripted::Hold(hold.clone())]);
    rig.ask(&runner, "rule", Some("71 yes"));
    std::thread::scope(|scope| {
        let looking = scope.spawn(|| {
            let mut reports = Vec::new();
            for _ in 0..6 {
                rig.clock.advance(CHECKS_SETTLE);
                reports.extend(step(&runner).unwrap());
            }
            reports
        });
        let began = hold.entered(CALL_PATIENCE);
        rig.forge.list_ready(8, false);
        rig.next_look();
        let passes: Vec<String> = (0..4)
            .filter(|_| began)
            .map(|_| format!("{:?}", advance(&runner).unwrap()))
            .collect();
        hold.release();
        looking.join().unwrap();
        assert!(began, "the retro never began");
        assert!(
            passes.iter().any(|p| p.contains("issue: Github(8)")),
            "{passes:#?}"
        );
    });
}

#[test]
fn a_drain_waits_on_a_retro_already_running_for_its_own_ceiling() {
    let (rig, runner) = parked_after_set(9, |rig| {
        rig.edit_settings(|s| s.replace("turn_timeout = 60", "turn_timeout = 5"));
    });
    let hold = Hold::default();
    rig.claude
        .script_for(Role::Pm, [Scripted::Hold(hold.clone())]);
    rig.ask(&runner, "rule", Some("71 yes"));
    std::thread::scope(|scope| {
        let looking = scope.spawn(|| {
            let mut reports = Vec::new();
            for _ in 0..6 {
                rig.clock.advance(CHECKS_SETTLE);
                reports.extend(step(&runner).unwrap());
            }
            reports
        });
        let began = hold.entered(CALL_PATIENCE);
        let status = rig.ask(&runner, "drain", None);
        hold.release();
        looking.join().unwrap();
        assert!(began, "the retro never began");
        assert_eq!(status["draining"]["ceiling"], super::CEILING);
    });
}

#[test]
fn the_friction_a_worker_reported_in_its_pull_request_reaches_the_retro() {
    let (rig, runner) = parked_after(9);
    let file = rig.paths().build(ticket(7)).join("pull-request.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let body = "feat: the thing\n\n## Friction\n\nThe lockfile fought me twice.\n\nResolves #7\n";
    fs::write(&file, body).unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let seen = retros(&rig);
    let [retro] = seen.as_slice() else {
        panic!("one retro call: {seen:#?}")
    };
    let prompt = &retro.call.prompt;
    assert!(
        prompt.contains("- Friction the worker reported: The lockfile fought me twice."),
        "{prompt}"
    );
    assert!(!prompt.contains("Resolves #7"), "{prompt}");
}

#[test]
fn a_pull_request_file_that_does_not_parse_falls_back_to_the_forges_body() {
    let (rig, runner) = parked_after(9);
    let file = rig.paths().build(ticket(7)).join("pull-request.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "\n\n").unwrap();
    let (remote, number) = {
        let runner = runner.lock().unwrap();
        let item = runner.current().expect("a parked work item");
        (
            runner.remote.clone(),
            item.pull_request.expect("a pull request"),
        )
    };
    let body = "feat: the thing\n\n## Friction\n\nThe forge kept this.\n";
    rig.forge
        .edit_pull_request(&remote, number, "feat: the thing", body)
        .unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let seen = retros(&rig);
    let [retro] = seen.as_slice() else {
        panic!("one retro call: {seen:#?}")
    };
    let prompt = &retro.call.prompt;
    assert!(
        prompt.contains("- Friction the worker reported: The forge kept this."),
        "{prompt}"
    );
}

#[test]
fn a_journal_that_cannot_be_marked_starts_no_retro_and_is_not_asked_again() {
    let (rig, runner) = parked_after(9);
    let journal = rig.paths().journal;
    let _ = fs::remove_file(&journal);
    fs::create_dir_all(&journal).unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);
    step(&runner).unwrap();

    assert!(retros(&rig).is_empty());
    assert!(notes(&runner).contains("no retro of"));
}

#[test]
fn a_drain_that_starts_during_a_retro_asks_for_the_work_items_finished_meanwhile() {
    let (rig, runner) = parked_after(9);
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Hold(hold.clone()), Scripted::Say(REPORT)],
    );
    rig.ask(&runner, "rule", Some("71 yes"));
    std::thread::scope(|scope| {
        let looking = scope.spawn(|| {
            for _ in 0..6 {
                rig.clock.advance(CHECKS_SETTLE);
                step(&runner).unwrap();
            }
        });
        let began = hold.entered(CALL_PATIENCE);
        assert!(began, "the retro never began");
        rig.clock.advance(60);
        {
            let mut runner = runner.lock().unwrap();
            let mut meanwhile = earlier(1).remove(0);
            meanwhile.issue = ticket(300);
            meanwhile.at = runner.ports.clock.now();
            runner.state.history.push(meanwhile);
        }
        rig.ask(&runner, "drain", None);
        hold.release();
        looking.join().unwrap();
    });
    for _ in 0..3 {
        rig.clock.advance(CHECKS_SETTLE);
        step(&runner).unwrap();
    }

    let seen = retros(&rig);
    assert_eq!(seen.len(), 2, "{seen:#?}");
    assert!(seen[1].call.prompt.contains("1 work items of koji"));
}

#[test]
fn the_drain_starts_its_retro_itself_so_its_status_lists_it() {
    let rig = Rig::new("koji");
    name_pm(&rig);
    rig.retro_on();
    seed(&rig, 3);
    let runner = rig.open().unwrap();
    let hold = Hold::default();
    rig.claude
        .script_for(Role::Pm, [Scripted::Hold(hold.clone())]);
    let status = rig.ask(&runner, "drain", None);
    let began = hold.entered(CALL_PATIENCE);
    hold.release();

    assert!(began, "the retro never began");
    assert_eq!(
        status["draining"]["calls"],
        serde_json::json!([{ "role": "retro" }])
    );
}

#[test]
fn a_history_of_many_old_work_items_gets_a_retro_of_the_latest_ten() {
    let rig = Rig::new("koji");
    name_pm(&rig);
    rig.retro_on();
    seed(&rig, 25);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    step(&runner).unwrap();

    let seen = retros(&rig);
    let [retro] = seen.as_slice() else {
        panic!("one retro call: {seen:#?}")
    };
    let prompt = &retro.call.prompt;
    assert!(prompt.contains("10 work items of koji"), "{prompt}");
    assert!(prompt.contains("#125"), "{prompt}");
    assert!(!prompt.contains("#115,"), "{prompt}");
    assert!(prompt.contains("#116,"), "{prompt}");
}

const GAP: &str = "sandbox cannot bind a socket";

// Keeps an earlier retro's report in the unverified folder, as the maintainer
// has not yet consolidated it
fn keep_report(rig: &Rig, text: &str) {
    let folder = rig.paths().retro_reports;
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("koji-batch-20261010T000000Z.md"), text).unwrap();
}

// Work item `issue` told the journal what slowed it down
fn friction(rig: &Rig, issue: u64, text: &str) {
    let journal = rig.paths().journal;
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let entry = journal::Entry::Friction {
        issue: ticket(issue),
        text: text.to_owned(),
    };
    journal::append(&journal, Timestamp(Rig::EPOCH - 1400), entry).unwrap();
}

// Work item `issue`'s round `round`, read by `reviewer` with `findings`
fn round(rig: &Rig, issue: u64, round: u32, reviewer: &str, findings: usize) {
    written(
        rig,
        journal::Entry::Round {
            issue: ticket(issue),
            round,
            reviewer: reviewer.to_owned(),
            findings,
        },
    );
}

// Work item `issue`'s round `round`, with `reviewer` passed over for `reason`
fn passed_over(rig: &Rig, issue: u64, round: u32, reviewer: &str, reason: &str) {
    written(
        rig,
        journal::Entry::ReviewerSkipped {
            issue: ticket(issue),
            round,
            reviewer: reviewer.to_owned(),
            reason: reason.to_owned(),
        },
    );
}

// Adds `entry` to the project's journal, in the seeded work items' own span
fn written(rig: &Rig, entry: journal::Entry) {
    let journal = rig.paths().journal;
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    journal::append(&journal, Timestamp(Rig::EPOCH - 1400), entry).unwrap();
}

#[test]
fn the_prompt_names_the_build_the_batch_ran_under() {
    let (rig, runner) = parked_after(9);
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let [retro] = retros(&rig).try_into().unwrap();
    let build = format!("build for this batch: {}", env!("CARGO_PKG_VERSION"));
    assert!(retro.call.prompt.contains(&build), "{}", retro.call.prompt);
    assert!(
        retro
            .call
            .prompt
            .contains("No earlier finding of yours is still open."),
        "{}",
        retro.call.prompt
    );
}

#[test]
fn an_open_known_gap_is_listed_and_one_that_recurs_is_counted_not_described_again() {
    let (rig, runner) = parked_after_set(9, |rig| {
        keep_report(
            rig,
            &format!("# Batch retro\n\n## New findings\n\n- {GAP}: tests need a port\n"),
        );
        friction(rig, 101, "The Sandbox cannot bind a socket again");
        friction(
            rig,
            102,
            "tests that bind a socket fail, sandbox cannot bind a socket",
        );
    });
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let [retro] = retros(&rig).try_into().unwrap();
    let prompt = &retro.call.prompt;
    assert!(
        prompt.contains(&format!("(the known gaps):\n- {GAP}\n")),
        "{prompt}"
    );
    assert!(prompt.contains("reporting new items only"), "{prompt}");
    assert!(prompt.contains("Report none of them again"), "{prompt}");
    // Counted by the digest, by theme, with the earlier report's description left out
    assert!(
        prompt.contains(&format!(
            "named again in this batch:\n  - {GAP}: #101, #102\n"
        )),
        "{prompt}"
    );
    assert!(!prompt.contains("tests need a port"), "{prompt}");
}

#[test]
fn the_digest_lists_a_clean_round_and_a_reviewer_passed_over_numbered() {
    let (rig, runner) = parked_after_set(9, |rig| {
        passed_over(
            rig,
            101,
            1,
            "codex",
            "its window opens more than an hour on",
        );
        round(rig, 101, 2, "claude", 0);
    });
    rig.claude.script_for(Role::Pm, [Scripted::Say(REPORT)]);
    merge(&rig, &runner);

    let [retro] = retros(&rig).try_into().unwrap();
    let prompt = &retro.call.prompt;
    for expected in [
        "- Review rounds: 1 read, 1 passed over, the latest numbered 2",
        "  - #101 round 1 by codex: passed over, its window opens more than an hour on",
        "  - #101 round 2 by claude: 0 findings",
    ] {
        assert!(prompt.contains(expected), "{expected}: {prompt}");
    }
}
