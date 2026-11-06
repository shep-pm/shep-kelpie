//! The rulings the maintainer puts to the project manager from their thread:
//! what is posted, what is applied, and what is refused

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::held_line;
use super::super::said::{SaidCall, SaidHow};
use super::{NOTHING, name_pm, pm_seen, step_until};
use crate::ports::{AgentError, CLEAN, Checks, Cost, Role};
use crate::runner::flight::advance;
use crate::runner::retro::journal::{self, Entry};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::state::{AgentFit, AgentMeasure, RulingKind, Stuck};
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::work_item::{Phase, Priced, Turn};

/// A project whose worker is parked on merge ruling 71, posted to the
/// maintainer, who may write; with a project manager where `pm` says
pub(super) fn merge_put(project: &str, pm: bool) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_set(project, |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        if pm {
            name_pm(rig);
        }
    });
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    rig.forge.threads.grant_write("octocat");
    assert!(card(&rig).is_some(), "{:?}", comments(&rig));
    rig.clock.advance(5);
    (rig, runner)
}

/// The project manager's vote on ruling 71 for `choice`, saying `why`
pub(super) fn vote(choice: char, why: &str) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": 71, \"choice\": \"{choice}\", \"why\": \
         \"{why}\", \"design\": false}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

/// The comments posted on the threads, oldest first
pub(super) fn comments(rig: &Rig) -> Vec<String> {
    rig.github.comments().into_iter().map(|(_, b)| b).collect()
}

/// The ruling's own comment, the card
fn card(rig: &Rig) -> Option<String> {
    (comments(rig).into_iter()).find(|body| body.starts_with("### Merge this pull request"))
}

/// Writes `reply` as the maintainer, and steps until a comment `wanted`
/// matches is posted, which it returns
pub(super) fn replied(
    rig: &Rig,
    runner: &Mutex<Runner>,
    reply: &str,
    wanted: impl Fn(&str) -> bool,
) -> String {
    rig.writes_on(71, "octocat", reply);
    rig.next_look();
    posted(rig, runner, wanted)
}

/// Steps until a comment `wanted` matches is posted, which it returns
pub(super) fn posted(rig: &Rig, runner: &Mutex<Runner>, wanted: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + CALL_PATIENCE;
    loop {
        if let Some(found) = comments(rig).into_iter().find(|c| wanted(c)) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "never posted: {:?}",
            comments(rig)
        );
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

/// Advances the runner, without waiting on a call in flight, until `done`
pub(super) fn advanced_until(runner: &Mutex<Runner>, done: impl Fn() -> bool) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "the runner never got there");
        advance(runner).unwrap();
        std::thread::yield_now();
    }
}

/// Whether ruling 71 still waits
pub(super) fn pending(rig: &Rig) -> bool {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["rulings"].as_array().is_some_and(|r| !r.is_empty())
}

/// How many of the project manager's calls carried `lead`
fn woken(rig: &Rig, lead: &str) -> usize {
    (pm_seen(rig).iter())
        .filter(|s| s.call.prompt.contains(lead))
        .count()
}

const RECOMMEND: &str = "asks which option you recommend for ruling 71";

#[test]
fn a_ruling_left_to_the_maintainer_wakes_no_project_manager_until_they_ask() {
    let (rig, runner) = merge_put("asked-quiet", true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(vote('A', "PM-WHY"))]);

    for _ in 0..3 {
        step(&runner).unwrap();
    }

    assert_eq!(pm_seen(&rig).len(), 0);
    assert!(!card(&rig).unwrap().contains("**Project manager"));
}

