//! Passes with CodeRabbit and cubic both listed, each its own round in its
//! place, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::coderabbit::{FULL_REVIEW, LABEL};
use crate::cubic::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Leases, PullRequestState};
use crate::review_bot::{Bot, Reviewers};
use crate::runner::coderabbit::tests::now;
use crate::runner::{Runner, StepReport, step};
use crate::settings::AgentName;
use crate::test::{Rig, Scripted, Told};

const MONTH: u64 = 720 * 3600;

pub(super) fn cr() -> LeaseKind {
    LeaseKind::coderabbit()
}

fn cubic() -> LeaseKind {
    Bot::Cubic.lease()
}

// A project listing the rig's qwen and Claude rounds, then `bots`.
pub(super) fn listing(project: &str, bots: &[&str]) -> Rig {
    let rig = Rig::new(project);
    let names: Vec<&str> = ["qwen", "claude"]
        .into_iter()
        .chain(bots.iter().copied())
        .collect();
    rig.reviewers(&names);
    rig
}

// Pull request 71 read clean by the qwen and Claude rounds: the next step is
// the first listed bot's.
pub(super) fn reviewed(rig: &Rig) -> (Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    let head = item_reviewed(rig, &runner, 7);
    (runner, head)
}

// Issue `issue`'s pull request, numbered ten times it plus one, brought to
// the same point.
pub(super) fn item_reviewed(rig: &Rig, runner: &Mutex<Runner>, issue: u64) -> String {
    let branch = format!("kelpie/{issue}");
    rig.ask(runner, "add", Some(&issue.to_string()));
    rig.forge
        .open_pull_request(issue * 10 + 1, &branch, &[issue]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(runner).unwrap(); // the worker's first turn: opens the pull request
    step(runner).unwrap(); // review round 1, qwen: clean by default
    step(runner).unwrap(); // review round 2, claude: scripted clean above
    rig.forge.head_of(&branch).unwrap()
}

// Rereads the project's list as `to`, as a settings change reaching the
// runner does.
fn relist(rig: &Rig, runner: &Mutex<Runner>, from: &[&str], to: &[&str]) {
    let line = |names: &[&str]| {
        let all: Vec<&str> = ["qwen", "claude"]
            .into_iter()
            .chain(names.iter().copied())
            .collect();
        format!("reviewers = {all:?}\n")
    };
    rig.edit_settings(|s| s.replace(&line(from), &line(to)));
    let mut runner = runner.lock().unwrap();
    runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
}

pub(super) fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    let log = rig.forge.coderabbit.label_log();
    log.into_iter().filter(|(_, l, _)| l == LABEL).collect()
}

pub(super) fn summons_by_comment(rig: &Rig) -> usize {
    comments_of(rig, SUMMON)
}

// How many of the runner's comments are exactly `text`.
pub(super) fn comments_of(rig: &Rig, text: &str) -> usize {
    let comments = rig.forge.comments();
    comments.iter().filter(|(_, body)| body == text).count()
}

pub(super) fn summoned(head: &str) -> Option<StepReport> {
    Some(StepReport::Summoned {
        issue: 7,
        pull_request: 71,
        head: head.to_owned(),
    })
}

pub(super) fn bot_reviewed(round: u32, bot: &'static str, open: usize) -> Option<StepReport> {
    Some(StepReport::BotReviewed {
        issue: 7,
        pull_request: 71,
        round,
        reviewer: AgentName::kelpies(bot),
        open_threads: open,
    })
}

