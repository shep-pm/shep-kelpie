//! A post that comes after a wait on our side owns it, through the runner's
//! stand-ins

use std::sync::Mutex;

use super::{IssuePost, Opening, Post};
use crate::ports::{Clock as _, Role, Timestamp};
use crate::runner::Runner;
use crate::runner::contributions::pass_tests::read_through;
use crate::runner::contributions::security_tests::contribution;
use crate::runner::contributions::tests::poll_or_adopt;
use crate::runner::review_bot::contribution_turns::{FINDING, contributed, until_summoned};
use crate::test::{Rig, Scripted, VIEWER};

const CONTRIBUTOR: &str = "newcomer";
// The maintainer is not kelpie's account, which is `VIEWER`
const BOSS: &str = "Boss";
const DAY: u64 = Rig::DAY;
const NUDGE_DAYS: u64 = 3;
const QUIET_DAYS: u64 = 7;
const WAIT_DAYS: u64 = 2;

// A project that picks contributions with `nudge_days` and triages every issue
// with `wait_days`, so both waits are set apart from their defaults
fn picking() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer(BOSS);
    rig.edit_settings(|s| {
        format!(
            "{s}\n[app.dogs.kelpie.contributions]\npick = \"all\"\nnudge_days = {NUDGE_DAYS}\n\
             quiet_days = {QUIET_DAYS}\n[app.dogs.kelpie.triage]\npick = \"all\"\n\
             wait_days = {WAIT_DAYS}\n"
        )
    });
    let runner = rig.open().unwrap();
    (rig, runner)
}

// A post on pull request `number`, answering what the contributor wrote
fn post<'a>(number: u64, said: &'a str) -> Post<'a> {
    Post {
        pull_request: number,
        kind: "Tell the contributor the checks passed",
        facts: "The checks passed on the latest head.\n",
        earlier: &[],
        contributor: CONTRIBUTOR,
        said,
    }
}

// A post on an issue, greeting its author
fn issue_post() -> IssuePost<'static> {
    IssuePost {
        kind: "Write the triage comment",
        facts: "It is a bug.\n",
        earlier: &[],
        opening: Opening {
            greet: Some(CONTRIBUTOR),
            thanks: true,
        },
        author: CONTRIBUTOR,
        said: "It crashes when I run it.",
    }
}

// What the representative's last call was asked
fn prompt(rig: &Rig) -> String {
    (rig.claude.all_seen().into_iter())
        .filter(|seen| seen.call.role == Role::Representative)
        .map(|seen| seen.call.prompt)
        .last()
        .expect("the call was made")
}

// The wait the prompt owns, as the fact line names it, or `None` when it names none
fn owned(rig: &Rig) -> Option<String> {
    let said = prompt(rig);
    let from = said.find("This thread waited ")?;
    let line = said[from..].split('\n').next().expect("the fact line");
    Some(line.to_owned())
}

// `login` writes `body` on thread `number` `ago` whole days before the clock now
fn wrote(rig: &Rig, number: u64, login: &str, body: &str, ago: u64) {
    let at = rig.clock.now().0.saturating_sub(ago * DAY);
    (rig.forge.threads).write(number, login, body, Timestamp(at));
}

#[test]
fn a_reply_unanswered_past_nudge_days_gets_the_fact() {
    for (ago, waited) in [(NUDGE_DAYS, true), (NUDGE_DAYS - 1, false)] {
        let (rig, runner) = picking();
        wrote(&rig, 9, CONTRIBUTOR, "Any news on my pull request?", ago);
        rig.claude
            .script_for(Role::Representative, [Scripted::Say("Ok.")]);

        runner
            .lock()
            .unwrap()
            .represent(&post(9, "Any news?"))
            .unwrap();

        let fact = owned(&rig);
        match waited {
            true => assert_eq!(
                fact.as_deref(),
                Some("This thread waited 3 days on kelpie or the maintainer before this post."),
                "{ago} days: {}",
                prompt(&rig)
            ),
            false => assert_eq!(fact, None, "{ago} days: {}", prompt(&rig)),
        }
    }
}