#[test]
fn pm_recommend_posts_its_pick_as_an_existing_answer_and_the_ruling_stays_the_maintainers() {
    let (rig, runner) = merge_put("asked-recommend", true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(vote('A', "PM-WHY"))]);

    let said = replied(&rig, &runner, "71 pm recommend", |c| {
        c.contains("**Project manager: A**")
    });

    assert!(
        said.starts_with("@octocat **Project manager: A**. PM-WHY"),
        "{said}"
    );
    assert!(said.contains("It would answer `71 yes` (Merge)."), "{said}");
    assert!(said.contains("Ruling #71 is still yours"), "{said}");
    assert!(pending(&rig), "the maintainer still answers");
    assert_eq!(rig.github.reactions().len(), 1, "kelpie took the request");
    assert_eq!(woken(&rig, RECOMMEND), 1);
}

#[test]
fn its_recommendation_is_inert_text() {
    let (rig, runner) = merge_put("asked-inert", true);
    let why = "ask @mallory, see https://evil.example/x";
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(vote('A', why))]);

    let said = replied(&rig, &runner, "71 pm recommend", |c| {
        c.contains("**Project manager: A**")
    });

    assert!(!said.contains("@mallory"), "{said}");
    assert!(!said.contains("https://"), "{said}");
    assert!(said.contains("@\u{200b}mallory"), "{said}");
}

#[test]
fn with_no_project_manager_a_pm_reply_is_refused_and_says_so() {
    let (rig, runner) = merge_put("asked-none", false);

    let said = replied(&rig, &runner, "71 pm recommend", |c| {
        c.contains("was not put to the project manager")
    });

    assert!(
        said.contains("this project names no project manager"),
        "{said}"
    );
    assert!(rig.github.reactions().is_empty());
}

#[test]
fn a_pm_reply_it_does_not_know_says_what_it_takes() {
    let (rig, runner) = merge_put("asked-unknown", true);

    let said = replied(&rig, &runner, "71 pm whatever", |c| {
        c.contains("was not answered")
    });

    assert!(said.contains("`pm` takes `recommend`"), "{said}");
    assert_eq!(pm_seen(&rig).len(), 0);
}

#[test]
fn a_wake_that_fails_on_a_request_says_so_and_the_ruling_stays_the_maintainers() {
    let (rig, runner) = merge_put("asked-failed", true);
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    rig.claude.script_for(Role::Pm, [Scripted::Fail(failed)]);

    let said = replied(&rig, &runner, "71 pm recommend", |c| {
        c.contains("did not answer")
    });

    assert_eq!(
        said,
        "@octocat The project manager did not answer what you asked of ruling #71, which is \
         still yours."
    );
    assert!(pending(&rig));
}

#[test]
fn a_second_request_while_its_wake_runs_is_answered_once_and_not_queued() {
    let (rig, runner) = merge_put("asked-twice", true);
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [Scripted::HoldThenSay(hold.clone(), vote('A', "PM-WHY"))],
    );
    rig.writes_on(71, "octocat", "71 pm recommend");
    rig.next_look();
    advanced_until(&runner, || hold.entered(Duration::ZERO));

    rig.clock.advance(5);
    rig.writes_on(71, "octocat", "71 pm recommend");
    rig.next_look();
    let again = || {
        comments(&rig)
            .into_iter()
            .find(|c| c.contains("already on it"))
    };
    advanced_until(&runner, || again().is_some());
    hold.release();
    let said = again().unwrap();
    posted(&rig, &runner, |c| c.contains("**Project manager: A**"));

    assert!(
        said.contains("Ruling #71 was not put to the project manager"),
        "{said}"
    );
    assert_eq!(woken(&rig, RECOMMEND), 1);
    let already = comments(&rig)
        .iter()
        .filter(|c| c.contains("already on it"))
        .count();
    assert_eq!(already, 1);
}

// Other approaches, asked for with `pm options` and taken with `pm <n>`

/// The project manager's two approaches to ruling 71, the second as `second`
pub(super) fn approaches(second: &str) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": 71, \"approaches\": [\"Split the parser \
         out first\", \"{second}\"]}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

/// [`merge_put`], with the project manager's approaches posted
fn with_approaches(project: &str, second: &str) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner) = merge_put(project, true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(approaches(second))]);
    let said = replied(&rig, &runner, "71 pm options", |c| {
        c.contains("other approaches to ruling #71")
    });
    rig.clock.advance(5);
    (rig, runner, said)
}

