//! The resumes one head earns: a timed-out or failed turn's ruling keeps the
//! commit its worktree stood at, and past two of them at one commit the next
//! ruling there is the maintainer's, whatever `rulings.pm_decides` says

use std::sync::Mutex;
use std::time::Instant;

use super::super::asked::pending;
use super::super::turn_answers::{BOTH, card, failed, phase, project, votes};
use super::super::{answered, past_the_window, pm_seen, step_until};
use crate::ports::{Role, Timestamp};
use crate::runner::pm::Wake;
use crate::runner::ruling::{Answerer, Record};
use crate::runner::{Answer, Runner, StepReport, step};
use crate::settings::RulingName;
use crate::state::{Ruling, Stuck};
use crate::test::{CALL_PATIENCE, Rig, Scripted, ticket};
use crate::work_item::{Phase, Turn};

/// The kinds left to the maintainer, so every answer here is the maintainer's
const FALSE_MAP: &str = "turn-timeout = false, turn-failed = false";

/// What kelpie says of a ruling past the limit, as the log names a dropped one
const STALLED: &str = "2 resumes at this head moved nothing";

/// [`BOTH`], and `rebase`, so a ruling of another kind reads as the project
/// manager's own
const TURN_AND_REBASE: &str = "turn-timeout = true, turn-failed = true, rebase = true";

/// A `turn-failed` ruling on #7 raised at `head`, as `pm_noticed` leaves it
fn failed_at(head: Option<&str>) -> Ruling {
    let reason = Stuck::TurnFailed {
        why: "harness exited with 1".into(),
        phase: Phase::Implement,
        retry: Turn::Due,
        restore: Vec::new(),
        setup: false,
    };
    raised_at("harness exited with 1", reason, head)
}

/// A `turn-timeout` ruling on #7 raised at `head`
fn timed_out_at(head: Option<&str>) -> Ruling {
    raised_at(
        "the turn ran past its ceiling",
        Stuck::TurnTimeout { phase: None },
        head,
    )
}

/// A ruling of stuck reason `reason`, asked as `asked`, on #7 at `head`
fn raised_at(asked: &str, reason: Stuck, head: Option<&str>) -> Ruling {
    Ruling {
        issue: Some(ticket(7)),
        stuck_head: head.map(str::to_owned),
        ..Ruling::new(ticket(7), asked, reason.into())
    }
}

/// Appends the history line of `ruling`, answered yes by the project manager
fn resumed(rig: &Rig, ruling: &Ruling) {
    Record::of(ruling, &Answer::Yes, Timestamp(1), Answerer::Pm)
        .append(&rig.paths().rulings)
        .unwrap();
}

/// The project manager's answer unsticking #7 with `action`
fn unstick(action: &str) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"unstick\": {{\"item\": 7, \"action\": \"{action}\", \
         \"why\": \"PM-WHY\"}}}}"
    );
    Box::leak(text.into_boxed_str())
}

/// A project with `map` as its `rulings.pm_decides`, #7 open, whose worker's
/// turn fails at one head `turns` times in a row, each ruling but the last
/// answered yes, by the project manager where the map gives it the kind and by
/// the maintainer where it does not, stepped until the last ruling waits
fn failed_at_one_head(name: &str, map: &str, turns: usize) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = project(name, map);
    let by_pm = {
        let runner = runner.lock().unwrap();
        (runner.settings.rulings).pm_decides(RulingName::TurnFailed)
    };
    rig.claude.script((0..turns).map(|_| failed()));
    if by_pm {
        rig.claude
            .script_for(Role::Pm, (1..turns).map(|_| Scripted::Say(votes('A', ""))));
    }
    for round in 1..turns {
        if by_pm {
            worker_turns(&rig, &runner, round + 1);
        } else {
            waiting(&rig, &runner);
            rig.ask(&runner, "rule", Some("7 yes"));
        }
    }
    waiting(&rig, &runner);
    (rig, runner)
}

