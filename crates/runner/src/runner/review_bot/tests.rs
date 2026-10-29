//! A review bot's round with a stand-in bot, through the runner's stand-ins

use std::sync::{Arc, Mutex};

use super::late::LATE_READ;
use super::late_reads::passed_over;
use super::two_bots::listing;
use super::{DONE_SETTLE, HEARD_WAIT};
use crate::adapters::ShepLeases;
use crate::lease::saved::BookFile;
use crate::lease::wire::{Asker, WindowFact};
use crate::lease::{Epoch, LeaseKind};
use crate::ports::Leases;
use crate::ports::{CLEAN, Checks, Finding, Severity, Timestamp};
use crate::review_bot::{Activity, Bot, Comment, Login, Profile, Reading, Review, Status, Thread};
use crate::runner::coderabbit::tests::now;
use crate::runner::{CHECKS_SETTLE, Pass, Runner, StepReport, advance, step};
use crate::settings::AgentName;
use crate::test::ticket;
use crate::test::{DogStandIn, Rig, Scripted, Told};

const LABEL: &str = "stand-in please";
const LOGIN: &str = "stand-in[bot]";

// A bot unlike CodeRabbit in every part its profile names, though it takes
// CodeRabbit's place in the list: its own login, label and lease, no full
// review, a refusal that names its opening, a "done" status, and its
// severity in brackets.
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
        Finding::new(
            severity,
            thread.path.clone(),
            thread.line.unwrap_or(0),
            what.to_owned(),
            why.to_owned(),
        )
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

// Pull request 71 with the stand-in listed after the qwen and Claude
// rounds, which read it clean, the draft marked ready, and the stand-in
// summoned.
fn summoned(project: &str) -> (Rig, Mutex<Runner>, String) {
    summoned_in(Rig::new(project))
}

fn summoned_in(rig: Rig) -> (Rig, Mutex<Runner>, String) {
    rig.coderabbit_on();
    rig.allow(&["sonnet-high", "qwen", "claude", "coderabbit"]);
    let runner = rig.open_with(vec![Arc::new(StandIn)]).unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn: opens the pull request
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: scripted clean above
    let head = rig.forge.head_of("kelpie/7").unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert!(rig.leases.held(&stand_in()));
    (rig, runner, head)
}

// The runner's second image after a handover, whose first asked for the
// stand-in's lease twice, so its dog has heard two. The dog acts only on a
// total above the last it heard.
#[test]
fn a_bot_round_after_a_handover_gets_its_lease_and_summons() {
    let rig = Rig::new("shep");
    let (dog, epoch) = (Arc::new(DogStandIn::default()), Epoch(4242));
    let book = BookFile::new(rig.home.path().join("book.json"));
    let before = ShepLeases::new(dog.clone(), Asker::new(epoch), book.clone());
    for _ in 0..2 {
        before.want(&stand_in());
        before.give_back(&stand_in());
    }
    // Its grants went to the first image.
    dog.take_grants();
    let asker = Asker::resume(epoch, before.keep());
    let after = ShepLeases::new(dog.clone(), asker, book);
    rig.leases.behind_a_dog(dog, Arc::new(after));

    let (rig, _runner, _head) = summoned_in(rig);

    assert!(rig.leases.held(&stand_in()), "the dog granted the lease");
}

// The runner tells the dog of its ask, its summons and its return as
// notes, which shep's metrics dog never reads, under the names the dog
// reads.
#[test]
fn a_bot_round_s_ask_summons_and_return_reach_the_dog_as_notes() {
    let rig = Rig::new("shep");
    let (dog, epoch) = (Arc::new(DogStandIn::default()), Epoch(4242));
    let book = BookFile::new(rig.home.path().join("book.json"));
    let leases = ShepLeases::new(dog.clone(), Asker::new(epoch), book);
    rig.leases.behind_a_dog(dog.clone(), Arc::new(leases));
    let (rig, runner, head) = summoned_in(rig);
    let summon = now(&rig);
    rig.forge.coderabbit.post_as(71, LOGIN, |seen| {
        seen.reviews.push(review(&head, summon + 60));
    });
    rig.clock.advance(60);
    rig.threads_read(&runner);

    assert_eq!(
        dog.notes(),
        [
            ("lease.stand-in.want.4242".to_owned(), 1.0),
            ("window.stand-in.summoned.4242".to_owned(), summon as f64),
            ("lease.stand-in.return.4242".to_owned(), 1.0),
        ]
    );
}