/// Steps until the worker is sent a prompt holding `text`
pub(super) fn worker_sent(rig: &Rig, runner: &Mutex<Runner>, text: &str) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while !rig.claude.calls().iter().any(|c| c.prompt.contains(text)) {
        assert!(
            Instant::now() < deadline,
            "the worker was never sent {text:?}"
        );
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

#[test]
fn pm_options_posts_numbered_approaches_and_pm_n_sends_the_worker_one_as_a_no_note() {
    let (rig, runner, said) = with_approaches("asked-options", "Keep the old flag one release");

    assert!(
        said.contains("1. Split the parser out first\n2. Keep the old flag one release"),
        "{said}"
    );
    assert!(
        said.contains("Reply `71 pm <n>` to take one as your `71 no <note>`."),
        "{said}"
    );
    assert!(pending(&rig), "the maintainer still picks");

    rig.claude.script([Scripted::Push("flag.txt", "flag\n")]);
    rig.writes_on(71, "octocat", "71 pm 2");
    rig.next_look();
    worker_sent(
        &rig,
        &runner,
        "with this note:\n\nKeep the old flag one release",
    );

    assert!(!pending(&rig), "the no answered the ruling");
}

#[test]
fn the_approaches_are_posted_inert_and_reach_the_worker_as_written() {
    let raw = "Ask @mallory to read https://evil.example/x";
    let (rig, runner, said) = with_approaches("asked-options-inert", raw);

    assert!(!said.contains("@mallory"), "{said}");
    assert!(!said.contains("https://"), "{said}");
    assert!(said.contains("@\u{200b}mallory"), "{said}");

    rig.claude.script([Scripted::Push("flag.txt", "flag\n")]);
    rig.writes_on(71, "octocat", "71 pm 2");
    rig.next_look();
    worker_sent(&rig, &runner, raw);
}

#[test]
fn the_approaches_outlive_a_restart() {
    let (rig, runner, _) = with_approaches("asked-options-restart", "Keep the old flag");
    drop(runner);
    let runner = rig.open().unwrap();

    rig.claude.script([Scripted::Push("flag.txt", "flag\n")]);
    rig.writes_on(71, "octocat", "71 pm 1");
    rig.next_look();
    worker_sent(
        &rig,
        &runner,
        "with this note:\n\nSplit the parser out first",
    );
}

#[test]
fn pm_n_with_no_approaches_or_past_the_last_is_refused_and_says_why() {
    let (rig, runner) = merge_put("asked-take-none", true);
    let said = replied(&rig, &runner, "71 pm 1", |c| c.contains("was not answered"));
    assert!(
        said.contains("the project manager gave it no approaches: reply `71 pm options` for some"),
        "{said}"
    );

    let (rig, runner, _) = with_approaches("asked-take-past", "Keep the old flag");
    let said = replied(&rig, &runner, "71 pm 3", |c| c.contains("was not answered"));
    assert!(
        said.contains("it has 2 approaches, so `pm 3` names none"),
        "{said}"
    );
    assert!(pending(&rig));
}

// Free text under a ruling, which a project with a project manager reads as
// words to it

/// Steps until the project manager is woken, and returns its first prompt
pub(super) fn woken_prompt(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let deadline = Instant::now() + CALL_PATIENCE;
    loop {
        if let Some(seen) = pm_seen(rig).first() {
            return seen.call.prompt.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the project manager was never woken"
        );
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

/// Steps through every wait of the board a few times
pub(super) fn looked(rig: &Rig, runner: &Mutex<Runner>) {
    for _ in 0..3 {
        rig.next_look();
        step(runner).unwrap();
    }
}

/// How many turns the worker has been sent
pub(super) fn turns(rig: &Rig) -> usize {
    rig.claude.calls().len()
}

/// The comments that tell the maintainer a reply was not taken
pub(super) fn refusals(rig: &Rig) -> Vec<String> {
    (comments(rig).into_iter())
        .filter(|c| c.contains("was not answered") || c.contains("start your reply"))
        .collect()
}

/// Makes ruling 71 a failed-closed one, which takes a yes or a no
pub(super) fn made_stuck(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap();
    let mut next = runner.state.clone();
    next.rulings[0].kind = RulingKind::from(Stuck::Closed);
    runner.save(next).unwrap();
}

// The project manager's calls on the maintainer's held words, through `said`

/// The project manager's answer calling `call` on the maintainer's words on
/// #71, with `note` for a send-back and `why`
fn calling(call: &str, note: &str, why: &str) -> &'static str {
    let said = json!({"thread": 71, "call": call, "note": note, "why": why});
    let text = json!({"pick": null, "said": [said]}).to_string();
    Box::leak(text.into_boxed_str())
}

/// Steps until the project manager's wake ends, and returns what it did and
/// what it dropped
fn wake_ended(runner: &Mutex<Runner>) -> (Vec<String>, Vec<String>) {
    let ended = |r: &StepReport| matches!(r, StepReport::PmAnswered { .. });
    match step_until(runner, ended) {
        StepReport::PmAnswered { acted, dropped, .. } => (acted, dropped),
        other => panic!("{other:?}"),
    }
}

/// Writes `words` on pull request 71 as the maintainer, and steps until the
/// project manager's wake on them ends
fn said_words(rig: &Rig, runner: &Mutex<Runner>, words: &str) -> (Vec<String>, Vec<String>) {
    rig.writes_on(71, "octocat", words);
    rig.next_look();
    wake_ended(runner)
}

/// Makes the merge ruling 71 held by review bot threads `ids`
fn held_by(runner: &Mutex<Runner>, ids: &[&str]) {
    let mut runner = runner.lock().unwrap();
    let mut next = runner.state.clone();
    let RulingKind::Merge { held_by, .. } = &mut next.rulings[0].kind else {
        panic!("not a merge ruling");
    };
    *held_by = ids.iter().map(|id| (*id).to_owned()).collect();
    runner.save(next).unwrap();
}

/// The comments that name a call of the project manager that was not
/// carried out
fn not_carried_out(rig: &Rig) -> Vec<String> {
    (comments(rig).into_iter())
        .filter(|c| c.contains("was not carried out"))
        .collect()
}

/// The journal's wakes: what each acted on and dropped
fn journaled(rig: &Rig) -> Vec<(Vec<String>, Vec<String>)> {
    (journal::read(&rig.paths().journal).into_iter())
        .filter_map(|stamped| match stamped.entry {
            Entry::Wake { acted, dropped, .. } => Some((acted, dropped)),
            _ => None,
        })
        .collect()
}

#[test]
fn send_back_under_a_merge_ruling_sends_the_worker_the_note_and_posts_the_answer() {
    let (rig, runner) = merge_put("said-send-back", true);
    let call = calling("send-back", "Split the parser out first", "It is too large");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);
    rig.claude.script([Scripted::Push("flag.txt", "flag\n")]);

    rig.writes_on(71, "octocat", "This is too large, please split it.");
    rig.next_look();
    worker_sent(&rig, &runner, "with this note:\n\n");
    let said = posted(&rig, &runner, |c| {
        c.starts_with("**Answered by the project manager")
    });

    let sent = rig.claude.calls();
    let sent = sent.iter().find(|c| c.prompt.contains("with this note:"));
    assert!(
        sent.unwrap().prompt.contains("Split the parser out first"),
        "{sent:?}"
    );
    assert!(
        said.contains("Reply `71 no <note>` to reopen it."),
        "{said}"
    );
    assert!(!pending(&rig), "the no answered the ruling");
    let line = std::fs::read_to_string(rig.paths().rulings).unwrap();
    assert!(line.contains("Split the parser out first"), "{line}");
}

#[test]
fn a_no_note_reopens_a_ruling_sent_back_on_the_maintainers_words() {
    let (rig, runner) = merge_put("said-reopen", true);
    let call = calling("send-back", "Split the parser out first", "It is too large");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);
    rig.claude.script([
        Scripted::Push("flag.txt", "flag\n"),
        Scripted::Push("again.txt", "again\n"),
    ]);
    rig.writes_on(71, "octocat", "This is too large, please split it.");
    rig.next_look();
    worker_sent(&rig, &runner, "Split the parser out first");
    posted(&rig, &runner, |c| {
        c.starts_with("**Answered by the project manager")
    });
    rig.clock.advance(5);
    looked(&rig, &runner);

    rig.writes_on(71, "octocat", "71 no keep the flag instead");
    rig.next_look();

    worker_sent(&rig, &runner, "keep the flag instead");
}

