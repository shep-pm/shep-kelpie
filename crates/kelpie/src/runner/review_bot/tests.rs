//! Review bot rounds with a stand-in bot, through the runner's stand-ins

use std::sync::{Arc, Mutex};

use super::{DONE_SETTLE, HEARD_WAIT};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Checks, Finding, Role, Severity, Timestamp};
use crate::review_bot::{Activity, Bot, Comment, Login, Profile, Reading, Review, Status, Thread};
use crate::runner::coderabbit::tests::now;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told};

const HOLDS: &str = r#"{"holds": true, "severity": "medium", "reason": "real"}"#;
const LABEL: &str = "stand-in please";
const LOGIN: &str = "stand-in[bot]";

// A bot unlike CodeRabbit in every part its profile names: its own login,
// label and lease, no full review, a refusal that names its opening, a
// "done" status, and its severity in brackets.
#[derive(Debug)]
struct StandIn;

impl Profile for StandIn {
    fn bot(&self) -> Bot {
        Bot::Coderabbit
    }

    fn name(&self) -> &str {
        "Stand-in"
    }

    fn login(&self) -> Login<'_> {
        Login {
            rest: LOGIN,
            graphql: "stand-in",
        }
    }

    fn lease(&self) -> LeaseKind {
        LeaseKind::try_from("stand-in").unwrap()
    }

    fn label(&self) -> Option<&str> {
        Some(LABEL)
    }

    fn full_review(&self) -> Option<&str> {
        None
    }

    fn read(&self, activity: &Activity, head: &str, since: Timestamp) -> Reading {
        if self.covers(activity, head) {
            return Reading::Reviewed;
        }
        let opens = activity
            .comments
            .iter()
            .filter(|c| c.at >= since)
            .find_map(|c| c.body.strip_prefix("retry at ")?.parse().ok());
        if let Some(opens) = opens {
            return Reading::Refused {
                opens: Some(Timestamp(opens)),
            };
        }
        let done = activity
            .statuses
            .iter()
            .find(|s| s.commit == head && s.at >= since && s.description == "done");
        match done {
            Some(s) => Reading::Completed { at: s.at },
            None => Reading::Silent,
        }
    }

    fn heard(&self, activity: &Activity, head: &str, since: Timestamp) -> bool {
        activity.comments.iter().any(|c| c.at >= since)
            || activity.reviews.iter().any(|r| r.at >= since)
            || activity
                .statuses
                .iter()
                .any(|s| s.commit == head && s.at >= since)
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

fn review(head: &str, at: u64) -> Review {
    Review {
        commit: head.to_owned(),
        body: "Reviewed.".into(),
        at: Timestamp(at),
    }
}

fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    let log = rig.forge.coderabbit.label_log();
    log.into_iter().filter(|(_, l, _)| l == LABEL).collect()
}

// Every read of the bot's activity was by its own login, never CodeRabbit's.
fn read_as_stand_in(rig: &Rig) {
    let logins = rig.forge.coderabbit.logins();
    assert!(!logins.is_empty());
    assert!(logins.iter().all(|l| l == LOGIN), "{logins:?}");
}

// Pull request 71 with the stand-in on, the qwen-review loop settled, green
// CI, the draft marked ready, and the stand-in summoned.
fn summoned(project: &str) -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new(project);
    rig.coderabbit_on();
    let runner = rig.open_with(vec![Arc::new(StandIn)]).unwrap();
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
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert!(rig.leases.held(&stand_in()));
    (rig, runner, head)
}

#[test]
fn a_stand_in_bot_runs_a_whole_round_through_the_judge_to_the_worker() {
    let (rig, runner, head) = summoned("shep");
    let summon = now(&rig);
    let thread = Thread {
        id: "T_1".into(),
        resolved: false,
        path: "work.txt".into(),
        line: Some(1),
        body: "[high] Close the file.\n\nIt leaks a handle.".into(),
    };
    rig.forge.coderabbit.post_as(71, LOGIN, |seen| {
        seen.reviews.push(review(&head, summon + 60));
        seen.threads.push(thread);
    });
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
    read_as_stand_in(&rig);
}

#[test]
fn a_stand_in_refusal_reschedules_its_own_window_and_takes_its_label_off() {
    let (rig, runner, _) = summoned("shep");
    let summon = now(&rig);
    let opens = summon + 1800;
    rig.forge.coderabbit.post_as(71, LOGIN, |seen| {
        seen.comments.push(Comment {
            body: format!("retry at {opens}"),
            at: Timestamp(summon + 30),
        });
    });
    rig.clock.advance(30);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused {
            issue: 7,
            pull_request: 71,
            opens: Timestamp(opens),
        })
    );
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Opens, opens))
    );
    assert!(!rig.leases.held(&stand_in()));
    assert_eq!(
        labels(&rig),
        [(71, LABEL.to_owned(), true), (71, LABEL.to_owned(), false)]
    );
    read_as_stand_in(&rig);
}

#[test]
fn a_stand_in_summon_with_no_sign_goes_out_once_more_by_its_label() {
    let (rig, runner, head) = summoned("shep");
    rig.clock.advance(HEARD_WAIT);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SummonedAgain {
            issue: 7,
            pull_request: 71,
            head,
        })
    );
    let (on, off) = ((71, LABEL.to_owned(), true), (71, LABEL.to_owned(), false));
    assert_eq!(labels(&rig), [on.clone(), off, on]);
    assert_eq!(rig.forge.comments(), [], "no full review to ask for");
    assert!(rig.leases.held(&stand_in()), "the same summon's lease");
}

// An adopted pull request the stand-in reviewed before, so its summon is owed.
// With no full review to ask for, done with nothing posted is its answer.
#[test]
fn an_owed_stand_in_summon_marked_done_is_answered_not_summoned_again() {
    let rig = Rig::new("shep");
    rig.coderabbit_on();
    let reviewed = rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge.coderabbit.post_as(80, LOGIN, |seen| {
        seen.reviews.push(review(&reviewed, Rig::EPOCH - 60))
    });
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.ready_pull_request(80);
    let runner = rig.open_with(vec![Arc::new(StandIn)]).unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned { .. })
    ));
    let summon = now(&rig);

    rig.forge.coderabbit.post_as(80, LOGIN, |seen| {
        seen.statuses.insert(
            0,
            Status {
                commit: head.clone(),
                description: "done".into(),
                at: Timestamp(summon + 30),
            },
        );
    });
    rig.clock.advance(30 + DONE_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied { .. })
    ));
    assert_eq!(
        labels(&rig),
        [(80, LABEL.to_owned(), true), (80, LABEL.to_owned(), false)],
        "summoned once"
    );
    assert_eq!(rig.forge.comments(), []);
    read_as_stand_in(&rig);
}
