//! The rulings the maintainer puts to the project manager from their thread:
//! what is posted, what is applied, and what is refused

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::super::asked::never_decided;
use super::{name_pm, pm_seen};
use crate::ports::{AgentError, Checks, Role};
use crate::runner::flight::advance;
use crate::runner::{Runner, StepReport, step};
use crate::settings::{Harness, RulingName};
use crate::state::{Held, Ruling, RulingKind, Stuck};
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};

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
fn worker_sent(rig: &Rig, runner: &Mutex<Runner>, text: &str) {
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

// Handing one ruling to the project manager with `pm decide`

/// A project with a project manager whose worker is parked on ruling 71,
/// a label put on its pull request outside kelpie, posted to the maintainer
pub(super) fn foreign_put(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = Rig::with_pull_request_set(project, |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    rig.forge.label_pull_request(71, "bug");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    rig.forge.threads.grant_write("octocat");
    let card = |c: &String| c.starts_with("### Accept a change made outside kelpie?");
    assert!(comments(&rig).iter().any(card), "{:?}", comments(&rig));
    rig.clock.advance(5);
    (rig, runner)
}

/// The project manager's vote on ruling 71 for `choice`, flagged `design`
pub(super) fn decided(choice: char, design: bool) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": 71, \"choice\": \"{choice}\", \"why\": \
         \"PM-WHY\", \"design\": {design}}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

#[test]
fn pm_decide_applies_its_answer_as_if_the_project_left_the_ruling_to_it() {
    let (rig, runner) = foreign_put("asked-decide");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(decided('A', false))]);

    let said = replied(&rig, &runner, "71 pm decide", |c| {
        c.starts_with("**Answered by the project manager")
    });

    assert!(
        said.starts_with("**Answered by the project manager: A, Yes**"),
        "{said}"
    );
    assert!(
        said.contains("Reply `71 no <note>` to reopen it."),
        "{said}"
    );
    assert!(!pending(&rig), "the answer was applied");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["pm_answers"]["foreign-change"], 1, "{status}");
}

#[test]
fn a_design_flag_on_pm_decide_leaves_the_ruling_the_maintainers_with_its_pick() {
    let (rig, runner) = foreign_put("asked-decide-design");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(decided('B', true))]);

    let said = replied(&rig, &runner, "71 pm decide", |c| {
        c.contains("**Project manager: B**")
    });

    assert!(said.contains("**Project manager: B**. PM-WHY"), "{said}");
    assert!(
        said.contains("It would answer `71 no <note>` (No)."),
        "{said}"
    );
    assert!(
        said.contains("Ruling #71 is still yours, since it flagged design"),
        "{said}"
    );
    assert!(pending(&rig));
}

#[test]
fn pm_decide_on_a_merge_is_refused_naming_the_kind_and_wakes_nothing() {
    let (rig, runner) = merge_put("asked-decide-merge", true);

    let said = replied(&rig, &runner, "71 pm decide", |c| {
        c.contains("was not put to the project manager")
    });

    assert!(
        said.contains("the project manager never decides a `merge` ruling"),
        "{said}"
    );
    assert_eq!(pm_seen(&rig).len(), 0);
    assert!(pending(&rig));
}

#[test]
fn pm_decide_is_refused_on_every_kind_the_project_manager_never_decides() {
    let never: Vec<RulingName> = (RulingName::ALL.into_iter())
        .filter(|n| n.pm_default().is_none())
        .collect();
    assert!(never.iter().any(|n| n.as_str() == "local-model-spilled"));
    for name in never {
        let refused = never_decided(name).unwrap_err();
        assert!(
            refused.contains(&format!("`{}`", name.as_str())),
            "{refused}"
        );
    }
    let (_rig, runner) = super::with_pm("asked-decide-refused");
    let runner = runner.lock().unwrap();
    let merge = RulingKind::Contribution(Held::Merge {
        head: "abc".into(),
        rework_off: false,
    });
    let refused = runner.pm_may_decide(&Ruling::new(ticket(9), "q", merge));
    assert!(refused.unwrap_err().contains("`contribution-merge`"));
    let rebase = RulingKind::from(Stuck::Rebase {
        why: "conflicts".into(),
        review: None,
    });
    assert_eq!(
        runner.pm_may_decide(&Ruling::new(ticket(9), "q", rebase)),
        Ok(())
    );
}