/// Every kind whose `no` makes no worker turn, as `decide` has it
fn no_worker_turn() -> Vec<(&'static str, RulingKind)> {
    let phase = Phase::Implement;
    let stuck = |s: Stuck| RulingKind::from(s);
    vec![
        ("turn-timeout", stuck(Stuck::TurnTimeout { phase: None })),
        (
            "turn-failed",
            stuck(Stuck::TurnFailed {
                why: "exit 1".into(),
                phase: phase.clone(),
                retry: Turn::Due,
                restore: Vec::new(),
                setup: false,
            }),
        ),
        (
            "push-refused",
            stuck(Stuck::PushRefused { why: "no".into() }),
        ),
        (
            "agent-files",
            RulingKind::AgentFiles {
                head: "abc".into(),
                files: vec!["CLAUDE.md".into()],
                phase: phase.clone(),
            },
        ),
        (
            "check-queued",
            stuck(Stuck::CheckQueued {
                head: "abc".into(),
                checks: Vec::new(),
                run: None,
                url: String::new(),
                review: None,
            }),
        ),
        (
            "no-checks",
            stuck(Stuck::NoChecks {
                head: "abc".into(),
                review: None,
            }),
        ),
        (
            "over-budget",
            stuck(Stuck::OverBudget {
                phase,
                spent: Cost(900),
                cap: Cost(800),
                by_role: Priced::default(),
            }),
        ),
        (
            "adopt-failed",
            stuck(Stuck::AdoptFailed { why: "no".into() }),
        ),
        (
            "follow-up",
            RulingKind::FollowUp {
                findings: Vec::new(),
                refused: None,
            },
        ),
        (
            "agent-fit",
            RulingKind::AgentFit(AgentFit {
                work_kind: "fix".into(),
                worse: "a".into(),
                better: "b".into(),
                measure: AgentMeasure::MergeRate {
                    worse: 1,
                    better: 2,
                },
                suggested_kinds: Vec::new(),
            }),
        ),
    ]
}

