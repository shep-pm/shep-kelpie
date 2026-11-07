//! The maintainer's free text under a ruling, which a project with a
//! project manager reads as words to it, and what it leaves unchanged

use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

use super::asked::{
    advanced_until, comments, looked, made_stuck, merge_put, pending, posted, refusals, turns,
    woken_prompt, worker_sent,
};
use super::{NOTHING, answered, name_pm, pm_seen, step_until};
use crate::ports::{AuthorAssociation, Checks, Clock, OpenIssue, Role, Timestamp};
use crate::runner::flight::advance;
use crate::runner::{Runner, StepReport, step};
use crate::state::{Resume, RulingKind};
use crate::test::{CALL_PATIENCE, Rig, Scripted, ticket};

// What the triager found on the issue that asks one question only its author
// can answer, and what its second pass found once they had
const ONE_QUESTION: &str = "{\"type\": \"bug\", \"priority\": \"P2\", \"related\": [], \"files\": \
    [], \"tests\": [], \"criteria\": [], \"open\": [{\"question\": \"Which OS crashes?\", \
    \"recommended\": \"Linux\"}], \"body\": null}";
const SETTLED: &str = "{\"type\": \"bug\", \"priority\": \"P2\", \"related\": [], \"files\": [], \
    \"tests\": [], \"criteria\": [], \"open\": [], \"body\": \"## Settled\\n\\n- Linux, on start.\\\
    n\"}";
const ASK_WORDS: &str = "Thanks for the report. Answer the questions below in one comment.";
const ANSWER_WORDS: &str = "That settles most of it.";

#[test]
fn a_plain_comment_under_a_merge_ruling_waits_for_the_project_manager() {
    let (rig, runner) = merge_put("asked-held-merge", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(71, "octocat", "Why is this one so large?");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("Why is this one so large?"), "{prompt}");
    assert!(pending(&rig), "the ruling waits for an answer");
    looked(&rig, &runner);
    assert_eq!(turns(&rig), before, "no worker turn started");
    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
    assert_eq!(refusals(&rig), Vec::<String>::new());
}

#[test]
fn a_plain_comment_under_a_stuck_ruling_waits_and_is_not_told_how_to_reply() {
    let (rig, runner) = merge_put("asked-held-stuck", true);
    made_stuck(&runner);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.writes_on(71, "octocat", "go ahead and retry");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("go ahead and retry"), "{prompt}");
    assert!(pending(&rig));
    assert_eq!(rig.github.reactions().len(), 1);
    assert_eq!(refusals(&rig), Vec::<String>::new());
}

#[test]
fn free_text_after_a_rulings_number_waits_for_the_project_manager() {
    let (rig, runner) = merge_put("asked-held-numbered", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(71, "octocat", "71 is this the smaller option?");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("is this the smaller option?"), "{prompt}");
    assert!(pending(&rig));
    looked(&rig, &runner);
    assert_eq!(turns(&rig), before);
    assert_eq!(rig.github.reactions().len(), 1);
    assert_eq!(refusals(&rig), Vec::<String>::new());
}

#[test]
fn a_no_note_and_a_rework_still_answer_a_merge_ruling_with_a_project_manager_named() {
    for (reply, note) in [
        ("71 no fix the nit", "with this note:\n\nfix the nit"),
        ("71 rework split the parser", "split the parser"),
    ] {
        let (rig, runner) = merge_put("asked-held-answers", true);
        rig.claude.script([Scripted::Push("flag.txt", "flag\n")]);

        rig.writes_on(71, "octocat", reply);
        rig.next_look();
        worker_sent(&rig, &runner, note);

        assert!(!pending(&rig), "{reply}");
        assert_eq!(pm_seen(&rig).len(), 0, "{reply}: nothing was held");
    }
}

#[test]
fn plain_text_still_answers_a_workers_question_with_a_project_manager_named() {
    let (rig, runner) = merge_put("asked-held-question", true);
    {
        let mut runner = runner.lock().unwrap();
        let mut next = runner.state.clone();
        next.rulings[0].kind = RulingKind::Question {
            asked: "Which flag?".into(),
            resume: Resume::Nothing,
        };
        runner.save(next).unwrap();
    }

    rig.writes_on(71, "octocat", "use the blue one");
    rig.next_look();
    advanced_until(&runner, || !pending(&rig));

    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
    assert_eq!(pm_seen(&rig).len(), 0, "the answer was the worker's");
}

