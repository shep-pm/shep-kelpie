//! Files saved by older versions of kelpie, loaded and saved again

use std::fs;

use super::super::*;
use super::store_in;
use crate::test::{a_work_item, a_work_item_from_before_comments, ticket};

#[test]
fn a_version_6_file_loads_with_nothing_attached_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 6.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.work_items[0].attached, None);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

#[test]
fn a_version_7_file_loads_with_no_project_manager_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 7.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(
        (&loaded.pm_session, &loaded.pm_told, &loaded.pm_attached),
        (&None, &Vec::new(), &None)
    );
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

#[test]
fn a_version_9_file_saved_paused_loads_and_saves_with_no_run_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 9.into();
    old["run"] = "paused".into();
    old["since"] = 7.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded, ProjectState::new());
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!((saved.get("run"), saved.get("since")), (None, None));
}

#[test]
fn a_version_12_file_loads_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item_from_before_comments());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 12.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded, state);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

// A version 14 file has no `late_from` on its work items and no `late_fix`
// or `nits` on its merge rulings, which load as their defaults.
#[test]
fn a_version_14_file_loads_with_no_late_fix_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 14.into();
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    item["noted_from"] = "c0ffee".into();
    old["work_items"] = serde_json::json!([item]);
    old["rulings"] = serde_json::json!([{
        "id": 1,
        "issue": 42,
        "question": "q",
        "pull_request": 51,
        "kind": { "kind": "merge", "head": "f1x", "note_fix": true },
        "alerted": true,
    }]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    let item = &loaded.work_items[0];
    assert_eq!(
        (item.noted_from.as_deref(), item.late_from.as_deref()),
        (Some("c0ffee"), None)
    );
    assert_eq!(
        loaded.rulings[0].kind,
        RulingKind::Merge {
            head: "f1x".into(),
            unreviewed: None,
            open_threads: None,
            held_by: Vec::new(),
            unread_head: false,
            note_fix: true,
            late_fix: false,
            nits: None,
            disputes: None,
            rivals: Vec::new(),
        }
    );
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

// A version 70 file has no `ci_failures` in a work item's counts: none counted.
// A count saves under its class alone.
#[test]
fn a_version_70_file_loads_with_no_red_heads_counted() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 70.into();
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    item["counts"] =
        serde_json::json!({ "review_rounds": 1, "fix_turns": 0, "rulings": 0, "ci_fix_turns": 1 });
    old["work_items"] = serde_json::json!([item]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert!(loaded.work_items[0].counts.ci_failures.is_zero());
    loaded.work_items[0].counts.ci_failures.runnable = 2;
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["work_items"][0]["counts"]["ci_failures"],
        serde_json::json!({ "runnable": 2 })
    );
}

#[test]
fn a_version_71_file_loads_with_nothing_unmined_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 71.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert!(
        !loaded.pm_unmined,
        "an older file has nothing waiting to be read out of the archive"
    );
    loaded.pm_unmined = true;
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["pm_unmined"], serde_json::json!(true));
}

// A version 15 file has no `ci_fix_turns` in a work item's counts and no
// `fix_turns` on a still-red ruling: none counted, and the pushed-no-fix ruling.
#[test]
fn a_version_15_file_loads_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 15.into();
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    item["counts"] = serde_json::json!({ "review_rounds": 2, "fix_turns": 1, "rulings": 1 });
    old["work_items"] = serde_json::json!([item]);
    let still_red = serde_json::json!({
        "kind": "stuck",
        "reason": "still-red",
        "head": "bad",
        "checks": ["lint"],
    });
    old["rulings"] = serde_json::json!([{
        "id": 1,
        "issue": 42,
        "question": "q",
        "pull_request": 51,
        "kind": still_red,
        "alerted": true,
    }]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    let counts = loaded.work_items[0].counts;
    assert_eq!((counts.fix_turns, counts.ci_fix_turns), (1, 0));
    assert_eq!(
        loaded.rulings[0].kind,
        RulingKind::from(Stuck::StillRed {
            head: "bad".into(),
            checks: vec!["lint".into()],
            fix_turns: None,
            review: None,
            rerun: false,
        })
    );
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0]["counts"], item["counts"]);
    assert_eq!(saved["rulings"][0]["kind"], still_red);
}