#[test]
fn send_back_on_a_kind_whose_no_makes_no_worker_turn_is_named_and_the_ruling_waits() {
    let (rig, runner) = merge_put("said-refused", true);
    let words = held_line(71, 9001, Some(ticket(71)), "go on");
    let call = SaidCall {
        thread: 71,
        call: SaidHow::SendBack,
        note: "Split it".into(),
        why: "It is large".into(),
    };
    let before = turns(&rig);
    for (name, kind) in no_worker_turn() {
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.rulings[0].kind = kind.clone();
        runner.save(next).unwrap();

        let (acted, dropped) =
            runner.pm_said(std::slice::from_ref(&words), std::slice::from_ref(&call));

        assert_eq!(acted, Vec::<String>::new(), "{name}");
        assert_eq!(dropped.len(), 1, "{name}: {dropped:?}");
        assert!(
            dropped[0].contains("a send-back is refused here"),
            "{name}: {dropped:?}"
        );
        assert!(dropped[0].contains("It is large"), "{name}: {dropped:?}");
        assert_eq!(runner.state.rulings[0].kind, kind, "{name}: it waits");
    }
    let said = not_carried_out(&rig);
    assert_eq!(said.len(), 10, "{said:?}");
    assert!(said[0].contains("It is still yours."), "{}", said[0]);
    assert_eq!(turns(&rig), before, "no worker turn started");
}