#[test]
fn each_listed_bot_reads_in_its_own_round_and_cubics_finding_reaches_the_worker_with_its_level() {
    let rig = listing("shep", &["coderabbit", "cubic"]);
    let (runner, head) = reviewed(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(
        rig.leases.told(),
        [Told::Want(cr())],
        "cubic waits its turn"
    );
    rig.forge.coderabbit.review(71, &head, now(&rig) + 60, &[]);
    rig.clock.advance(60);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(3, "coderabbit", 0));

    // cubic's round is the pass's fourth, on a pull request already ready.
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(summons_by_comment(&rig), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["round"], 4);
    assert_eq!(status["work_item"]["phase"]["stage"]["bot"], "cubic");
    assert_eq!(
        status["leases"],
        json!([{ "resource": "cubic", "issue": 7, "since": now(&rig) }])
    );

    let summon = now(&rig);
    rig.forge.coderabbit.cubic_start(71, summon + 6);
    rig.clock.advance(6);
    assert_eq!(step(&runner).unwrap(), None, "started, not yet reviewed");
    assert!(!rig.leases.held(&cubic()), "its start answered the summon");
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Summoned, summon))
    );

    let finding = "P2: `bleats` loses a stamped prefix. Strip only files shep stamped.";
    rig.forge
        .coderabbit
        .cubic_review(71, &head, summon + 420, &[finding]);
    rig.clock.advance(420);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(4, "cubic", 1));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent {
            round: 4,
            held: 1,
            ..
        })
    ));
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        file.contains("MEDIUM|work.txt:1|`bleats` loses a stamped prefix. Strip only files shep stamped.|cubic rates it P2, with confidence 8 of 10."),
        "{file}"
    );
    rig.claude
        .script([Scripted::Push("work.txt", "stripped\n")]);
    step(&runner).unwrap();
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FixPushed { round: 4, .. })
    ));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["bot_reads"],
        json!({ "coderabbit": 1, "cubic": 1 })
    );
}

#[test]
fn a_busy_window_holds_its_bots_round_and_the_next_bot_waits_its_turn() {
    let rig = listing("shep", &["coderabbit", "cubic"]);
    rig.leases.close(&cr(), true);
    let (runner, head) = reviewed(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.leases.told(), [Told::Want(cr()), Told::Want(cr())]);
    assert_eq!((labels(&rig), summons_by_comment(&rig)), (vec![], 0));

    rig.leases.grant(&cr());
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(summons_by_comment(&rig), 0);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["stage"]["bot"], "coderabbit");
    assert_eq!(status["leases"][0]["resource"], "coderabbit");
}

#[test]
fn a_cubic_refusal_closes_its_window_for_its_span_and_the_pass_goes_on_to_coderabbit() {
    let rig = listing("shep", &["cubic", "coderabbit"]);
    let (runner, head) = reviewed(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.cubic_refuse(71, summon + 20);
    rig.clock.advance(20);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped {
            issue: 7,
            pull_request: 71,
            round: 3,
            reviewer: AgentName::kelpies("cubic"),
            reason: "its window opens more than an hour on".into(),
        })
    );
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Opens, summon + 20 + MONTH)),
        "its file's month, since cubic quotes no opening"
    );
    assert!(!rig.leases.held(&cubic()));

    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(summons_by_comment(&rig), 1, "no second summon of cubic");
}

