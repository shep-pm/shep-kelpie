//! Rounds with CodeRabbit and cubic both listed, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::coderabbit::LABEL;
use crate::cubic::SUMMON;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Checks, Role, Timestamp};
use crate::review_bot::Bot;
use crate::runner::coderabbit::tests::now;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, Told};

const HOLDS: &str = r#"{"holds": true, "severity": "medium", "reason": "real"}"#;
const CUBIC_WINDOW: &str = "\n[reviewers.cubic]\nreviews = 20\nhours = 720\n";
const MONTH: u64 = 720 * 3600;

fn cr() -> LeaseKind {
    LeaseKind::coderabbit()
}

fn cubic() -> LeaseKind {
    Bot::Cubic.lease()
}

// A project listing `list`, with cubic defined in kelpie's own settings.
fn listing(project: &str, list: &str) -> Rig {
    let rig = Rig::new(project);
    rig.coderabbit_on();
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!("{kelpie}{CUBIC_WINDOW}"));
    if !list.is_empty() {
        let line = format!("[app.dogs.kelpie]\npull_request_reviewers = {list}\n");
        rig.edit_settings(|s| s.replacen("[app.dogs.kelpie]\n", &line, 1));
    }
    rig
}

// Pull request 71 with the qwen-review loop settled, green CI, and the
// draft marked ready: the next step summons.
fn ready(rig: &Rig) -> (Mutex<Runner>, String) {
    let runner = rig.open().unwrap();
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
    (runner, head)
}

fn labels(rig: &Rig) -> Vec<(u64, String, bool)> {
    let log = rig.forge.coderabbit.label_log();
    log.into_iter().filter(|(_, l, _)| l == LABEL).collect()
}

fn summons_by_comment(rig: &Rig) -> usize {
    let comments = rig.forge.comments();
    comments.iter().filter(|(_, body)| body == SUMMON).count()
}

fn summoned(head: &str) -> Option<StepReport> {
    Some(StepReport::Summoned {
        issue: 7,
        pull_request: 71,
        head: head.to_owned(),
    })
}

#[test]
fn coderabbits_window_busy_sends_the_round_to_cubic_whose_finding_reaches_the_judge_with_its_level()
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
    rig.claude.script([Scripted::Text(HOLDS)]);
    step(&runner).unwrap();
    let calls = rig.claude.all_calls();
    let judged = calls.iter().rfind(|c| c.role == Role::Judge).unwrap();
    assert!(
        judged.prompt.contains(
            "severity: MEDIUM\nlocation: work.txt:1\nwhat: `bleats` loses a stamped prefix. \
             Strip only files shep stamped.\nwhy: cubic rates it P2, with confidence 8 of 10."
        ),
        "{}",
        judged.prompt
    );
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::CodeRabbitJudged { held: 1, .. })
    ));
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
fn a_cubic_refusal_parks_it_for_its_window_and_the_next_round_goes_elsewhere() {
    let rig = listing("shep", r#"["cubic", "coderabbit"]"#);
    rig.leases.close(&cr(), true);
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
    assert!(
        rig.leases
            .told()
            .contains(&Told::Window(WindowFact::Opens, opens.0))
    );
    assert!(!rig.leases.held(&cubic()));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["phase"]["stage"],
        "lease"
    );

    // The dog holds cubic's window closed for the month it was told.
    rig.leases.close(&cubic(), true);
    rig.leases.close(&cr(), false);
    assert_eq!(step(&runner).unwrap(), summoned(&head));
    assert_eq!(labels(&rig), [(71, LABEL.to_owned(), true)]);
    assert_eq!(summons_by_comment(&rig), 1, "cubic's first summon alone");
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