#[test]
fn park_under_a_ruling_leaves_it_waiting_and_says_why() {
    let (rig, runner) = merge_put("said-park", true);
    let call = calling("park", "", "It asks for a design choice");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);
    let before = turns(&rig);

    let (acted, dropped) = said_words(&rig, &runner, "Should this be two changes?");
    let said = posted(&rig, &runner, |c| c.contains("**Project manager: parked**"));

    assert_eq!(dropped, Vec::<String>::new());
    assert!(acted[0].contains("parked on ruling"), "{acted:?}");
    assert!(said.starts_with("@octocat "), "{said}");
    assert!(said.contains("Ruling #71 waits for you."), "{said}");
    assert!(said.contains("It asks for a design choice"), "{said}");
    assert!(pending(&rig));
    assert_eq!(turns(&rig), before);
}

#[test]
fn settle_accepts_the_held_threads_and_the_gate_looks_again() {
    let (rig, runner) = merge_put("said-settle", true);
    held_by(&runner, &["thread-1", "thread-2"]);
    let call = calling("settle", "", "The bot's threads are nits");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);

    let (acted, dropped) = said_words(&rig, &runner, "Those bot threads are only nits.");

    assert_eq!(dropped, Vec::<String>::new());
    assert!(acted[0].contains("settled the threads"), "{acted:?}");
    let runner = runner.lock().unwrap();
    let item = &runner.state.work_items[0];
    assert_eq!(item.threads_accepted, ["thread-1", "thread-2"]);
    assert!(matches!(item.phase, Phase::Ci { .. }), "{:?}", item.phase);
    assert!(runner.state.rulings.is_empty(), "the ruling was withdrawn");
    let said = comments(&rig).into_iter().find(|c| c.contains("withdrawn"));
    let said = said.unwrap_or_default();
    assert!(said.contains("The bot's threads are nits"), "{said}");
}

#[test]
fn settle_with_nothing_held_is_named_and_the_ruling_waits() {
    let (rig, runner) = merge_put("said-settle-none", true);
    let call = calling("settle", "", "They are nits");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);

    let (acted, dropped) = said_words(&rig, &runner, "Those bot threads are only nits.");

    assert_eq!(acted, Vec::<String>::new());
    assert!(
        dropped[0].contains("no review bot thread holds it"),
        "{dropped:?}"
    );
    let said = not_carried_out(&rig);
    assert!(said[0].contains("It is still yours."), "{said:?}");
    assert!(pending(&rig));
}

/// A project with a project manager whose pull request 71 is open and whose
/// CI is pending, with no ruling raised, the maintainer able to write; also
/// the head
fn bare_put(project: &str) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.with_app();
    rig.maintainer("octocat");
    name_pm(&rig);
    rig.ci_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.forge.threads.grant_write("octocat");
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));
    (rig, runner, head)
}

/// Writes `words` as the maintainer on [`bare_put`]'s pull request, which
/// the review round reads, and steps until the project manager's wake on
/// them ends
fn said_bare(rig: &Rig, runner: &Mutex<Runner>, words: &str) -> (Vec<String>, Vec<String>) {
    rig.writes_on(71, "octocat", words);
    rig.next_look();
    wake_ended(runner)
}

/// Passes CI on `head` and lets the checks settle, so the gate reads the
/// comments again
fn passed(rig: &Rig, head: &str) {
    rig.forge.set_checks(head, Checks::Passed);
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    rig.next_look();
}

