//! Which issues `status` lists as due for triage, through the runner's stand-ins

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::Value;

use super::research_tests::{WORDS, calls, nothing, reply};
use crate::ports::{AuthorAssociation, Clock, OpenIssue, Role, Timestamp};
use crate::runner::trigger::lock;
use crate::runner::{Runner, advance, answer, step};
use crate::state::{Ruling, RulingKind, StateStore, Stuck};
use crate::test::{CALL_PATIENCE, Rig, Scripted, ticket};

// The maintainer is not kelpie's account, which is `the-maintainer`
pub(super) const MAINTAINER: &str = "Boss";

// A rig whose App covers the repo, with `[app.dogs.kelpie.triage]` set to `lines`
pub(super) fn rig_with(lines: &str) -> Rig {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer(MAINTAINER);
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.triage]\n{lines}\n"));
    rig
}

pub(super) fn triaging(pick: &str) -> (Rig, Mutex<Runner>) {
    let rig = rig_with(&format!("pick = \"{pick}\""));
    let runner = rig.open().unwrap();
    (rig, runner)
}

// An issue by `author` made `after` seconds after the rig's clock now reads
pub(super) fn issue(rig: &Rig, number: u64, author: &str, after: u64) -> OpenIssue {
    OpenIssue {
        number,
        title: format!("issue {number}"),
        body: String::new(),
        author: author.to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Contributor),
        labels: Vec::new(),
        created: Timestamp(rig.clock.now().0 + after),
    }
}

pub(super) fn list(rig: &Rig, issue: OpenIssue) {
    rig.forge.list_open_issue(issue);
}

pub(super) fn status(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)
}

// The issues `status` lists as due, in the order it lists them
pub(super) fn due(rig: &Rig, runner: &Mutex<Runner>) -> Vec<u64> {
    let status = status(rig, runner);
    let listed = status["triage"]["due"].as_array().cloned();
    let numbers = listed.unwrap_or_default();
    numbers
        .iter()
        .map(|i| i["issue"].as_u64().unwrap())
        .collect()
}

#[test]
fn status_lists_an_unlabelled_issue_opened_after_pick_was_turned_on() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 31, "newcomer", 20));
    list(&rig, issue(&rig, 30, "newcomer", 10));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [30, 31], "oldest first");
    let status = status(&rig, &runner);
    assert_eq!(status["triage"]["due"][0]["title"], "issue 30");
    assert_eq!(status["triage"]["due"][0]["author"], "newcomer");
    assert_eq!(rig.forge.comments(), [], "nothing is posted");
}

#[test]
fn an_issue_opened_before_pick_was_turned_on_is_not_listed() {
    let rig = rig_with("pick = \"all\"");
    let old = issue(&rig, 30, "newcomer", 0);
    rig.clock.advance(100);
    list(&rig, old);
    let runner = rig.open().unwrap();
    list(&rig, issue(&rig, 31, "newcomer", 1));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [31]);
}

// A `ready-for-agent` the issue check refuses is listed, as no decision.
#[test]
fn an_issue_with_any_triage_label_but_a_refused_ready_for_agent_is_not_listed() {
    let (rig, runner) = triaging("all");
    for (number, label) in [
        (31, "needs-info"),
        (32, "ready-for-agent"),
        (33, "ready-for-human"),
        (34, "wontfix"),
    ] {
        let mut labelled = issue(&rig, number, "newcomer", 5);
        labelled.labels = vec!["bug".to_owned(), label.to_owned()];
        list(&rig, labelled);
    }
    let mut other = issue(&rig, 35, "newcomer", 5);
    other.labels = vec!["bug".to_owned()];
    list(&rig, other);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [32, 35]);
}

#[test]
fn an_issue_someone_labelled_needs_triage_by_hand_is_listed() {
    let (rig, runner) = triaging("all");
    let mut by_hand = issue(&rig, 30, "newcomer", 5);
    by_hand.labels = vec!["needs-triage".to_owned()];
    list(&rig, by_hand);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [30]);
}

// The records of the issues kelpie triaged first went in a prune, which kept the
// highest number it dropped: a `needs-triage` at or below it is kelpie's own
#[test]
fn a_needs_triage_on_an_issue_whose_record_was_pruned_is_not_read_as_by_hand() {
    let rig = rig_with("pick = \"all\"");
    let store = StateStore::new(rig.paths().state);
    let runner = rig.open().unwrap();
    drop(runner);
    let mut saved = serde_json::to_value(store.load().unwrap().unwrap()).unwrap();
    saved["triage"]["pruned_through"] = 30.into();
    store.save(&serde_json::from_value(saved).unwrap()).unwrap();
    let runner = rig.open().unwrap();
    for number in [30, 31] {
        let mut labelled = issue(&rig, number, "newcomer", 5);
        labelled.labels = vec!["needs-triage".to_owned()];
        list(&rig, labelled);
    }

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [31]);
}