#[test]
fn a_comment_of_ours_after_their_word_clears_the_wait() {
    for login in [VIEWER, BOSS] {
        let (rig, runner) = picking();
        wrote(&rig, 9, CONTRIBUTOR, "Any news on my pull request?", 5);
        wrote(&rig, 9, login, "The checks passed on your latest head.", 1);
        rig.claude
            .script_for(Role::Representative, [Scripted::Say("Ok.")]);

        runner
            .lock()
            .unwrap()
            .represent(&post(9, "Any news?"))
            .unwrap();

        assert_eq!(owned(&rig), None, "{login} answered it: {}", prompt(&rig));
    }
}

// Pull request 30 by `newcomer`, whose head a review bot left a thread in, so
// kelpie waits on its contributor, and its first ask is posted at the nudge.
// The representative says `ASK` for it and `TAKEN` for the release.
fn asked() -> (Rig, Mutex<Runner>) {
    const ASK: &str = "May kelpie take this from here?";
    const TAKEN: &str = "This pull request is kelpie's now.";
    let (rig, runner, head) = contributed(&["cubic", "mine"]);
    until_summoned(&rig, &runner);
    let at = rig.clock.now().0 + 30;
    rig.forge.coderabbit.cubic_review(30, &head, at, &[FINDING]);
    read_through(&rig, &runner, &head);
    rig.claude.script_for(
        Role::Representative,
        [Scripted::Say(ASK), Scripted::Say(TAKEN)],
    );
    poll_or_adopt(&rig, &runner);
    poll_or_adopt(&rig, &runner);
    // The nudge posts the ask, which is where the promise this post is late on
    // was made
    rig.clock.advance(NUDGE_DAYS * DAY);
    poll_or_adopt(&rig, &runner);
    (rig, runner)
}

// Lets `days` pass since the clock said kelpie would act on silence, then polls
fn past_the_moment(rig: &Rig, runner: &Mutex<Runner>, days: u64) {
    let to_quiet = contribution(rig, runner)["clock"]["days_to_quiet"]
        .as_u64()
        .expect("the clock runs");
    rig.clock.advance((to_quiet + days) * DAY);
    poll_or_adopt(rig, runner);
}

#[test]
fn a_release_ask_acted_on_late_gets_the_fact() {
    let (rig, runner) = asked();

    past_the_moment(&rig, &runner, 9);

    let fact = owned(&rig).expect("the release line owns the wait");
    assert_eq!(
        fact,
        "This thread waited 9 days on kelpie or the maintainer before this post.",
        "{}",
        prompt(&rig)
    );
}

// The heading a triage comment carries its questions under, which is what makes
// it the ask a later post is late on
const QUESTIONS: &str = "❓ Q1 - Which OS crashes?\n\n➡️ Linux";

#[test]
fn a_lapsed_triage_ask_acted_on_late_gets_the_fact() {
    let (rig, runner) = picking();
    wrote(&rig, 30, VIEWER, QUESTIONS, WAIT_DAYS + 9);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say("Ok.")]);

    runner
        .lock()
        .unwrap()
        .represent_issue(30, &issue_post())
        .unwrap();

    let fact = owned(&rig).expect("the post owns the lapsed ask");
    assert_eq!(
        fact,
        "This thread waited 9 days on kelpie or the maintainer before this post.",
        "{}",
        prompt(&rig)
    );
}

#[test]
fn an_ask_the_author_answered_inside_its_wait_is_no_late_promise() {
    let (rig, runner) = picking();
    // The ask, an answer inside its wait, and kelpie's word after the answer
    wrote(&rig, 30, VIEWER, QUESTIONS, WAIT_DAYS + 9);
    wrote(&rig, 30, CONTRIBUTOR, "Linux, on start.", WAIT_DAYS + 8);
    wrote(&rig, 30, VIEWER, "That settles most of it.", 1);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say("Ok.")]);

    runner
        .lock()
        .unwrap()
        .represent_issue(30, &issue_post())
        .unwrap();

    assert_eq!(owned(&rig), None, "{}", prompt(&rig));
}

#[test]
fn a_failed_comment_read_leaves_the_fact_out() {
    let (rig, runner) = picking();
    wrote(
        &rig,
        9,
        CONTRIBUTOR,
        "Any news on my pull request?",
        NUDGE_DAYS + 4,
    );
    (rig.forge.threads).set_unlisted(9, true);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say("Ok.")]);

    let words = runner.lock().unwrap().represent(&post(9, "Any news?"));

    assert_eq!(words.unwrap(), "Ok.");
    assert_eq!(owned(&rig), None, "{}", prompt(&rig));
}