// A dog whose book has no window for cubic never grants it, and the round
// waits on the lease as any does.
#[test]
fn a_bot_the_dogs_book_does_not_define_is_never_granted() {
    let rig = listing("shep", &["cubic"]);
    rig.leases
        .use_book(rig.clock.clone(), "shep", Reviewers::default());
    let (runner, _) = reviewed(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    for _ in 0..3 {
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert!(!rig.leases.held(&cubic()));
    assert_eq!(summons_by_comment(&rig), 0);
}

#[test]
fn a_private_repo_listing_cubic_alone_starts_and_cubic_reads() {
    let rig = listing("shep", &["cubic"]);
    rig.forge.set_visibility(crate::ports::Visibility::Private);
    let (runner, head) = reviewed(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(rig.leases.told(), [Told::Want(cubic())]);
    assert_eq!((labels(&rig), summons_by_comment(&rig)), (vec![], 1));
}

// An adopted pull request both bots read before: CodeRabbit's owed summon
// asks for a full review, and once it lands cubic owes nothing more than a
// read of its own.
#[test]
fn an_adopted_pull_request_gets_a_pass_of_the_listed_bots() {
    let rig = Rig::new("shep");
    rig.reviewers(&["qwen", "claude", "coderabbit", "cubic"]);
    let reviewed = rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge
        .coderabbit
        .review(80, &reviewed, Rig::EPOCH - 7200, &[]);
    rig.forge
        .coderabbit
        .cubic_review(80, &reviewed, Rig::EPOCH - 3600, &[]);
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.ready_pull_request(80);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["bot_reads"],
        json!({ "coderabbit": 1, "cubic": 1 }),
        "one by each bot"
    );
    assert_eq!(status["work_item"]["phase"]["bots_only"], true);
    let summoned_80 = Some(StepReport::Summoned {
        issue: 5,
        pull_request: 80,
        head: head.clone(),
    });
    assert_eq!(step(&runner).unwrap(), summoned_80);
    assert_eq!(comments_of(&rig, FULL_REVIEW), 1);
    let summon = now(&rig);
    rig.forge.coderabbit.review(80, &head, summon + 300, &[]);
    rig.clock.advance(300);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::BotReviewed { round: 1, .. })
    ));

    assert_eq!(step(&runner).unwrap(), summoned_80, "cubic's own read");
    assert_eq!(summons_by_comment(&rig), 1);
    rig.forge
        .coderabbit
        .cubic_review(80, &head, now(&rig) + 300, &[]);
    rig.clock.advance(300);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::BotReviewed { round: 2, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    assert_eq!(rig.reviewer.seen().len(), 0, "qwen never ran");
    assert_eq!(rig.claude.all_calls().len(), 0, "nor did the Claude round");
}

// cubic's lease row must go with the work item even when the list dropped
// cubic mid-round, or it blocks cubic for every item once listed again.
#[test]
fn a_bot_dropped_mid_round_gives_its_lease_back_when_the_work_item_ends() {
    let (both, just_coderabbit) = (["cubic", "coderabbit"], ["coderabbit"]);
    let rig = listing("shep", &both);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    relist(&rig, &runner, &both, &just_coderabbit);

    rig.forge.set_state(71, PullRequestState::Merged);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { .. })
    ));
    assert_eq!(rig.ask(&runner, "status", None)["leases"], json!([]));
    assert!(!rig.leases.held(&cubic()));

    relist(&rig, &runner, &just_coderabbit, &["cubic"]);
    let head = item_reviewed(&rig, &runner, 8);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 8,
            pull_request: 81,
            head,
        })
    );
    assert_eq!(summons_by_comment(&rig), 2, "cubic read the second too");
}

// CodeRabbit left a thread open on an older head: cubic's round sends only
// cubic's own threads.
#[test]
fn a_bots_round_sends_only_its_own_open_threads() {
    let rig = listing("shep", &["cubic"]);
    rig.forge
        .coderabbit
        .review(71, "0lderhead", Rig::EPOCH, &["Close the file."]);
    let (runner, head) = reviewed(&rig);
    step(&runner).unwrap(); // marks the draft ready
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge
        .coderabbit
        .cubic_review(71, &head, summon + 300, &[]);
    rig.clock.advance(300);
    assert_eq!(step(&runner).unwrap(), bot_reviewed(3, "cubic", 0));
}

// The dog's book closed cubic's window for a month: the pass goes on to
// CodeRabbit at once, with cubic never asked.
#[test]
fn a_month_long_window_in_the_dogs_book_skips_cubic_unasked() {
    let rig = listing("shep", &["cubic", "coderabbit"]);
    let dog = Reviewers::from_agents(&crate::agents::Agents::embedded());
    rig.leases.use_book(rig.clock.clone(), "shep", dog);
    rig.leases
        .window(&cubic(), WindowFact::Opens, now(&rig) + MONTH);
    let (runner, head) = reviewed(&rig);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewerSkipped { round: 3, .. })
    ));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert!(!rig.leases.told().contains(&Told::Want(cubic())));
    let skipped = &rig.ask(&runner, "status", None)["work_item"]["bots_skipped"];
    assert_eq!(
        skipped,
        &json!([{ "why": "window", "reviewer": "cubic", "opens": now(&rig) + MONTH }])
    );
}