#[test]
fn the_maintainers_comment_with_no_ruling_waits_and_starts_no_worker_turn() {
    let (rig, runner, head) = Rig::with_pull_request_set("asked-held-bare", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    rig.forge.threads.grant_write("octocat");
    // The comments are read as CI goes green, before any ruling is raised
    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(71, "octocat", "Can you say why the cache is a map?");
    step(&runner).unwrap(); // sees the checks
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    let prompt = woken_prompt(&rig, &runner);

    assert!(
        prompt.contains("Can you say why the cache is a map?"),
        "{prompt}"
    );
    looked(&rig, &runner);
    assert_eq!(turns(&rig), before, "no worker turn started");
}

#[test]
fn another_writers_comment_is_still_a_request_of_the_worker() {
    let (rig, runner) = merge_put("asked-held-other", true);
    rig.forge.threads.grant_write("octo-reviewer");
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);

    rig.writes_on(71, "octo-reviewer", "Please rename the flag.");
    rig.next_look();
    worker_sent(&rig, &runner, "Please rename the flag.");

    assert_eq!(pm_seen(&rig).len(), 0, "nothing was held");
}

#[test]
fn the_maintainers_review_thread_still_reaches_the_worker() {
    use crate::ports::Standing;
    let (rig, runner) = merge_put("asked-held-thread", true);
    rig.claude
        .script([Scripted::Push("rename.txt", "renamed\n")]);

    rig.forge.coderabbit.person_opens(
        71,
        "src/lib.rs",
        "octocat",
        Standing::Writer,
        "This name is wrong.",
    );
    rig.next_look();
    worker_sent(&rig, &runner, "This name is wrong.");

    assert_eq!(pm_seen(&rig).len(), 0, "nothing was held");
}

#[test]
fn a_read_made_again_holds_a_comment_once() {
    let (rig, runner) = merge_put("asked-held-once", true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(NOTHING), Scripted::Say(NOTHING)]);

    rig.writes_on(71, "octocat", "Why is this one so large?");
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);
    assert_eq!(prompt.matches("Why is this one so large?").count(), 1);
    for _ in 0..4 {
        looked(&rig, &runner);
        rig.clock.advance(crate::runner::review_bot::LATE_READ);
    }

    let wakes: Vec<_> = pm_seen(&rig)
        .into_iter()
        .filter(|s| s.call.prompt.contains("Why is this one so large?"))
        .collect();
    assert_eq!(wakes.len(), 1, "the words went to it once");
}

#[test]
fn held_words_outlast_a_restart_and_wake_the_project_manager() {
    let (rig, runner) = merge_put("asked-held-restart", true);
    rig.writes_on(71, "octocat", "Why is this one so large?");
    rig.next_look();
    // Read and held, with no wake started yet
    let held = |runner: &Mutex<Runner>| runner.lock().unwrap().state.pm_told.clone();
    let deadline = Instant::now() + CALL_PATIENCE;
    while held(&runner).is_empty() {
        assert!(Instant::now() < deadline, "the comment was never held");
        advance(&runner).unwrap();
        assert_eq!(pm_seen(&rig).len(), 0, "woken before the restart");
    }
    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("Why is this one so large?"), "{prompt}");
}

// The maintainer's comment on an issue with no work item and no triage wait,
// which the same read holds for the project manager to answer

/// Open issue 44, the maintainer's own, on a project with its App on the repo
/// and the maintainer `octocat`, with nothing on it, and a project manager
fn issue_only(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = issue_listed(project, true);
    let runner = rig.open().unwrap();
    (rig, runner)
}

/// [`issue_only`]'s rig, before its runner opens, with `pm` naming the project
/// manager and open issue 44 the maintainer's own
fn issue_listed(project: &str, pm: bool) -> Rig {
    let rig = Rig::new(project);
    rig.with_app();
    rig.maintainer("octocat");
    if pm {
        name_pm(&rig);
    }
    rig.forge.list_open_issue(OpenIssue {
        number: 44,
        title: "Why is the cache a map?".to_owned(),
        body: "It grew on the last release.".to_owned(),
        author: "octocat".to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Owner),
        labels: Vec::new(),
        created: Timestamp(rig.clock.now().0),
    });
    rig
}

/// An open issue `number` the maintainer opened, on the rig's forge
fn listed(rig: &Rig, number: u64, title: &str, body: &str) {
    rig.forge.list_open_issue(OpenIssue {
        number,
        title: title.to_owned(),
        body: body.to_owned(),
        author: "octocat".to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Owner),
        labels: Vec::new(),
        created: Timestamp(rig.clock.now().0),
    });
}