// A version 20 file has no `thread_answers`: a fix turn under way when it was
// saved has said nothing of its threads, so each is replied to as not addressed.
// The worker's lines are pinned as they are saved.
#[test]
fn a_version_20_file_loads_with_no_thread_answers_and_saves_them_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 20.into();
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    item["threads_sent"] = serde_json::json!(["PRRT_a", "PRRT_b"]);
    old["work_items"] = serde_json::json!([item]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.work_items[0].thread_answers, []);
    loaded.work_items[0].answer_threads("thread 1: fixed\nthread 2: declined: deliberate");
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["work_items"][0]["thread_answers"],
        serde_json::json!([
            { "said": "fixed", "thread": "PRRT_a" },
            { "said": "declined", "thread": "PRRT_b", "reason": "deliberate" },
        ])
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A ruling saved before `raised` loads with no time, and one raised since
// keeps its time through a save and a load.
#[test]
fn a_ruling_saved_without_raised_loads_and_a_raised_time_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.rulings.push(Ruling {
        issue: Some(ticket(7)),
        pull_request: Some(71),
        alerted: true,
        ..Ruling::new(ticket(1), "q", RulingKind::Stuck(Stuck::Closed))
    });
    let old = serde_json::to_value(&state).unwrap();
    assert!(old["rulings"][0].get("raised").is_none());
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.rulings[0].raised, None);
    loaded.rulings[0].raised = Some(Timestamp(1_700_000_000));
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["rulings"][0]["raised"], 1_700_000_000);
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 87 ruling has no `approaches` or `asked`, so it loads with
// neither, and those a ruling gains save and load back.
#[test]
fn a_version_87_ruling_loads_with_no_approaches_and_keeps_those_it_gains() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.rulings.push(Ruling {
        issue: Some(ticket(7)),
        pull_request: Some(71),
        ..Ruling::new(ticket(1), "q", RulingKind::Stuck(Stuck::Closed))
    });
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 87.into();
    assert!(old["rulings"][0].get("approaches").is_none());
    assert!(old["rulings"][0].get("asked").is_none());
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert!(loaded.rulings[0].approaches.is_empty());
    assert_eq!(loaded.rulings[0].asked, None);
    loaded.rulings[0].approaches = vec!["Split the parser out first".into()];
    loaded.rulings[0].asked = Some(PmAsked::Options);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["rulings"][0]["approaches"],
        serde_json::json!(["Split the parser out first"])
    );
    assert_eq!(saved["rulings"][0]["asked"], "options");
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 30 file has no `order`, so it loads with the board following its
// rule, and an order a later read keeps saves and loads back.
#[test]
fn a_version_30_file_loads_with_an_empty_order_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 30.into();
    old.as_object_mut().unwrap().remove("order");
    old.as_object_mut().unwrap().remove("ranked");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.order, Vec::<TicketId>::new());
    assert!(loaded.ranked.is_empty());
    loaded.order = vec![ticket(7), ticket(3), ticket(9)];
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (&saved["version"], &saved["order"]),
        (
            &serde_json::json!(VERSION),
            &serde_json::json!(["7", "3", "9"])
        )
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 33 file has no `test` on a finding: it loads with none attached, a
// finding with one keeps it through a save, and one without saves no key.
#[test]
fn a_state_file_without_a_test_still_loads() {
    use crate::ports::Severity;
    use crate::work_item::{Phase, Review, ReviewStage};

    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let state_with = |test: Option<&str>| {
        let mut item = a_work_item_from_before_comments();
        item.phase = Phase::Review(Review {
            stage: ReviewStage::Found {
                findings: vec![Finding {
                    test: test.map(str::to_owned),
                    ..Finding::new(
                        Severity::High,
                        "src/lib.rs",
                        3,
                        "it panics",
                        "a caller sees it",
                    )
                }],
                threads: Vec::new(),
            },
            ..Review::first()
        });
        let mut state = ProjectState::new();
        state.work_items.push(item);
        state
    };
    let finding_of =
        |saved: &serde_json::Value| saved["work_items"][0]["phase"]["stage"]["findings"][0].clone();

    let mut old = serde_json::to_value(state_with(None)).unwrap();
    old["version"] = 33.into();
    assert!(finding_of(&old).get("test").is_none(), "{old}");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
    assert_eq!(store.load().unwrap(), Some(state_with(None)));

    let attached = state_with(Some("tests/panics.rs"));
    store.save(&attached).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(finding_of(&saved)["test"], "tests/panics.rs");
    assert_eq!(store.load().unwrap(), Some(attached));
}