/// Steps until the worker has made `n` calls, each a turn that fails
fn worker_turns(rig: &Rig, runner: &Mutex<Runner>, n: usize) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while rig.claude.calls().len() < n {
        let made = rig.claude.calls().len();
        assert!(Instant::now() < deadline, "the worker made {made} calls");
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

/// Steps until a ruling waits on #7
fn waiting(rig: &Rig, runner: &Mutex<Runner>) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while !pending(rig) {
        assert!(Instant::now() < deadline, "no ruling waits");
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

/// Whether the project manager is woken for a ruling of its own answer
fn asked_for_answer(runner: &Mutex<Runner>) -> bool {
    let runner = runner.lock().unwrap();
    (runner.pm.due.iter()).any(|wake| matches!(wake, Wake::Answer { .. }))
}

#[test]
fn a_second_ruling_at_one_head_is_still_the_project_managers() {
    let (rig, runner) = project("stalled-second", BOTH);
    let runner = runner.lock().unwrap();
    let first = failed_at(Some("c0ffee1"));

    assert!(
        runner.pm_answers_ruling(&first),
        "the first ruling at a head is the project manager's"
    );
    resumed(&rig, &first);

    assert!(
        !runner.turn_stalled(&failed_at(Some("c0ffee1"))),
        "one resume is not past the limit"
    );
    assert!(
        runner.pm_answers_ruling(&failed_at(Some("c0ffee1"))),
        "the second at the same head is the project manager's too"
    );
}

#[test]
fn a_third_ruling_at_one_head_goes_to_the_maintainer_at_once() {
    let (rig, runner) = failed_at_one_head("stalled-third", BOTH, 3);

    let card = card(&rig).expect("the maintainer is asked");
    assert!(
        card.contains("The worker's turn failed. Try again?"),
        "{card}"
    );
    assert_eq!(pm_seen(&rig).len(), 2, "the two before it were woken for");
    assert!(
        !asked_for_answer(&runner),
        "this one wakes no answer of the project manager's"
    );
    assert!(pending(&rig), "the item stays parked on it");
}

#[test]
fn a_timeout_and_a_failure_at_one_head_share_the_count() {
    let (rig, runner) = project("stalled-mixed", BOTH);
    let runner = runner.lock().unwrap();
    resumed(&rig, &timed_out_at(Some("c0ffee1")));

    assert!(
        runner.pm_answers_ruling(&failed_at(Some("c0ffee1"))),
        "one timeout resumed is not past the limit"
    );
    resumed(&rig, &failed_at(Some("c0ffee1")));

    assert!(
        !runner.pm_answers_ruling(&timed_out_at(Some("c0ffee1"))),
        "a timeout and a failure at one head are the two resumes"
    );
}

#[test]
fn a_moved_head_starts_the_count_again() {
    let (rig, runner) = project("stalled-moved", BOTH);
    let runner = runner.lock().unwrap();
    for head in ["c0ffee1", "9beef12"] {
        resumed(&rig, &failed_at(Some(head)));
        resumed(&rig, &timed_out_at(Some(head)));
    }

    assert!(
        !runner.pm_answers_ruling(&failed_at(Some("9beef12"))),
        "two resumes at the newest head are past the limit"
    );
    assert!(
        runner.pm_answers_ruling(&failed_at(Some("c0ffee1"))),
        "the older head's two are behind a moved head, which starts again"
    );
    assert!(
        runner.pm_answers_ruling(&failed_at(Some("feedface"))),
        "a head never ruled on is the project manager's"
    );
}

#[test]
fn a_stalled_rulings_card_says_why_it_came_to_the_maintainer() {
    let (rig, _runner) = failed_at_one_head("stalled-card", BOTH, 3);

    let card = card(&rig).expect("the maintainer is asked");

    let line = "| 👍 or `7 yes` | Tries that step again. 2 resumes at this head moved nothing. |";
    assert!(card.contains(line), "{card}");
    let no = "| 👎 or `7 no <note>` | Stops the work item";
    assert!(card.contains(no), "{card}");
}

#[test]
fn a_retry_past_the_limit_is_dropped_and_the_item_stays_parked() {
    let (rig, runner) = failed_at_one_head("stalled-retry", BOTH, 3);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(unstick("retry"))]);
    past_the_window(&rig);

    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        [format!(
            "retry #7: {STALLED}, so only the maintainer can ask for another"
        )]
    );
    assert!(pending(&rig), "the ruling still waits");
    assert_eq!(phase(&rig, &runner)["state"], "ruling");
}

#[test]
fn a_retry_past_the_limit_is_dropped_with_the_kind_set_false() {
    let (rig, runner) = failed_at_one_head("stalled-retry-false", FALSE_MAP, 3);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(unstick("retry"))]);
    past_the_window(&rig);

    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        [format!(
            "retry #7: {STALLED}, so only the maintainer can ask for another"
        )]
    );
    assert!(pending(&rig), "the ruling still waits");
    assert_eq!(phase(&rig, &runner)["state"], "ruling");
}

