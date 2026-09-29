//! A review bot round with a stand-in bot, through the runner's stand-ins

use std::sync::Arc;

use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Checks, Finding, Role, Severity, Timestamp};
use crate::review_bot::{Activity, Login, Profile, Reading, Review, Thread};
use crate::runner::coderabbit::tests::now;
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted, Told};

const HOLDS: &str = r#"{"holds": true, "severity": "medium", "reason": "real"}"#;

// A bot unlike CodeRabbit in every part its profile names: its own login,
// label and lease, no full review, and its severity in brackets.
#[derive(Debug)]
struct StandIn;

impl Profile for StandIn {
    fn name(&self) -> &str {
        "Stand-in"
    }

    fn login(&self) -> Login<'_> {
        Login {
            rest: "stand-in[bot]",
            graphql: "stand-in",
        }
    }

    fn lease(&self) -> LeaseKind {
        LeaseKind::try_from("stand-in").unwrap()
    }

    fn label(&self) -> &str {
        "stand-in please"
    }

    fn full_review(&self) -> Option<&str> {
        None
    }

    fn read(&self, activity: &Activity, head: &str, _since: Timestamp) -> Reading {
        if self.covers(activity, head) {
            Reading::Reviewed
        } else {
            Reading::Silent
        }
    }

    fn heard(&self, activity: &Activity, _head: &str, since: Timestamp) -> bool {
        activity.reviews.iter().any(|r| r.at >= since)
    }

    fn covers(&self, activity: &Activity, head: &str) -> bool {
        activity.reviews.iter().any(|r| r.commit == head)
    }

    fn reviewed_besides(&self, activity: &Activity, head: &str) -> u32 {
        let others = activity.reviews.iter().filter(|r| r.commit != head);
        u32::try_from(others.count()).unwrap()
    }

    fn quota(&self, _activity: &Activity) -> Option<(u32, Timestamp)> {
        None
    }

    fn finding(&self, thread: &Thread) -> Finding {
        let (label, rest) = thread.body.split_once("] ").unwrap();
        let (what, why) = rest.split_once("\n\n").unwrap();
        let severity = match label {
            "[high" => Severity::High,
            _ => Severity::Low,
        };
        Finding {
            severity,
            file: thread.path.clone(),
            line: thread.line.unwrap_or(0),
            what: what.to_owned(),
            why: why.to_owned(),
        }
    }
}

fn stand_in() -> LeaseKind {
    StandIn.lease()
}

#[test]
fn a_stand_in_bot_runs_a_whole_round_through_the_judge_to_the_worker() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    let runner = rig.open_with(Arc::new(StandIn)).unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    let summon = now(&rig);
    assert_eq!(
        rig.forge.coderabbit.label_log(),
        [(71, "stand-in please".to_owned(), true)]
    );
    assert!(rig.leases.held(&stand_in()));

    let review = Review {
        commit: head.clone(),
        body: "One thing.".into(),
        at: Timestamp(summon + 60),
    };
    let thread = Thread {
        id: "T_1".into(),
        resolved: false,
        path: "work.txt".into(),
        line: Some(1),
        body: "[high] Close the file.\n\nIt leaks a handle.".into(),
    };
    rig.forge.coderabbit.post(71, review, &[thread]);
    rig.clock.advance(60);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            issue: 7,
            pull_request: 71,
            round: 1,
            open_threads: 1
        })
    );
    assert!(
        !rig.leases.held(&stand_in()),
        "the review answered the summon"
    );
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summon))
    );

    rig.claude.script([Scripted::Text(HOLDS)]);
    step(&runner).unwrap();
    let calls = rig.claude.all_calls();
    let judged = calls.iter().rfind(|c| c.role == Role::Judge).unwrap();
    assert!(
        judged.prompt.contains(
            "severity: HIGH\nlocation: work.txt:1\nwhat: Close the file.\nwhy: It leaks a handle."
        ),
        "{}",
        judged.prompt
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitJudged { held: 1, .. })
    ));

    rig.claude.script([Scripted::Push("work.txt", "closed\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("Stand-in round 1 on your pull request #71 left 1 finding(s) that hold"),
        "{}",
        fix.prompt
    );
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(file.contains("Close the file."), "{file}");
    assert!(
        rig.leases.told().iter().all(
            |t| !matches!(t, Told::Want(k) | Told::Return(k) if *k == LeaseKind::coderabbit())
        ),
        "CodeRabbit's window is never touched"
    );
}
