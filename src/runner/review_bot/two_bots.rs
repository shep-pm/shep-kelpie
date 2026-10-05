//! Rounds with CodeRabbit and cubic both listed, through the runner's stand-ins

use std::num::NonZeroU32;
use std::sync::Mutex;

use serde_json::json;

use crate::coderabbit::LABEL;
use crate::cubic::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Checks, Leases, PullRequestState, Timestamp};
use crate::review_bot::{Bot, ReviewWindow, Reviewers};
use crate::runner::coderabbit::tests::now;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told};

const CUBIC_WINDOW: &str = "\n[reviewers.cubic]\nreviews = 20\nhours = 720\n";
pub(super) const CODEX_WINDOW: &str = "\n[reviewers.codex]\nreviews = 10\nhours = 168\n";
const MONTH: u64 = 720 * 3600;

pub(super) fn cr() -> LeaseKind {
    LeaseKind::coderabbit()
}

fn cubic() -> LeaseKind {
    Bot::Cubic.lease()
}

// A project listing `list`, with cubic defined in kelpie's own settings.
pub(super) fn listing(project: &str, list: &str) -> Rig {
    let rig = Rig::new(project);
    rig.coderabbit_on();
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!("{kelpie}{CUBIC_WINDOW}{CODEX_WINDOW}"));
    if !list.is_empty() {
        let line = format!("[app.dogs.kelpie]\npull_request_reviewers = {list}\n");
        rig.edit_settings(|s| s.replacen("[app.dogs.kelpie]\n", &line, 1));
    }
    rig
}

// Pull request 71 with the review done, green CI, and the draft
// still a draft.
pub(super) fn green(rig: &Rig) -> (Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    let head = item_green(rig, &runner, 7);
    (runner, head)
}

// Pull request 71 with the review done, green CI, and the
// draft marked ready: the next step summons.
pub(super) fn ready(rig: &Rig) -> (Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    let head = item_ready(rig, &runner, 7);
    (runner, head)
}

// Issue `issue`'s pull request, numbered ten times it plus one, brought to
// the same point.
fn item_ready(rig: &Rig, runner: &Mutex<Runner>, issue: u64) -> String {
    let head = item_green(rig, runner, issue);
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::MarkedReady { .. })
    ));
    head
}

// Issue `issue`'s pull request with its review rounds done and CI green on
// its head: the next step is the review bot's round, which `verdict` takes.
pub(super) fn item_green(rig: &Rig, runner: &Mutex<Runner>, issue: u64) -> String {
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
    let head = rig.forge.head_of(&branch).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    head
}

// Rereads the project's list as `list`, as a settings change reaching the
// runner does.
fn relist(rig: &Rig, runner: &Mutex<Runner>, from: &str, to: &str) {
    rig.edit_settings(|s| s.replace(from, to));
    let mut runner = runner.lock().unwrap();
    runner
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
}

pub(super) fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    let log = rig.forge.coderabbit.label_log();
    log.into_iter().filter(|(_, l, _)| l == LABEL).collect()
}

fn summons_by_comment(rig: &Rig) -> usize {
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

#[test]
fn coderabbits_window_busy_sends_the_round_to_cubic_whose_finding_reaches_the_worker_with_its_level()
 {
    let rig = listing("shep", r#"["coderabbit", "cubic"]"#);
    rig.leases.close(&cr(), true);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(summons_by_comment(&rig), 1);
    assert_eq!(labels(&rig), [], "CodeRabbit is not summoned");
    assert_eq!(rig.leases.told(), [Told::Want(cr()), Told::Want(cubic())]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["bot"], "cubic");
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
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            issue: 7,
            pull_request: 71,
            round: 1,
            open_threads: 1
        })
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSent { held: 1, .. })
    ));
    let file = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        file.contains("MEDIUM|work.txt:1|`bleats` loses a stamped prefix. Strip only files shep stamped.|cubic rates it P2, with confidence 8 of 10."),
        "{file}"
    );
    rig.claude
        .script([Scripted::Push("work.txt", "stripped\n")]);
    step(&runner).unwrap();
    let fix = rig.claude.calls().pop().unwrap();
    assert!(
        fix.prompt
            .starts_with("cubic round 1 on your pull request #71"),
        "{}",
        fix.prompt
    );
}