// A version 34 file has no `test_checks`: it loads with none, a work item with
// a check keeps it through a save, and one without saves no key.
#[test]
fn a_state_file_without_test_checks_still_loads() {
    use crate::ports::TestOutcome;
    use crate::work_item::TestCheck;

    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let state_with = |checks: Vec<TestCheck>| {
        let mut item = a_work_item_from_before_comments();
        item.test_checks = checks;
        let mut state = ProjectState::new();
        state.work_items.push(item);
        state
    };

    let mut old = serde_json::to_value(state_with(Vec::new())).unwrap();
    old["version"] = 34.into();
    assert!(old["work_items"][0].get("test_checks").is_none(), "{old}");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
    assert_eq!(store.load().unwrap(), Some(state_with(Vec::new())));

    let checked = state_with(vec![TestCheck {
        file: "src/lib.rs".into(),
        line: 3,
        what: "it panics".into(),
        round: 2,
        before: TestOutcome::Failed {
            output: "boom".into(),
        },
        after: Some(TestOutcome::Passed),
        sent_back: false,
    }]);
    store.save(&checked).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["work_items"][0]["test_checks"],
        serde_json::json!([{
            "file": "src/lib.rs",
            "line": 3,
            "what": "it panics",
            "round": 2,
            "before": { "result": "failed", "output": "boom" },
            "after": { "result": "passed" },
        }])
    );
    assert_eq!(store.load().unwrap(), Some(checked));
}

// A version 35 file has no `reran`: it loads with none, a work item that reran
// a head keeps it through a save, and one that did not saves no key.
#[test]
fn a_state_file_without_reran_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let state_with = |reran: Option<&str>| {
        let mut item = a_work_item_from_before_comments();
        item.reran = reran.map(str::to_owned);
        let mut state = ProjectState::new();
        state.work_items.push(item);
        state
    };

    let mut old = serde_json::to_value(state_with(None)).unwrap();
    old["version"] = 35.into();
    assert!(old["work_items"][0].get("reran").is_none(), "{old}");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
    assert_eq!(store.load().unwrap(), Some(state_with(None)));

    let reran = state_with(Some("c0ffee"));
    store.save(&reran).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0]["reran"], "c0ffee");
    assert_eq!(store.load().unwrap(), Some(reran));
}

// A version 38 file has no `talked` on its work items, so no comment is
// handled and no round counted. A work item with a pull request loads with
// its comments marked as seen, since its worker has read them.
#[test]
fn a_version_38_file_loads_with_no_comment_handled_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 38.into();
    let item = serde_json::to_value(a_work_item()).unwrap();
    assert!(item.get("talked").is_none(), "{item}");
    old["work_items"] = serde_json::json!([item]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    let talked = &loaded.work_items[0].talked;
    assert!(talked.comments.is_empty() && talked.rounds == 0);
    // Its pull request has comments its worker has seen.
    assert_eq!(talked.quiet, loaded.work_items[0].pull_request.is_some());
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

// A version 40 file has no `sent_back` on its checks: they load as not sent
// back, and one that was sent back keeps the mark through a save.
#[test]
fn a_state_file_without_the_sent_back_mark_still_loads() {
    use crate::ports::TestOutcome;
    use crate::work_item::TestCheck;

    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let check = |sent_back| TestCheck {
        file: "src/lib.rs".into(),
        line: 3,
        what: "it panics".into(),
        round: 2,
        before: TestOutcome::Failed {
            output: "boom".into(),
        },
        after: Some(TestOutcome::Failed {
            output: "boom".into(),
        }),
        sent_back,
    };
    let state_with = |check: TestCheck| {
        let mut item = a_work_item_from_before_comments();
        item.test_checks = vec![check];
        let mut state = ProjectState::new();
        state.work_items.push(item);
        state
    };

    let mut old = serde_json::to_value(state_with(check(false))).unwrap();
    old["version"] = 40.into();
    assert!(
        old["work_items"][0]["test_checks"][0]
            .get("sent_back")
            .is_none(),
        "{old}"
    );
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
    assert_eq!(store.load().unwrap(), Some(state_with(check(false))));

    store.save(&state_with(check(true))).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0]["test_checks"][0]["sent_back"], true);
    assert_eq!(store.load().unwrap(), Some(state_with(check(true))));
}

#[test]
fn an_older_state_file_loads_with_no_contributions() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 39.into();
    assert_eq!(old.get("contributions"), None);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.contributions, []);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