#[test]
fn a_stand_in_bot_runs_a_whole_round_through_to_the_worker() {
    let (rig, runner, head) = summoned("shep");
    let summon = now(&rig);
    let thread = Thread {
        id: "T_1".into(),
        resolved: false,
        outdated: false,
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
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed {
            issue: ticket(7),
            pull_request: 71,
            round: 3,
            reviewer: AgentName::kelpies("coderabbit"),
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

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        file.contains("HIGH|work.txt:1|Close the file.|It leaks a handle."),
        "{file}"
    );

    rig.claude.script([Scripted::Push("work.txt", "closed\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("Round 3 of the review on your pull request #71 found 1 finding(s)"),
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
            issue: ticket(7),
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
            issue: ticket(7),
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
    rig.allow(&["sonnet-high", "qwen", "claude", "coderabbit"]);
    let reviewed = rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge.coderabbit.post_as(80, LOGIN, |seen| {
        seen.reviews.push(review(&reviewed, Rig::EPOCH - 3600))
    });
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.ready_pull_request(80);
    let runner = rig.open_with(vec![Arc::new(StandIn)]).unwrap();
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
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
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed {
            open_threads: 0,
            ..
        })
    ));
    assert_eq!(
        labels(&rig),
        [(80, LABEL.to_owned(), true), (80, LABEL.to_owned(), false)],
        "summoned once"
    );
    assert_eq!(rig.forge.comments(), []);
    read_as_stand_in(&rig);
}

fn notes(runner: &Mutex<Runner>) -> Vec<String> {
    runner.lock().unwrap().take_notes()
}

// Pull request 71 with the stand-in listed after the qwen and Claude rounds,
// the draft marked ready, and the dog granting no lease.
fn ready_without_a_lease(rig: &Rig) -> Mutex<Runner> {
    rig.coderabbit_on();
    rig.allow(&["sonnet-high", "qwen", "claude", "coderabbit"]);
    let runner = rig.open_with(vec![Arc::new(StandIn)]).unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    for _ in 0..4 {
        step(&runner).unwrap(); // the worker, qwen, Claude, then the draft marked ready
    }
    rig.leases.withhold(true);
    notes(&runner);
    runner
}

#[test]
fn a_round_waiting_for_a_lease_says_so_once_and_says_the_wait_when_it_is_granted() {
    let rig = Rig::new("shep");
    let runner = ready_without_a_lease(&rig);

    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None, "no lease, no summon");
        rig.clock.advance(60);
    }
    assert_eq!(
        notes(&runner),
        ["#7 lease-wait kind=stand-in reviewer=coderabbit"]
    );

    rig.leases.withhold(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    assert_eq!(
        notes(&runner),
        ["#7 lease-granted kind=stand-in reviewer=coderabbit waited=180s"]
    );
}

#[test]
fn a_lease_granted_at_the_first_ask_says_nothing() {
    let (_rig, runner, _head) = summoned("shep");

    assert_eq!(notes(&runner), Vec::<String>::new());
}

#[test]
fn a_round_that_gives_up_on_the_lease_says_the_wait_it_gave_up_after() {
    let rig = Rig::new("shep");
    let runner = ready_without_a_lease(&rig);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(2 * 60 * 60);

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { .. })
    ));
    assert_eq!(
        notes(&runner),
        [
            "#7 lease-wait kind=stand-in reviewer=coderabbit",
            "#7 lease-refused kind=stand-in reviewer=coderabbit waited=7200s",
        ]
    );
}

// The pass reads the bot's activity with the runner's lock let go, so a
// trigger is not made to wait the read's seconds out.
#[test]
fn a_trigger_is_answered_while_a_bots_activity_is_read() {
    let (rig, runner, _head) = summoned("shep");
    let read = rig.forge.hold_next_activity_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the trigger waited for the lock the activity's read held");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    pass.unwrap();
}

// A review that lands while the activity is read is what the step made
// again reads, so the round goes on to its threads as it would had the
// bot answered before the step began.
#[test]
fn a_bot_that_answered_during_the_read_is_seen_by_the_step_that_follows() {
    let (rig, runner, head) = summoned("shep");
    let summon = now(&rig);
    let read = rig.forge.hold_next_activity_read();

    let (_, pass) = rig.during_read(
        read,
        || advance(&runner),
        || {
            rig.forge.coderabbit.post_as(71, LOGIN, |seen| {
                seen.reviews.push(review(&head, summon + 60));
            });
            rig.clock.advance(60);
        },
    );

    pass.unwrap();
    assert_eq!(
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed {
            issue: ticket(7),
            pull_request: 71,
            round: 3,
            reviewer: AgentName::kelpies("coderabbit"),
            open_threads: 0
        })
    );
    assert!(
        !rig.leases.held(&stand_in()),
        "the review answered the summon"
    );
}

// A forge that fails the read is told as a failed gate, the way the same
// read made in place was, and the round goes on once it answers again.
#[test]
fn a_read_that_fails_is_handled_as_the_inline_read_was() {
    let (rig, runner, head) = summoned("shep");
    let read = rig.forge.hold_next_activity_read();

    let (_, pass) = rig.during_read(read, || advance(&runner), || rig.forge.set_down(true));

    let Pass::Report(StepReport::GateFailed { reason, .. }) = pass.unwrap() else {
        panic!("a failed read was not reported");
    };
    assert!(
        reason.starts_with("cannot read Stand-in on #71: "),
        "{reason}"
    );
    rig.forge.set_down(false);
    rig.next_look();
    let summon = now(&rig);
    rig.forge.coderabbit.post_as(71, LOGIN, |seen| {
        seen.reviews.push(review(&head, summon));
    });
    assert!(matches!(
        rig.threads_read(&runner),
        Some(StepReport::BotReviewed { .. })
    ));
}

// The activity of a bot passed over is read with the runner's lock let go,
// when CI turns green and while the merge ruling waits.
#[test]
fn a_trigger_is_answered_while_the_late_review_activity_is_read() {
    let rig = listing("shep", &["cubic"]);
    let (runner, head) = passed_over(&rig);
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None, "CI settles first");
    rig.clock.advance(CHECKS_SETTLE);
    let read = rig.forge.hold_next_activity_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("green CI held the lock for the activity's read");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    assert!(
        matches!(pass.unwrap(), Pass::Report(StepReport::Ruling { .. })),
        "no merge ruling"
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    rig.clock.advance(LATE_READ);
    let read = rig.forge.hold_next_activity_read();

    let (reply, pass) = rig.asked_while_held(&runner, read, "status", None);

    let reply = reply.expect("the parked work item held the lock for the activity's read");
    assert_eq!(reply["work_items"].as_array().unwrap().len(), 1);
    pass.unwrap();
}