#[test]
fn both_windows_busy_waits_and_the_first_to_free_up_takes_the_round() {
    let rig = listing("shep", r#"["coderabbit", "cubic"]"#);
    rig.leases.close(&cr(), true);
    rig.leases.close(&cubic(), true);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(step(&runner).unwrap(), None);
    let asked = [Told::Want(cr()), Told::Want(cubic())];
    assert_eq!(rig.leases.told(), [asked.clone(), asked].concat());
    assert_eq!((labels(&rig), summons_by_comment(&rig)), (vec![], 0));

    rig.leases.grant(&cr());
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(summons_by_comment(&rig), 0);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"].get("bot"),
        None,
        "CodeRabbit's"
    );
    assert_eq!(status["leases"][0]["resource"], "coderabbit");
}

#[test]
fn a_cubic_refusal_closes_its_window_in_the_dogs_book_and_the_next_round_goes_elsewhere() {
    let rig = listing("shep", r#"["cubic", "coderabbit"]"#);
    let month = ReviewWindow {
        reviews: NonZeroU32::new(20).unwrap(),
        hours: NonZeroU32::new(720).unwrap(),
    };
    let dog = Reviewers {
        cubic: Some(month),
        ..Reviewers::default()
    };
    rig.leases.use_book(rig.clock.clone(), "shep", dog);
    rig.leases.window(&cr(), WindowFact::Summoned, now(&rig));
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge.coderabbit.cubic_refuse(71, summon + 20);
    rig.clock.advance(20);
    let opens = Timestamp(summon + 20 + MONTH);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::SummonRefused {
            issue: 7,
            pull_request: 71,
            opens,
        })
    );
    assert!(!rig.leases.held(&cubic()));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["stage"],
        "lease"
    );
    for _ in 0..3 {
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), None, "both windows closed");
    }
    assert_eq!(summons_by_comment(&rig), 1, "no second summon of cubic");

    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(summons_by_comment(&rig), 1);
}

// The runner's own settings define cubic, but the dog started before they
// did, so its book has no window for cubic.
#[test]
fn a_bot_the_dogs_book_does_not_define_is_never_granted() {
    let rig = listing("shep", r#"["cubic", "coderabbit"]"#);
    rig.leases
        .use_book(rig.clock.clone(), "shep", Reviewers::default());
    rig.leases.window(&cr(), WindowFact::Summoned, now(&rig));
    let (runner, head) = ready(&rig);
    for _ in 0..3 {
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), None);
    }
    assert!(!rig.leases.held(&cubic()));
    assert_eq!(summons_by_comment(&rig), 0);

    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(summons_by_comment(&rig), 0);
}

#[test]
fn a_project_that_lists_none_keeps_coderabbit_alone() {
    let rig = listing("shep", "");
    rig.leases.close(&cr(), true);
    let (runner, _) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.leases.told(), [Told::Want(cr())]);
    assert_eq!(summons_by_comment(&rig), 0);
}

#[test]
fn a_listed_reviewer_kelpie_does_not_define_stops_the_runner() {
    let rig = listing("shep", r#"["coderabbit", "cubic"]"#);
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&kelpie.replace(CUBIC_WINDOW, ""));
    let err = rig.open().unwrap_err().to_string();
    assert_eq!(
        err,
        "setting `pull_request_reviewers`: cubic is not defined: kelpie's own \
         settings need a [reviewers.cubic] table"
    );
}

