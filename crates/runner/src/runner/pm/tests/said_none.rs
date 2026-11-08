//! A `none` call on the maintainer's words on an issue, which is the reply
//! when it has one and no call when it has none

use std::sync::Mutex;

use super::asked::comments;
use super::name_pm;
use crate::ports::{AuthorAssociation, Clock, OpenIssue, Timestamp};
use crate::runner::Runner;
use crate::runner::pm::held_line;
use crate::runner::pm::said::{SaidCall, SaidHow};
use crate::test::Rig;

/// A rig whose open work item builds issue 7, the maintainer `octocat`'s,
/// with a project manager named
fn issue_with_item(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = Rig::with_pull_request_set(project, |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
        rig.forge.list_open_issue(OpenIssue {
            number: 7,
            title: "Why is the cache a map?".to_owned(),
            body: "It grew on the last release.".to_owned(),
            author: "octocat".to_owned(),
            author_is_bot: false,
            author_association: Some(AuthorAssociation::Owner),
            labels: Vec::new(),
            created: Timestamp(rig.clock.now().0),
        });
    });
    (rig, runner)
}

/// What a wake ending in a `none` call on issue 7 does with the comment held
/// before the work item was open, as `(acted, dropped)`
fn said_none(runner: &Mutex<Runner>, why: &str) -> (Vec<String>, Vec<String>) {
    let told = [held_line(7, 5000, None, "Why is the cache a map?")];
    let call = SaidCall {
        thread: 7,
        call: SaidHow::Nothing,
        note: String::new(),
        why: why.to_owned(),
    };
    runner.lock().unwrap().pm_said(&told, &[call])
}

#[test]
fn a_none_call_with_a_reply_on_an_issue_with_a_work_item_posts_the_reply() {
    let (rig, runner) = issue_with_item("said-none-reply");

    let before = comments(&rig);

    let (acted, dropped) = said_none(&runner, "It is a map because the keys are names.");

    assert_eq!(
        acted,
        ["answered on #7: It is a map because the keys are names."]
    );
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        comments(&rig)[before.len()..],
        ["@octocat It is a map because the keys are names."]
    );
}

#[test]
fn a_none_call_with_no_reply_posts_nothing() {
    let (rig, runner) = issue_with_item("said-none-silent");

    let before = comments(&rig);

    let (acted, dropped) = said_none(&runner, "");

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(dropped, ["said none on #7: it made no call"]);
    assert_eq!(comments(&rig), before);
}