#[test]
fn an_older_state_file_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.contributions.push(Contribution::new(
        12,
        "newcomer".into(),
        "2a8f1e6".into(),
        Timestamp(1_790_000_000),
    ));
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 47.into();
    for key in ["heard", "context"] {
        assert_eq!(old["contributions"][0].get(key), None, "left out until set");
    }
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");

    assert_eq!(loaded, state);
    assert_eq!(loaded.contributions[0].heard, []);
    assert_eq!(loaded.contributions[0].context, []);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

#[test]
fn what_a_contribution_heard_survives_a_save_and_a_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut heard = Contribution::new(12, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    heard.hear(Said::Comment {
        id: 7,
        at: Timestamp(2),
    });
    heard.hear(Said::Reply {
        id: 7,
        at: Timestamp(3),
    });
    heard.overhear(Said::Comment {
        id: 8,
        at: Timestamp(4),
    });
    let heard = heard.pushed("9c0ffee".into(), Timestamp(5));
    let mut state = ProjectState::new();
    state.contributions.push(heard);

    store.save(&state).unwrap();

    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn an_id_means_a_thing_only_with_its_kind() {
    let mut contribution = Contribution::new(12, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    let comment = Said::Comment {
        id: 7,
        at: Timestamp(2),
    };
    contribution.hear(comment.clone());
    contribution.overhear(Said::Reply {
        id: 9,
        at: Timestamp(3),
    });

    assert!(contribution.knows(&comment));
    assert!(contribution.knows(&Said::Comment {
        id: 7,
        at: Timestamp(99)
    }));
    assert!(!contribution.knows(&Said::Reply {
        id: 7,
        at: Timestamp(2)
    }));
    assert!(contribution.knows(&Said::Reply {
        id: 9,
        at: Timestamp(3)
    }));
}

#[test]
fn a_contribution_keeps_the_last_fifty_of_each_list() {
    let mut contribution = Contribution::new(12, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    for id in 0..70 {
        let said = Said::Comment {
            id,
            at: Timestamp(id),
        };
        contribution.hear(said.clone());
        contribution.overhear(said);
    }

    let ids = |list: &[Said]| -> Vec<u64> {
        list.iter()
            .map(|s| match s {
                Said::Comment { id, .. } => *id,
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(ids(&contribution.heard), (20..70).collect::<Vec<_>>());
    assert_eq!(ids(&contribution.context), (20..70).collect::<Vec<_>>());
    assert_eq!(contribution.latest(), Timestamp(69));
}

#[test]
fn a_full_list_does_not_take_back_what_dropped_off_it() {
    let mut contribution = Contribution::new(12, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    for id in 0..51 {
        let at = Timestamp(100 + id);
        assert!(contribution.hear(Said::Comment { id, at }));
    }

    let old = Said::Comment {
        id: 0,
        at: Timestamp(100),
    };
    assert!(!contribution.hear(old.clone()));
    assert_eq!(contribution.heard.len(), 50);
    assert_eq!(contribution.heard[0].at(), Timestamp(101));
    // A push is always new, and a newer comment is still recorded
    assert!(contribution.hear(Said::Push {
        head: "abc".into(),
        at: Timestamp(600)
    }));
    assert!(contribution.hear(Said::Comment {
        id: 99,
        at: Timestamp(700)
    }));
}

#[test]
fn a_followed_contribution_survives_a_save_and_a_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.contributions.push(Contribution::new(
        12,
        "newcomer".into(),
        "2a8f1e6".into(),
        Timestamp(1_790_000_000),
    ));
    store.save(&state).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn a_contributions_security_review_survives_a_save_and_a_load_and_is_left_out_until_made() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    let mut read = Contribution::new(12, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    state.contributions.push(read.clone());
    store.save(&state).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["contributions"][0].get("security"), None);
    assert_eq!(saved["contributions"][0].get("passed"), None);

    read.passed = Some("2a8f1e6".into());
    read.security = Some(Security {
        head: "2a8f1e6".into(),
        verdict: SecurityVerdict::Passed,
    });
    state.contributions = vec![read];
    store.save(&state).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));
}

// A version 42 file has no `red`, so no workflow is red on `main` until the
// runner reads it. A red workflow survives a save and a load.
#[test]
fn a_version_42_file_loads_with_no_red_workflow_and_red_ones_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 42.into();
    assert!(old.get("red").is_none(), "{old}");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert!(loaded.red.is_empty());

    loaded.red = vec![RedWorkflow {
        workflow: "test".into(),
        first_run: 91,
        run: 95,
        head: "abc123".into(),
        since: Timestamp(1_790_000_000),
        reran: None,
        reran_head: None,
        again: false,
        handled: None,
        noticed: false,
    }];
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["red"][0]["workflow"], "test");
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 43 file's red workflow has no `reran` or `noticed`, and its notice
// is a merge's. Both load, and a notice of a workflow on `main` round trips
// beside it.
#[test]
fn a_version_43_file_loads_its_red_workflow_and_merge_notice_and_main_notices_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 43.into();
    old["red"] = serde_json::json!([{
        "workflow": "test", "first_run": 91, "run": 95, "head": "abc123", "since": 1_790_000_000
    }]);
    old["notices"] = serde_json::json!([{ "issue": 22, "pull_request": 30, "head": "c0ffee" }]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.red[0].reran, None);
    assert!(!loaded.red[0].noticed);
    assert!(matches!(loaded.notices[..], [Notice::Merged(_)]));

    loaded.red[0].reran = Some(95);
    loaded.red[0].noticed = true;
    loaded.notices.push(Notice::Main(MainNotice {
        workflow: "test".into(),
        run: 95,
        red_for: 900,
        log: "error".into(),
        green: false,
    }));
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["notices"][1],
        serde_json::json!({ "workflow": "test", "run": 95, "red_for": 900, "log": "error" })
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 46 file has no contribution rulings, and one raised since is kept
// as the runner left it.
#[test]
fn an_older_state_file_loads_and_a_contribution_ruling_survives_a_save_and_a_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 46.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.rulings, []);

    let mut state = ProjectState::new();
    state.rulings.push(Ruling {
        id: ticket(12),
        issue: None,
        question: "flagged".into(),
        pull_request: Some(12),
        kind: RulingKind::Contribution(Held::Security {
            head: "2a8f1e6".into(),
            lines: vec![Finding {
                severity: crate::ports::Severity::High,
                file: "build.rs".into(),
                line: 3,
                what: "runs a script".into(),
                why: "at build time".into(),
                test: None,
            }],
        }),
        alerted: false,
        unposted: false,
        effects: Vec::new(),
        raised: Some(Timestamp(1)),
        approaches: Vec::new(),
        asked: None,
    });
    store.save(&state).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["rulings"][0]["kind"]["kind"], "contribution");
    assert_eq!(saved["rulings"][0]["kind"]["reason"], "security");
    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn a_version_53_file_loads_with_no_disputes_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 53.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.work_items[0].disputes, []);
    assert_eq!(loaded.work_items[0].disputes_comment, None);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