/// [`merge_put`], with open issue 44 listed before its runner opens, so the
/// poll that first lists it comes before the read that is made from it, and a
/// project manager where `pm` says
fn merge_put_listed(project: &str, pm: bool) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_set(project, |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        if pm {
            name_pm(rig);
        }
        listed(
            rig,
            44,
            "Why is the cache a map?",
            "It grew on the last release.",
        );
    });
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    rig.forge.threads.grant_write("octocat");
    rig.clock.advance(5);
    (rig, runner)
}

/// How many of the project manager's wakes carried `words`
fn carried(rig: &Rig, words: &str) -> usize {
    (pm_seen(rig).iter())
        .filter(|s| s.call.prompt.contains(words))
        .count()
}

/// The words the runner is holding for its next wake
fn told(runner: &Mutex<Runner>) -> Vec<String> {
    runner.lock().unwrap().state.pm_told.clone()
}

/// Looks until a wake carries `words`, which the read holds on the poll after
/// the one that listed the issue
fn carried_until(rig: &Rig, runner: &Mutex<Runner>, words: &str) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while carried(rig, words) == 0 {
        assert!(Instant::now() < deadline, "{words} never reached it");
        rig.next_look();
        step(runner).unwrap();
    }
}

#[test]
fn the_maintainers_comment_on_an_issue_with_no_work_item_reaches_the_next_wake() {
    let (rig, runner) = issue_only("held-issue-wake");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    let before = turns(&rig);

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    step(&runner).unwrap(); // lists the open issues, which the next read is made from
    rig.next_look();
    let prompt = woken_prompt(&rig, &runner);

    assert!(prompt.contains("The maintainer's comment"), "{prompt}");
    assert!(prompt.contains("on #44:"), "{prompt}");
    assert!(prompt.contains("Why is the cache a map?"), "{prompt}");
    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
    assert_eq!(turns(&rig), before, "no worker turn started");
    assert_eq!(pm_seen(&rig).len(), 1, "one wake");
}

#[test]
fn the_answer_is_posted_on_the_issue() {
    let (rig, runner) = issue_only("held-issue-answer");
    let answer = json!({
        "pick": null,
        "said": [{"thread": 44, "call": "none", "why": "It is a map because the keys are names."}]
    })
    .to_string();
    let answer: &'static str = Box::leak(answer.into_boxed_str());
    rig.claude.script_for(Role::Pm, [Scripted::Say(answer)]);

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    step(&runner).unwrap(); // lists the open issues, which the next read is made from
    rig.next_look();
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));

    assert_eq!(
        acted,
        ["answered on #44: It is a map because the keys are names."]
    );
    assert_eq!(dropped, Vec::<String>::new());
    let said = posted(&rig, &runner, |c| c.contains("the keys are names."));
    assert!(said.starts_with("@octocat "), "{said}");
    assert_eq!(
        rig.ask(&runner, "status", None)["pm"]["told"],
        json!(null),
        "carried by the wake it answered"
    );
}

#[test]
fn anyone_elses_comment_on_such_an_issue_is_not_held() {
    let (rig, runner) = issue_only("held-issue-other");
    rig.forge.threads.grant_write("newcomer");
    let before = turns(&rig);

    rig.writes_on(44, "newcomer", "The cache grew for me too.");
    looked(&rig, &runner);

    assert_eq!(carried(&rig, "The cache grew for me too."), 0);
    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
    assert_eq!(comments(&rig), Vec::<String>::new(), "nothing posted");
    assert_eq!(turns(&rig), before, "no worker turn started");
}

#[test]
fn a_comment_answering_a_triage_wait_is_not_held() {
    let rig = Rig::new("held-issue-wait");
    rig.with_app();
    rig.maintainer("octocat");
    name_pm(&rig);
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.triage]\npick = \"maintainer\"\n"));
    rig.forge.list_open_issue(OpenIssue {
        number: 44,
        title: "It crashes on start".to_owned(),
        body: "It crashes when I run it.".to_owned(),
        author: "octocat".to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Owner),
        labels: Vec::new(),
        created: Timestamp(rig.clock.now().0 + 5),
    });
    rig.claude
        .script_for(Role::Triager, [Scripted::Say(ONE_QUESTION)]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    let runner = rig.open().unwrap();

    step(&runner).unwrap(); // lists the issue and starts its triager call
    posted(&rig, &runner, |c| c.contains("Which OS crashes?"));

    rig.writes_on(44, "octocat", "Linux, on start.");
    rig.claude
        .script_for(Role::Triager, [Scripted::Say(SETTLED)]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ANSWER_WORDS)]);
    looked(&rig, &runner);

    assert_eq!(carried(&rig, "Linux, on start."), 0, "its own round");
    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
}