#[test]
fn send_back_with_no_ruling_reaches_the_worker_at_the_next_comment_read() {
    let (rig, runner, head) = bare_put("said-bare-send");
    let call = calling("send-back", "Cache it in a map", "The words ask for it");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);
    rig.claude.script([Scripted::Push("map.txt", "map\n")]);

    let (acted, dropped) = said_bare(&rig, &runner, "Why is the cache a vector?");
    passed(&rig, &head);
    worker_sent(&rig, &runner, "Cache it in a map");

    assert_eq!(dropped, Vec::<String>::new());
    assert!(acted[0].contains("send-back on #71"), "{acted:?}");
    let sent = rig.claude.calls();
    let sent = sent.iter().find(|c| c.prompt.contains("Cache it in a map"));
    let sent = &sent.unwrap().prompt;
    assert!(sent.contains("Why is the cache a vector?"), "{sent}");
    assert!(runner.lock().unwrap().pm_released().is_empty());
}

#[test]
fn send_back_with_no_ruling_and_no_note_is_named_and_the_comment_stays_held() {
    let (rig, runner, _head) = bare_put("said-bare-no-note");
    let call = calling("send-back", "", "The words ask for it");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);

    let (acted, dropped) = said_bare(&rig, &runner, "Why is the cache a vector?");

    assert_eq!(acted, Vec::<String>::new());
    assert!(dropped[0].contains("no note for the worker"), "{dropped:?}");
    assert!(runner.lock().unwrap().pm_released().is_empty());
    let said = posted(&rig, &runner, |c| c.contains("was not carried out"));
    assert!(said.contains("no note for the worker"), "{said}");
}

#[test]
fn park_with_no_ruling_raises_a_question_at_the_next_comment_read() {
    let (rig, runner, head) = bare_put("said-bare-park");
    let call = calling("park", "", "It asks for a design choice");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(call), Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    let (acted, dropped) = said_bare(&rig, &runner, "Should this be two changes?");
    passed(&rig, &head);
    let said = posted(&rig, &runner, |c| c.contains("parked this work item"));

    assert_eq!(dropped, Vec::<String>::new());
    assert!(acted[0].contains("park on #71"), "{acted:?}");
    assert!(said.contains("Should this be two changes?"), "{said}");
    assert!(said.contains("It asks for a design choice"), "{said}");
    assert!(pending(&rig), "a question waits for the maintainer");
    let runner = runner.lock().unwrap();
    assert!(matches!(
        runner.state.rulings[0].kind,
        RulingKind::Question { .. }
    ));
    assert_eq!(turns(&rig), before, "no worker turn started");
}

#[test]
fn a_released_comment_is_held_again_after_a_restart() {
    let (rig, runner, head) = bare_put("said-bare-restart");
    let call = calling("send-back", "Cache it in a map", "The words ask for it");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);
    said_bare(&rig, &runner, "Why is the cache a vector?");
    assert_eq!(runner.lock().unwrap().pm_released().len(), 1);
    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    passed(&rig, &head);
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("Why is the cache a vector?"), "{prompt}");
    assert!(runner.lock().unwrap().pm_released().is_empty());
    let calls = rig.claude.calls();
    assert!(!calls.iter().any(|c| c.prompt.contains("Cache it in a map")));
}

#[test]
fn a_call_on_a_thread_the_wake_held_no_words_for_is_dropped() {
    let (rig, runner) = merge_put("said-unheld", true);
    let call = calling("send-back", "Split the parser", "A note told it so");
    rig.claude.script_for(Role::Pm, [Scripted::Say(call)]);
    let before = turns(&rig);

    rig.ask(&runner, "tell", Some("Split the parser on #71"));
    let (acted, dropped) = wake_ended(&runner);

    assert_eq!(acted, Vec::<String>::new());
    assert!(
        dropped[0].contains("the maintainer's words on it were not held for this wake"),
        "{dropped:?}"
    );
    assert!(pending(&rig));
    assert_eq!(turns(&rig), before, "no worker turn started");
    assert_eq!(not_carried_out(&rig), Vec::<String>::new());
}

