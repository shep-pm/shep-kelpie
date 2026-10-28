//! A request of the project manager that outlives its ruling or its runner

use std::time::Duration;

use super::asked::{
    advanced_until, approaches, comments, decided, foreign_put, merge_put, pending, posted,
    replied, vote,
};
use crate::ports::{Role, Timestamp};
use crate::runner::Answer;
use crate::runner::ruling::{Answerer, Record};
use crate::runner::voice::RECOMMENDS;
use crate::state::{Held, Resume, Ruling, RulingKind, Stuck};
use crate::test::{Hold, Rig, Scripted, ticket};
use crate::work_item::{Phase, Turn};

// A ruling raised again under the same id while the project manager works

/// When ruling 71 was raised, as the state keeps it
fn raised(rig: &Rig) -> serde_json::Value {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["rulings"][0]["raised"].clone()
}

#[test]
fn an_answer_for_a_ruling_raised_again_meanwhile_is_dropped_and_says_so() {
    let asks = [
        (
            "asked-again-recommend",
            "71 pm recommend",
            vote('A', "PM-WHY"),
        ),
        (
            "asked-again-options",
            "71 pm options",
            approaches("Keep the old flag"),
        ),
        ("asked-again-decide", "71 pm decide", decided('A', false)),
    ];
    for (project, reply, said) in asks {
        let (rig, runner) = foreign_put(project);
        let hold = Hold::default();
        rig.claude
            .script_for(Role::Pm, [Scripted::HoldThenSay(hold.clone(), said)]);
        rig.writes_on(71, "octocat", reply);
        rig.next_look();
        advanced_until(&runner, || hold.entered(Duration::ZERO));
        let first = raised(&rig);

        // A yes accepts the label; its removal is another change, ruled again.
        rig.ask(&runner, "rule", Some("71 yes"));
        rig.clock.advance(5);
        rig.forge.unlabel_pull_request(71, "bug");
        advanced_until(&runner, || pending(&rig) && raised(&rig) != first);
        hold.release();
        let dropped = || {
            comments(&rig)
                .into_iter()
                .find(|c| c.contains("was dropped"))
        };
        advanced_until(&runner, || dropped().is_some());

        let said = dropped().unwrap();
        assert!(
            said.contains("about ruling #71 was dropped: the ruling was raised again"),
            "{project}: {said}"
        );
        let acted = |c: &String| {
            c.contains("**Project manager") || c.starts_with("**Answered by the project manager")
        };
        assert!(
            !comments(&rig).iter().any(acted),
            "{project}: {:?}",
            comments(&rig)
        );
        assert!(pending(&rig), "{project}: the new ruling still waits");
        let text = std::fs::read_to_string(rig.paths().state).unwrap();
        assert!(!text.contains("approaches"), "{project}: {text}");
    }
}

#[test]
fn pm_decide_is_checked_again_when_its_answer_comes() {
    let (rig, runner) = foreign_put("asked-decide-drained");
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [Scripted::HoldThenSay(hold.clone(), decided('A', false))],
    );
    rig.writes_on(71, "octocat", "71 pm decide");
    rig.next_look();
    advanced_until(&runner, || hold.entered(Duration::ZERO));

    rig.ask(&runner, "drain", None);
    hold.release();
    let left = || {
        comments(&rig)
            .into_iter()
            .find(|c| c.contains("was not applied"))
    };
    advanced_until(&runner, || left().is_some());

    assert!(
        left().unwrap().contains("since the runner drains"),
        "{:?}",
        left()
    );
    assert!(pending(&rig));
}

#[test]
fn a_request_pending_when_the_runner_stops_is_asked_again_when_it_starts() {
    let (rig, runner) = foreign_put("asked-restart");
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::HoldThenSay(hold.clone(), vote('A', "LOST")),
            Scripted::Say(vote('A', "PM-WHY")),
        ],
    );
    rig.writes_on(71, "octocat", "71 pm recommend");
    rig.next_look();
    advanced_until(&runner, || hold.entered(Duration::ZERO));
    assert!(rig.github.reactions().len() == 1, "the request was taken");
    drop(runner);
    hold.release();

    let runner = rig.open().unwrap();
    let said = posted(&rig, &runner, |c| c.contains("**Project manager: A**"));

    assert!(said.contains("PM-WHY"), "{said}");
    assert!(!comments(&rig).iter().any(|c| c.contains("LOST")));
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    assert!(
        !text.contains("\"asked\""),
        "the request is settled: {text}"
    );
}

// What the maintainer may ask about one ruling

#[test]
fn pm_decide_is_refused_on_a_question_and_on_a_failure_it_already_answered() {
    let (rig, runner) = super::with_pm("asked-decide-rules");
    let runner = runner.lock().unwrap();
    let question = RulingKind::Question {
        asked: "Which?".into(),
        resume: Resume::Nothing,
    };
    let refused = runner.pm_may_decide(&Ruling::new(ticket(7), "Which?", question));
    assert!(refused.unwrap_err().contains("its own wake"));

    let failing = Ruling {
        issue: Some(ticket(7)),
        ..Ruling::new(
            ticket(7),
            "harness exited with 1",
            RulingKind::from(Stuck::TurnFailed {
                why: "harness exited with 1".into(),
                phase: Phase::Implement,
                retry: Turn::Due,
                restore: Vec::new(),
                setup: false,
            }),
        )
    };
    assert_eq!(runner.pm_may_decide(&failing), Ok(()));
    let record = Record::of(&failing, &Answer::Yes, Timestamp(1), Answerer::Pm);
    record.append(&rig.paths().rulings).unwrap();
    let refused = runner.pm_may_decide(&failing).unwrap_err();
    assert!(
        refused.contains("already answered this failed turn"),
        "{refused}"
    );
}

#[test]
fn pm_n_on_a_contribution_s_ruling_is_refused_since_no_only_clears_it() {
    let (_rig, runner) = super::with_pm("asked-take-contribution");
    let mut runner = runner.lock().unwrap();
    let kind = RulingKind::Contribution(Held::Merge {
        head: "abc".into(),
        rework_off: false,
    });
    runner.state.rulings.push(Ruling {
        approaches: vec!["Wait for the author".into()],
        ..Ruling::new(ticket(71), "Merge it?", kind)
    });

    let refused = runner.approach(ticket(71), 1).unwrap_err();
    assert!(
        refused.contains("`71 no` would only clear a contribution's ruling"),
        "{refused}"
    );
}

#[test]
fn a_second_recommendation_replaces_the_first() {
    let (rig, runner) = merge_put("asked-recommend-twice", true);
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::Say(vote('A', "FIRST-WHY")),
            Scripted::Say(vote('B', "SECOND-WHY")),
        ],
    );
    replied(&rig, &runner, "71 pm recommend", |c| {
        c.contains("FIRST-WHY")
    });
    rig.clock.advance(5);
    replied(&rig, &runner, "71 pm recommend", |c| {
        c.contains("SECOND-WHY")
    });

    let question = runner.lock().unwrap().state.rulings[0].question.clone();
    assert_eq!(question.matches(RECOMMENDS).count(), 1, "{question}");
    assert!(question.contains("SECOND-WHY"), "{question}");
    assert!(!question.contains("FIRST-WHY"), "{question}");
}
