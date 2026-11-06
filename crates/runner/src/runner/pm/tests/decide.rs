//! Handing one ruling to the project manager with `pm decide`

use std::sync::Mutex;

use super::super::asked::never_decided;
use super::asked::{comments, merge_put, pending, replied};
use super::{name_pm, pm_seen};
use crate::ports::Role;
use crate::runner::{Runner, StepReport, step};
use crate::settings::RulingName;
use crate::state::{Held, Ruling, RulingKind, Stuck};
use crate::test::ticket;
use crate::test::{Rig, Scripted};

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