#[test]
fn a_comment_on_an_issue_with_an_open_work_item_stays_as_it_is() {
    let (rig, runner, _) = Rig::with_pull_request_set("held-issue-item", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
        listed(
            rig,
            7,
            "Why is the cache a map?",
            "It grew on the last release.",
        );
    });
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.writes_on(7, "octocat", "Why is the cache a map?");
    looked(&rig, &runner);

    assert_eq!(carried(&rig, "Why is the cache a map?"), 0);
    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
}

#[test]
fn a_comment_on_a_closed_issue_or_a_pull_request_is_not_held() {
    let rig = issue_listed("held-issue-closed", true);
    listed(&rig, 45, "A closed issue", "Nothing to build.");
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    rig.forge.open_pull_request(71, "someone/71", &[]);
    // Closed before the first poll lists it, so no listing ever holds it
    rig.forge.close_issue(45);
    rig.forge.unlist_open_issue(45);

    step(&runner).unwrap(); // lists 44, which the next read is made from
    rig.writes_on(45, "octocat", "Nothing left on this one.");
    rig.writes_on(71, "octocat", "Look over this one.");
    looked(&rig, &runner);

    assert_eq!(carried(&rig, "Look over this one."), 0, "a pull request");
    assert_eq!(carried(&rig, "Nothing left"), 0, "a closed issue");
    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
}

#[test]
fn a_read_made_after_a_restart_does_not_hold_an_older_comment() {
    let (rig, runner) = merge_put_listed("held-issue-restart", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    carried_until(&rig, &runner, "Why is the cache a map?");

    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    // The read reaches back to where the ruling's comment was heard, which is
    // before the comment on #44 was written
    for _ in 0..3 {
        rig.next_look();
        step(&runner).unwrap();
    }

    assert_eq!(told(&runner), Vec::<String>::new(), "not held again");
    assert_eq!(carried(&rig, "Why is the cache a map?"), 1, "held once");
}

#[test]
fn a_comment_read_while_the_listing_failed_is_held_after_it_works() {
    let (rig, runner) = issue_only("held-issue-listing-down");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    rig.forge.set_issues_down(true);
    // The read finds the comment and cannot tell the issue is open, and the
    // poll that same pass makes fails
    step(&runner).unwrap();
    assert_eq!(
        told(&runner),
        Vec::<String>::new(),
        "no listing to decide from"
    );
    rig.forge.set_issues_down(false);
    carried_until(&rig, &runner, "Why is the cache a map?");

    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
    assert_eq!(carried(&rig, "Why is the cache a map?"), 1, "held once");
}

#[test]
fn a_comment_kept_for_a_listing_that_never_came_is_left_unheard_and_said() {
    let (rig, runner) = issue_only("held-issue-listing-gone");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    rig.forge.set_issues_down(true);
    // The first read is made at the comment's own time, and each look after it
    // is 5 min on, so the last one is past how long a comment is kept
    step(&runner).unwrap();
    for _ in 0..4 {
        rig.next_look();
        step(&runner).unwrap();
    }

    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(
        carried(&rig, "Why is the cache a map?"),
        0,
        "no wake with it"
    );
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("#44") && n.contains("left unheard")),
        "{notes:?}"
    );
}

#[test]
fn no_project_manager_means_no_read() {
    let rig = issue_listed("held-issue-none", false);
    let runner = rig.open().unwrap();

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    for _ in 0..3 {
        rig.next_look();
        step(&runner).unwrap();
    }

    assert_eq!(rig.forge.threads.reads(), Vec::new(), "no read was made");
    assert_eq!(pm_seen(&rig).len(), 0);
}

#[test]
fn no_project_manager_holds_nothing_when_a_ruling_makes_the_read() {
    // Ruling 71 waits on the maintainer, so the comments are read with no
    // project manager named to answer them
    let (rig, runner) = merge_put_listed("held-issue-no-pm", false);
    // A read is made before the comment, so it is one this runner saw written
    looked(&rig, &runner);

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    looked(&rig, &runner);

    let reads = rig.forge.threads.reads();
    assert!(!reads.is_empty(), "the ruling's thread is watched");
    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
    assert_eq!(pm_seen(&rig).len(), 0);
}