mod tickets;

// A version 74 file has no `disputed` or `deferred` on a work item's counts:
// none were. Each saves once above 0 and is left out at 0.
#[test]
fn a_state_file_from_before_disputed_and_deferred_loads_with_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 74.into();
    old["work_items"] = serde_json::json!([serde_json::to_value(a_work_item()).unwrap()]);
    assert_eq!(old["work_items"][0]["counts"].get("disputed"), None);
    assert_eq!(old["work_items"][0]["counts"].get("deferred"), None);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    let counts = loaded.work_items[0].counts;
    assert_eq!((counts.disputed, counts.deferred), (0, 0));
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0]["counts"].get("disputed"), None);

    loaded.work_items[0].counts.disputed = 1;
    loaded.work_items[0].counts.deferred = 2;
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["work_items"][0]["counts"]["disputed"], 1);
    assert_eq!(saved["work_items"][0]["counts"]["deferred"], 2);
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 86 file's ruling may hold the panel's ballots, its verdict and
// why it was skipped. The panel is gone, so the ruling loads without them and
// waits on the maintainer as it stands.
#[test]
fn a_version_86_file_with_a_panel_ruling_loads_without_the_panels_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.rulings.push(Ruling {
        issue: Some(ticket(7)),
        pull_request: Some(71),
        unposted: true,
        ..Ruling::new(ticket(71), "q", RulingKind::Stuck(Stuck::Closed))
    });
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 86.into();
    old["rulings"][0]["panel"] = serde_json::json!([
        { "voter": "advisor", "vote": { "choice": "A", "why": "w", "design": false } },
        { "voter": "worker", "vote": null },
    ]);
    old["rulings"][0]["verdict"] = serde_json::json!({
        "work": "fix", "tier": "fix", "advice": { "choice": "A", "agree": 1, "of": 2 },
    });
    old["rulings"][0]["skipped"] = "draining".into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");

    assert_eq!(loaded.rulings, state.rulings);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    for field in ["panel", "verdict", "skipped"] {
        assert_eq!(saved["rulings"][0].get(field), None, "{saved}");
    }
}