#[test]
fn a_private_repo_listing_cubic_alone_starts_and_rounds_go_to_cubic() {
    let rig = listing("shep", r#"["cubic"]"#);
    rig.forge.set_visibility(crate::ports::Visibility::Private);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(rig.leases.told(), [Told::Want(cubic())]);
    assert_eq!((labels(&rig), summons_by_comment(&rig)), (vec![], 1));
}

// An adopted pull request CodeRabbit read before owes a summon. cubic's
// review settles it, and the cap counts both bots' reviews.
#[test]
fn a_cubic_round_settles_an_adopted_pull_requests_owed_summon() {
    let rig = listing("shep", r#"["coderabbit", "cubic"]"#);
    let reviewed = rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge
        .coderabbit
        .review(80, &reviewed, Rig::EPOCH - 120, &[]);
    rig.forge
        .coderabbit
        .cubic_review(80, &reviewed, Rig::EPOCH - 60, &[]);
    let head = rig.push_by_hand("fix/timeline", "more.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.ready_pull_request(80);
    rig.leases.close(&cr(), true);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "adopt", Some("80"));
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["coderabbit"]["rounds"], 2,
        "one by each bot"
    );
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Summoned {
            pull_request: 80,
            ..
        })
    ));
    assert_eq!(summons_by_comment(&rig), 1);

    let summon = now(&rig);
    rig.forge
        .coderabbit
        .cubic_review(80, &head, summon + 300, &[]);
    rig.clock.advance(300);
    // An owed summon still standing would summon again instead.
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitSatisfied {
            issue: 5,
            pull_request: 80,
            rounds: 3,
        })
    );
    assert_eq!(summons_by_comment(&rig), 1);
    assert_eq!(labels(&rig), []);
}

// cubic's lease row must go with the work item even when the list dropped
// cubic mid-round, or it blocks cubic for every item once listed again.
#[test]
fn a_bot_dropped_mid_round_gives_its_lease_back_when_the_work_item_ends() {
    let (both, just_coderabbit) = (r#"["coderabbit", "cubic"]"#, r#"["coderabbit"]"#);
    let rig = listing("shep", both);
    rig.leases.close(&cr(), true);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    relist(&rig, &runner, both, just_coderabbit);

    rig.forge.set_state(71, PullRequestState::Merged);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { .. })
    ));
    assert_eq!(rig.ask(&runner, "status", None)["leases"], json!([]));
    assert!(!rig.leases.held(&cubic()));

    relist(&rig, &runner, just_coderabbit, both);
    let head = item_ready(&rig, &runner, 8);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned {
            issue: 8,
            pull_request: 81,
            head,
        })
    );
    assert_eq!(
        summons_by_comment(&rig),
        2,
        "cubic took the second round too"
    );
}

// CodeRabbit left a thread open on an older head, and cubic takes the next
// round: that thread goes to the worker too before the round is satisfied.
#[test]
fn the_other_bots_open_threads_go_to_the_worker_with_the_rounds_own() {
    let rig = listing("shep", r#"["coderabbit", "cubic"]"#);
    rig.leases.close(&cr(), true);
    rig.forge
        .coderabbit
        .review(71, "0lderhead", Rig::EPOCH, &["Close the file."]);
    let (runner, head) = ready(&rig);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    let summon = now(&rig);
    rig.forge
        .coderabbit
        .cubic_review(71, &head, summon + 300, &[]);
    rig.clock.advance(300);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitReviewed {
            issue: 7,
            pull_request: 71,
            round: 1,
            open_threads: 1
        })
    );
}

// Reading CodeRabbit fails while its window is closed: cubic still takes
// the round.
#[test]
fn a_first_bot_that_cannot_be_read_leaves_the_round_to_the_next() {
    let rig = listing("shep", r#"["coderabbit", "cubic"]"#);
    rig.leases.close(&cr(), true);
    let (runner, head) = ready(&rig);
    rig.forge.coderabbit.set_down(true);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(summons_by_comment(&rig), 1);
}