#[test]
fn an_issue_kelpie_triaged_is_not_listed_again_whatever_its_labels() {
    let (rig, runner) = triaging("all");
    let mut by_hand = issue(&rig, 30, "newcomer", 5);
    by_hand.labels = vec!["needs-triage".to_owned()];
    list(&rig, by_hand);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(nothing())]);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.github.comments().len(), 1, "it was triaged");

    for labels in [vec![], vec!["needs-triage"], vec!["bug"]] as [Vec<&str>; 3] {
        let mut again = issue(&rig, 30, "newcomer", 5);
        again.labels = labels.into_iter().map(str::to_owned).collect();
        list(&rig, again);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(due(&rig, &runner), Vec::<u64>::new());
    }
}

#[test]
fn a_bots_issue_is_not_listed() {
    let (rig, runner) = triaging("all");
    for (number, author) in [(30, "dependabot[bot]"), (31, "kelpie-app[bot]")] {
        let mut bot = issue(&rig, number, author, 5);
        bot.author_is_bot = true;
        list(&rig, bot);
    }
    list(&rig, issue(&rig, 32, "newcomer", 5));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [32]);
}

#[test]
fn pick_maintainer_lists_only_the_maintainers_issues_ignoring_case() {
    let (rig, runner) = triaging("maintainer");
    list(&rig, issue(&rig, 30, "boss", 5));
    list(&rig, issue(&rig, 31, "stranger", 6));
    list(&rig, issue(&rig, 32, "BOSS", 7));
    list(&rig, issue(&rig, 33, "", 8));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [30, 32]);
}

#[test]
fn pick_all_lists_other_people_s_issues_too() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "stranger", 5));
    list(&rig, issue(&rig, 31, "", 6));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [30, 31], "a deleted account's too");
}

#[test]
fn pick_off_starts_with_no_triager_file_and_lists_nothing() {
    let rig = rig_with("pick = \"off\"\nagent = \"nobody\"");
    let runner = rig.open().unwrap();
    list(&rig, issue(&rig, 30, "newcomer", 5));

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
    assert!(status(&rig, &runner).get("triage").is_none());
}

#[test]
fn a_pick_other_than_off_without_the_app_refuses_to_start_naming_triage_pick() {
    for pick in ["maintainer", "all"] {
        let rig = Rig::new("shep");
        rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.triage]\npick = \"{pick}\"\n"));
        let err = rig.open().unwrap_err().to_string();
        assert!(err.contains("triage.pick"), "{err}");
        assert!(err.contains("as the maintainer"), "{err}");
    }
}

#[test]
fn a_missing_or_wrong_role_triage_agent_refuses_to_start_naming_triage_agent() {
    for agent in ["nobody", "opus-high"] {
        let rig = rig_with(&format!("pick = \"all\"\nagent = \"{agent}\""));
        let err = rig.open().unwrap_err().to_string();
        assert!(err.contains("triage.agent"), "{agent}: {err}");
    }
}

#[test]
fn since_survives_a_restart_and_pick_off_at_start_clears_it() {
    let rig = rig_with("pick = \"all\"");
    let first = rig.open().unwrap();
    assert_eq!(step(&first).unwrap(), None);
    rig.clock.advance(100);
    list(&rig, issue(&rig, 30, "newcomer", 0));
    // It was opened between the first start and the second.
    list(&rig, {
        let mut between = issue(&rig, 31, "newcomer", 0);
        between.created = Timestamp(between.created.0 - 50);
        between
    });

    let second = rig.open().unwrap();
    assert_eq!(step(&second).unwrap(), None);
    assert_eq!(
        due(&rig, &second),
        [31, 30],
        "since stayed at the first start"
    );

    rig.edit_settings(|s| s.replace("pick = \"all\"", "pick = \"off\""));
    let off = rig.open().unwrap();
    assert_eq!(step(&off).unwrap(), None);
    assert_eq!(due(&rig, &off), Vec::<u64>::new());

    rig.edit_settings(|s| s.replace("pick = \"off\"", "pick = \"all\""));
    rig.clock.advance(100);
    let back = rig.open().unwrap();
    assert_eq!(step(&back).unwrap(), None);
    assert_eq!(
        due(&rig, &back),
        Vec::<u64>::new(),
        "since is the new start"
    );
}