#[test]
fn held_words_given_no_call_are_named_in_the_thread() {
    let (rig, runner) = merge_put("said-no-call", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    let (acted, dropped) = said_words(&rig, &runner, "Why is this one so large?");
    let said = posted(&rig, &runner, |c| c.contains("made no call on it"));

    assert_eq!(acted, Vec::<String>::new());
    assert!(
        dropped[0].contains("made no call on the maintainer's words on #71"),
        "{dropped:?}"
    );
    assert!(said.starts_with("@octocat "), "{said}");
    assert!(said.contains("It is still yours."), "{said}");
    assert!(pending(&rig));
}

#[test]
fn a_wake_that_fails_keeps_the_held_words_for_the_next_wake() {
    let (rig, runner) = merge_put("said-failed", true);
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    let call = calling("park", "", "It asks for a design choice");
    rig.claude
        .script_for(Role::Pm, [Scripted::Fail(failed), Scripted::Say(call)]);

    rig.writes_on(71, "octocat", "Should this be two changes?");
    rig.next_look();
    let deadline = Instant::now() + CALL_PATIENCE;
    while runner.lock().unwrap().state.pm_told.is_empty() || pm_seen(&rig).is_empty() {
        assert!(Instant::now() < deadline, "the first wake never ran");
        step(&runner).unwrap();
        std::thread::yield_now();
    }
    assert_eq!(runner.lock().unwrap().state.pm_told.len(), 1, "kept");
    let ended = |r: &StepReport| matches!(r, StepReport::PmAnswered { .. });
    let deadline = Instant::now() + CALL_PATIENCE;
    let mut report = None;
    while report.is_none() && Instant::now() < deadline {
        rig.clock.advance(60 * 60);
        rig.next_look();
        report = step(&runner).unwrap().filter(ended);
    }
    let Some(StepReport::PmAnswered { acted, .. }) = report else {
        panic!("the wake never ran again: {:?}", pm_seen(&rig).len());
    };

    assert!(acted[0].contains("parked on ruling"), "{acted:?}");
    let seen = pm_seen(&rig);
    let again = seen
        .iter()
        .filter(|s| s.call.prompt.contains("two changes"));
    assert_eq!(again.count(), 2, "the second wake read the words again");
    assert!(runner.lock().unwrap().state.pm_told.is_empty());
}

#[test]
fn each_call_leaves_its_why_in_the_journal() {
    let (rig, runner) = merge_put("said-journal", true);
    let park = calling("park", "", "PARK-WHY");
    let settle = calling("settle", "", "SETTLE-WHY");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(park), Scripted::Say(settle)]);

    let (acted, _) = said_words(&rig, &runner, "Should this be two changes?");
    assert!(acted[0].contains("PARK-WHY"), "{acted:?}");
    rig.clock.advance(5 * 60);
    let (_, dropped) = said_words(&rig, &runner, "Those bot threads are only nits.");
    assert!(dropped[0].contains("SETTLE-WHY"), "{dropped:?}");

    let wakes = journaled(&rig);
    assert!(
        wakes
            .iter()
            .any(|(a, _)| a.iter().any(|a| a.contains("PARK-WHY")))
    );
    assert!(
        wakes
            .iter()
            .any(|(_, d)| d.iter().any(|d| d.contains("SETTLE-WHY")))
    );
    let notes = std::fs::read_to_string(rig.paths().pm.join("pm-notes.md")).unwrap();
    assert!(notes.contains("PARK-WHY"), "{notes}");
}

#[test]
fn a_said_call_adds_no_wake() {
    let (rig, runner) = merge_put("said-one-wake", true);
    let call = calling("park", "", "It asks for a design choice");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(call), Scripted::Say(NOTHING)]);

    said_words(&rig, &runner, "Should this be two changes?");
    for _ in 0..4 {
        looked(&rig, &runner);
        rig.clock.advance(crate::runner::review_bot::LATE_READ);
    }

    let wakes = pm_seen(&rig);
    assert_eq!(wakes.len(), 1, "{:?}", wakes.iter().map(|s| &s.call.prompt));
    assert!(wakes[0].call.prompt.contains("two changes"));
    assert!(runner.lock().unwrap().state.pm_told.is_empty());
}
