use super::super::CHECKS_SETTLE;
use super::super::tests::ruling_report;
use crate::ports::{Checks, Queued};
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted, ticket};

const RUN: u64 = 39_697_676_985;
const CAP: u64 = 60 * 60;

fn stuck(name: &str, run: u64) -> Queued {
    Queued {
        name: name.to_owned(),
        run: Some(run),
        running: false,
    }
}

fn windows_queued() -> Checks {
    Checks::Pending(vec![stuck("windows-gnu", RUN)])
}

#[test]
fn a_check_queued_past_the_cap_reruns_its_whole_run_once() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, windows_queued());
    assert_eq!(rig.verdict(&runner), None, "queued, within the cap");
    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());

    rig.clock.advance(CAP);
    step(&runner).unwrap();
    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);

    // The cap starts again from the rerun, and a head is rerun once.
    rig.clock.advance(CAP - CHECKS_SETTLE);
    step(&runner).unwrap();
    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);

    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(CHECKS_SETTLE);
    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
}

#[test]
fn a_check_still_queued_after_its_rerun_raises_a_ruling_naming_the_job() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, windows_queued());
    rig.verdict(&runner);
    rig.clock.advance(CAP);
    step(&runner).unwrap();

    rig.clock.advance(CAP);
    let (id, question) = ruling_report(step(&runner).unwrap());

    assert_eq!(id, ticket(71));
    assert_eq!(
        question,
        format!(
            "windows-gnu stayed queued on pull request #71 at {} after its run was rerun: \
             https://github.com/shep-pm/shep/actions/runs/{RUN}. `shep kelpie rule 71 yes` \
             reruns the run once more and kelpie waits again, and `shep kelpie rule 71 no \
             <note>` leaves it waiting for you to act on the forge.",
            &head[..7]
        )
    );
    assert_eq!(rig.forge.whole_run_reruns(), [RUN], "no second rerun");
}

fn parked_on_check_queued(rig: &Rig, runner: &std::sync::Mutex<crate::runner::Runner>, head: &str) {
    rig.forge.set_checks(head, windows_queued());
    rig.verdict(runner);
    rig.clock.advance(CAP);
    step(runner).unwrap();
    rig.clock.advance(CAP);
    ruling_report(step(runner).unwrap());
}

#[test]
fn a_yes_on_check_queued_reruns_the_run_and_waits_again() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    parked_on_check_queued(&rig, &runner, &head);

    rig.ask(&runner, "rule", Some("71 yes"));

    assert_eq!(rig.forge.whole_run_reruns(), [RUN, RUN]);
    assert_eq!(step(&runner).unwrap(), None, "it waits again");
    rig.clock.advance(CAP - 1);
    assert_eq!(step(&runner).unwrap(), None, "the cap starts from the yes");
    rig.forge.set_checks(&head, Checks::Passed);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_no_on_check_queued_waits_again_and_reruns_nothing() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    parked_on_check_queued(&rig, &runner, &head);

    rig.ask(&runner, "rule", Some("71 no it is stuck on the forge"));

    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);
    assert_eq!(step(&runner).unwrap(), None, "it waits again");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(CHECKS_SETTLE);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(question.starts_with("Merge pull request #71"), "{question}");
}

#[test]
fn a_check_queued_in_reviews_ci_wait_is_rerun_too() {
    let rig = Rig::new("shep");
    rig.ci_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, windows_queued());
    assert_eq!(step(&runner).unwrap(), None, "the wait starts");
    rig.clock.advance(CAP);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::CiRerun { ref head, .. }) if *head == head_of(&rig)),
        "{report:?}"
    );
    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);
    rig.clock.advance(CAP);
    let (id, question) = ruling_report(step(&runner).unwrap());
    assert_eq!(id, ticket(71));
    assert!(
        question.starts_with("windows-gnu stayed queued"),
        "{question}"
    );
    rig.ask(&runner, "rule", Some("71 yes"));
    assert_eq!(rig.forge.whole_run_reruns(), [RUN, RUN]);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "review", "{status}");
}

fn head_of(rig: &Rig) -> String {
    rig.forge.head_of("kelpie/7").unwrap()
}

#[test]
fn a_check_that_started_is_never_rerun_for_being_slow() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    // A pending check that is neither queued nor in progress, such as a commit
    // status, is in the rollup but not in the list.
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));
    rig.verdict(&runner);

    rig.clock.advance(10 * CAP);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());
}

#[test]
fn a_rerun_the_forge_refuses_waits_another_cap_and_tells_once() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, windows_queued());
    rig.verdict(&runner);
    rig.forge
        .set_rerun_error("run 39697676985 is still in progress");
    rig.clock.advance(CAP);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::GateFailed { .. })),
        "{report:?}"
    );
    assert_eq!(step(&runner).unwrap(), None, "the next poll only waits");
    rig.clock.advance(CAP - 1);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the cap started from the refusal"
    );
    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());
}