#[test]
fn an_unreadable_listing_lists_nothing_and_fails_no_poll() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.forge.set_issues_down(true);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());

    rig.forge.set_issues_down(false);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), [30]);
}

// A pass lists the open issues with the runner's lock let go, so a trigger that needs the
// lock is answered while the forge is slow, and no hold of the lock is as long as the read.
#[test]
fn a_trigger_is_answered_while_the_triage_listing_is_read() {
    const HELD_FOR: Duration = Duration::from_millis(300);
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let read = rig.forge.hold_next_listing_read();

    let pass = std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(
            read.waiting(CALL_PATIENCE),
            "the pass never reached the listing"
        );
        let (sent, heard) = mpsc::channel();
        {
            let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
            scope.spawn(move || sent.send(answer(&runner, &postings, "timings", None)));
        }
        let answered = heard.recv_timeout(CALL_PATIENCE);
        std::thread::sleep(HELD_FOR);
        // Before the assertion, so a trigger that is still waiting is let go.
        drop(read);
        assert!(
            answered.is_ok(),
            "`timings` waited for the lock while the listing was read"
        );
        pass.join().expect("the pass thread")
    });

    assert!(pass.is_ok(), "{pass:?}");
    let status = status(&rig, &runner);
    let longest = (status["lock"]
        .as_object()
        .expect("the holds by site")
        .values())
    .map(|seconds| seconds.as_f64().unwrap())
    .fold(0.0, f64::max);
    assert!(
        longest < HELD_FOR.as_secs_f64(),
        "a hold lasted as long as the read: {longest}s"
    );
}

#[test]
fn a_listing_that_fails_leaves_the_look_due_and_stamps_nothing() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.forge.set_issues_down(true);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());

    // No time has passed, and the next pass lists again.
    rig.forge.set_issues_down(false);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(due(&rig, &runner), [30]);
}

// A second pass lists, triages and labels the issue in the gap the first one's listing was read
// across, so the listing it applies is out of date by one triage.
#[test]
fn an_issue_labelled_while_the_listing_was_read_is_not_triaged_twice() {
    let (rig, runner) = triaging("all");
    list(&rig, issue(&rig, 30, "newcomer", 5));
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(WORDS)]);
    rig.claude.script_for(Role::Triager, [reply(nothing())]);
    let runner = Arc::new(runner);
    let read = rig.forge.hold_next_listing_read();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| step(&runner));
        assert!(
            read.waiting(CALL_PATIENCE),
            "the pass never reached the listing"
        );
        let second = step(&runner);
        drop(read);
        assert_eq!(second.unwrap(), None);
        assert_eq!(pass.join().expect("the pass thread").unwrap(), None);
    });

    assert_eq!(rig.github.comments().len(), 1, "it was triaged once");
    assert_eq!(calls(&rig, Role::Triager).len(), 1);
    assert_eq!(due(&rig, &runner), Vec::<u64>::new());
}

// What the apply saves is cloned from the state it finds once the lock is taken again, not from
// the one the job was built on.
#[test]
fn a_ruling_saved_while_the_listing_was_read_is_kept() {
    let rig = rig_with("pick = \"maintainer\"");
    let mut by_hand = issue(&rig, 30, MAINTAINER, 5);
    by_hand.labels = vec!["ready-for-agent".to_owned()];
    list(&rig, by_hand);
    let runner = Arc::new(rig.open().unwrap());
    let read = rig.forge.hold_next_listing_read();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(
            read.waiting(CALL_PATIENCE),
            "the pass never reached the listing"
        );
        let mut held = lock(&runner);
        let mut next = held.state.clone();
        let kind = RulingKind::Stuck(Stuck::AdoptFailed { why: "no".into() });
        next.rulings
            .push(Ruling::new(ticket(71), "Adopt it?", kind));
        held.save(next).unwrap();
        drop(held);
        drop(read);
        pass.join().expect("the pass thread").unwrap();
    });

    let status = status(&rig, &runner);
    assert_eq!(status["rulings"].as_array().map(Vec::len), Some(1));
    let saved =
        serde_json::to_value(StateStore::new(rig.paths().state).load().unwrap().unwrap()).unwrap();
    let kept = &saved["checks"];
    assert_eq!(
        kept[0]["issue"], 30,
        "the apply saved its result: {kept} for {saved}"
    );
}
