//! The maintainer's comment on an issue whose triage asked its author, which
//! the triage poll reads alone, and the one on an issue it never triaged

use std::sync::Mutex;
use std::time::Instant;

use crate::ports::{AuthorAssociation, Clock, OpenIssue, Role, Timestamp};
use crate::runner::{Runner, step};
use crate::test::{CALL_PATIENCE, Rig, Scripted};

const ONE_QUESTION: &str = "{\"type\": \"bug\", \"priority\": \"P2\", \"related\": [], \"files\": \
    [], \"tests\": [], \"criteria\": [], \"open\": [{\"question\": \"Which OS crashes?\", \
    \"recommended\": \"Linux\"}], \"body\": null}";

/// A project with a project manager and `octocat` for its maintainer, whose
/// open issue 44 was written by `author`
fn rig_with_issue(project: &str, author: &str) -> Rig {
    let rig = Rig::new(project);
    rig.with_app();
    rig.maintainer("octocat");
    rig.edit_settings(|s| {
        let agents = "[app.dogs.kelpie.agents]\n";
        let named = s.replace(agents, &format!("{agents}pm = \"pm\"\n"));
        format!("{named}\n[app.dogs.kelpie.triage]\npick = \"all\"\nwait_days = 1\n")
    });
    rig.forge.list_open_issue(OpenIssue {
        number: 44,
        title: "It crashes on start".to_owned(),
        body: "It crashes when I run it.".to_owned(),
        author: author.to_owned(),
        author_is_bot: false,
        author_association: Some(AuthorAssociation::Contributor),
        labels: Vec::new(),
        created: Timestamp(rig.clock.now().0 + 5),
    });
    rig
}

/// Steps through a few reads of the board
fn looked(rig: &Rig, runner: &Mutex<Runner>) {
    for _ in 0..3 {
        rig.next_look();
        step(runner).unwrap();
    }
}

/// Steps until a comment holding `words` is posted
fn posted(rig: &Rig, runner: &Mutex<Runner>, words: &str) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while !rig.github.comments().iter().any(|(_, c)| c.contains(words)) {
        assert!(Instant::now() < deadline, "never posted {words}");
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

fn told(runner: &Mutex<Runner>) -> Vec<String> {
    runner.lock().unwrap().state.pm_told.clone()
}

#[test]
fn an_answer_to_a_lapsed_triage_question_is_not_held_for_the_project_manager() {
    let rig = rig_with_issue("triage-answer-lapsed", "newcomer");
    rig.claude
        .script_for(Role::Triager, [Scripted::Say(ONE_QUESTION)]);
    rig.claude.script_for(
        Role::Representative,
        [Scripted::Say("Answer the questions below.")],
    );
    let runner = rig.open().unwrap();
    step(&runner).unwrap(); // lists the issue and starts its triager call
    posted(&rig, &runner, "Which OS crashes?");

    // The author stays silent past `wait_days`, and the maintainer writes
    // before the read that ends the wait
    rig.clock.advance(2 * Rig::DAY);
    rig.writes_on(44, "octocat", "ok");
    looked(&rig, &runner);

    assert_eq!(told(&runner), Vec::<String>::new(), "nothing was held");
    assert_eq!(rig.github.reactions().len(), 0, "no thumbs up");
}

#[test]
fn a_comment_on_an_untriaged_issue_is_still_held() {
    let rig = rig_with_issue("triage-answer-untriaged", "octocat");
    let runner = rig.open().unwrap();
    step(&runner).unwrap(); // lists the issue

    rig.writes_on(44, "octocat", "Why is the cache a map?");
    looked(&rig, &runner);

    assert_eq!(told(&runner).len(), 1, "{:?}", told(&runner));
    assert_eq!(rig.github.reactions().len(), 1, "a thumbs up");
}