#[test]
fn each_queued_run_is_rerun_once_before_a_ruling_is_raised() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let both = Checks::Pending(vec![stuck("windows-gnu", RUN), stuck("docs", OTHER)]);
    rig.forge.set_checks(&head, both);
    rig.verdict(&runner);
    rig.clock.advance(CAP);
    step(&runner).unwrap();
    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);

    // The first run clears, and the second is still queued.
    rig.forge
        .set_checks(&head, Checks::Pending(vec![stuck("docs", OTHER)]));
    rig.clock.advance(CAP);
    let report = step(&runner).unwrap();
    assert!(
        matches!(report, Some(StepReport::CiRerun { .. })),
        "{report:?}"
    );
    assert_eq!(rig.forge.whole_run_reruns(), [RUN, OTHER]);

    rig.clock.advance(CAP);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(question.starts_with("docs stayed queued"), "{question}");
}

const OTHER: u64 = 39_697_679_104;

fn docs_in_progress() -> Queued {
    Queued {
        running: true,
        ..stuck("docs", OTHER)
    }
}

#[test]
fn a_check_in_progress_on_a_run_still_going_stays_pending_and_is_never_ruled_on() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge
        .set_checks(&head, Checks::Pending(vec![docs_in_progress()]));
    rig.forge.set_run_in_progress(OTHER);
    rig.verdict(&runner);

    for _ in 0..3 {
        rig.clock.advance(CAP);
        let report = step(&runner).unwrap();
        assert!(
            matches!(report, Some(StepReport::GateFailed { .. })),
            "{report:?}"
        );
    }

    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());
}

#[test]
fn a_run_still_going_does_not_hold_up_the_stuck_run_beside_it() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let both = Checks::Pending(vec![docs_in_progress(), stuck("windows-gnu", RUN)]);
    rig.forge.set_checks(&head, both);
    rig.forge.set_run_in_progress(OTHER);
    rig.verdict(&runner);
    rig.clock.advance(CAP);

    let report = step(&runner).unwrap();

    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);
    match report {
        Some(StepReport::CiRerun { checks, .. }) => assert_eq!(checks, ["windows-gnu"]),
        other => panic!("{other:?}"),
    }
}

// A rerun run that is still going after another cap is waited on, not ruled on,
// until a job's own timeout has passed: GitHub ends a hosted job at six hours.
#[test]
fn a_check_of_a_rerun_run_still_going_is_ruled_on_only_past_a_jobs_timeout() {
    const JOB_TIMEOUT: u64 = 6 * 60 * 60;
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, windows_queued());
    rig.verdict(&runner);
    rig.clock.advance(CAP);
    step(&runner).unwrap();
    assert_eq!(rig.forge.whole_run_reruns(), [RUN]);

    // The rerun started, and its job is running
    let running = Queued {
        running: true,
        ..stuck("windows-gnu", RUN)
    };
    rig.forge.set_checks(&head, Checks::Pending(vec![running]));
    rig.clock.advance(CAP);
    assert_eq!(step(&runner).unwrap(), None, "it waits for the job to end");
    rig.clock.advance(JOB_TIMEOUT - CAP - 1);
    assert_eq!(step(&runner).unwrap(), None);

    rig.clock.advance(1);
    let (_, question) = ruling_report(step(&runner).unwrap());
    assert!(
        question.starts_with("windows-gnu stayed queued"),
        "{question}"
    );
    assert_eq!(rig.forge.whole_run_reruns(), [RUN], "no second rerun");
}

const JOB_TIMEOUT: u64 = 6 * 60 * 60;

fn pages_deploy() -> Queued {
    Queued {
        name: "Cloudflare Pages".to_owned(),
        run: None,
        running: true,
    }
}

#[test]
fn a_runless_check_within_a_jobs_timeout_stays_pending_without_a_ruling() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge
        .set_checks(&head, Checks::Pending(vec![pages_deploy()]));
    rig.verdict(&runner);

    rig.clock.advance(JOB_TIMEOUT - CAP);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());
}

#[test]
fn a_runless_check_in_progress_past_a_jobs_timeout_raises_check_queued() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge
        .set_checks(&head, Checks::Pending(vec![pages_deploy()]));
    rig.verdict(&runner);
    rig.clock.advance(JOB_TIMEOUT);

    let (id, question) = ruling_report(step(&runner).unwrap());

    assert_eq!(id, ticket(71));
    assert_eq!(
        question,
        format!(
            "Cloudflare Pages never finished on pull request #71 at {}. It comes from an app, \
             not a workflow, so kelpie cannot rerun it, and it can be retried where it was \
             posted: https://github.com/shep-pm/shep/pull/71/checks. `shep kelpie rule 71 \
             yes` or `shep kelpie rule 71 no <note>` has kelpie wait another \
             6 hours.",
            &head[..7]
        )
    );
    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());
}

#[test]
fn a_yes_on_a_runless_check_queued_reruns_nothing_and_waits_again() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge
        .set_checks(&head, Checks::Pending(vec![pages_deploy()]));
    rig.verdict(&runner);
    rig.clock.advance(JOB_TIMEOUT);
    ruling_report(step(&runner).unwrap());

    rig.ask(&runner, "rule", Some("71 yes"));

    assert_eq!(rig.forge.whole_run_reruns(), Vec::<u64>::new());
    assert_eq!(step(&runner).unwrap(), None, "it waits again");
    rig.clock.advance(JOB_TIMEOUT - 1);
    assert_eq!(step(&runner).unwrap(), None, "the wait starts from the yes");
}