#[test]
fn an_ask_past_the_limit_still_adds_its_words_to_the_ruling() {
    let (rig, runner) = failed_at_one_head("stalled-ask", BOTH, 3);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(unstick("ask"))]);
    past_the_window(&rig);

    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));

    assert_eq!(acted, ["put #7 to the maintainer on ruling 7"]);
    assert_eq!(dropped, Vec::<String>::new());
    let status = rig.ask(&runner, "status", None);
    let question = status["rulings"][0]["question"]
        .as_str()
        .expect("the ruling asks");
    assert!(
        question.ends_with("The project manager asks you to decide. PM says: PM-WHY"),
        "{question}"
    );
}

#[test]
fn the_maintainers_yes_past_the_limit_resumes_and_the_next_at_that_head_is_theirs() {
    let (rig, runner) = failed_at_one_head("stalled-yes", BOTH, 3);

    rig.ask(&runner, "rule", Some("7 yes"));
    worker_turns(&rig, &runner, 4);
    waiting(&rig, &runner);

    let runner = runner.lock().unwrap();
    let ruling = &runner.state.rulings[0];
    assert!(!ruling.unposted, "the next ruling there is posted");
    assert!(
        !runner.pm_answers_ruling(ruling),
        "and it is the maintainer's, until the head moves"
    );
    let answer_wake = (runner.pm.due.iter()).any(|wake| matches!(wake, Wake::Answer { .. }));
    assert!(!answer_wake, "it wakes no answer of the project manager's");
}

#[test]
fn a_repeat_of_one_failure_at_a_moved_head_is_left_to_the_project_manager() {
    let (rig, runner) = project("stalled-repeat", BOTH);
    let runner = runner.lock().unwrap();
    let answered = failed_at(Some("c0ffee1"));

    assert!(runner.pm_answers_ruling(&answered));
    resumed(&rig, &answered);

    assert!(
        runner.pm_answers_ruling(&failed_at(Some("9beef12"))),
        "the same failure at a head that moved shows the worker got further"
    );
}

// A ruling of another kind is never stalled, whatever its item's history holds
#[test]
fn another_kind_of_ruling_is_never_stalled() {
    let (rig, runner) = project("stalled-other", TURN_AND_REBASE);
    let runner = runner.lock().unwrap();
    for head in ["c0ffee1", "9beef12"] {
        resumed(&rig, &failed_at(Some(head)));
    }
    let rebase = raised_at(
        "main moved under the branch",
        Stuck::Rebase {
            why: "conflicts".into(),
            review: None,
        },
        Some("c0ffee1"),
    );

    assert!(!runner.turn_stalled(&rebase));
    assert!(runner.pm_answers_ruling(&rebase));
}

// A head git could not read matches another that kept none, so an unreadable
// head errs towards the maintainer
#[test]
fn a_ruling_with_no_head_counts_with_another_with_none() {
    let (rig, runner) = project("stalled-no-head", BOTH);
    let runner = runner.lock().unwrap();
    resumed(&rig, &failed_at(None));
    resumed(&rig, &timed_out_at(None));

    assert!(
        !runner.pm_answers_ruling(&failed_at(None)),
        "two rulings with no head are the two resumes"
    );
    assert!(
        runner.pm_answers_ruling(&failed_at(Some("c0ffee1"))),
        "a head the item was never ruled at is the project manager's"
    );
}

// The count is this item's own turn rulings: another item's at the same head,
// and this item's of another kind, are no part of it
#[test]
fn the_count_is_this_items_own_turn_rulings() {
    let (rig, runner) = project("stalled-others", BOTH);
    let runner = runner.lock().unwrap();
    let rebase = raised_at(
        "main moved under the branch",
        Stuck::Rebase {
            why: "conflicts".into(),
            review: None,
        },
        Some("c0ffee1"),
    );
    let other = Ruling {
        issue: Some(ticket(9)),
        ..failed_at(Some("c0ffee1"))
    };
    for ruling in [&rebase, &other, &failed_at(Some("c0ffee1"))] {
        resumed(&rig, ruling);
    }

    assert!(
        runner.pm_answers_ruling(&failed_at(Some("c0ffee1"))),
        "one turn ruling of this item at this head is no limit"
    );
}
