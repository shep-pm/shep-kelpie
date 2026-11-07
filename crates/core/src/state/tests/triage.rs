use std::fs;

use super::super::*;
use super::store_in;
use crate::ports::Timestamp;

// A version 59 file has no `triage`, so it loads with no `since`; once set it
// is kept, and it is left out while unset.
#[test]
fn a_file_saved_before_this_version_loads_with_no_since() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 59.into();
    old.as_object_mut().unwrap().remove("triage");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.triage, None);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert!(saved.get("triage").is_none(), "left out while unset");

    loaded.triage = Some(TriageState {
        since: Timestamp(1_791_964_800),
        issues: Vec::new(),
        unrecorded: Vec::new(),
        pruned_through: 0,
        splits: Vec::new(),
    });
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        saved["triage"],
        serde_json::json!({ "since": 1_791_964_800 })
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 60 file has a `since` and no `issues`, so it loads with no records;
// records are kept with their step, and left out while there are none.
#[test]
fn a_file_saved_before_this_version_loads_with_no_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 60.into();
    old["triage"] = serde_json::json!({ "since": 1_791_964_800 });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    let triage = loaded.triage.as_mut().expect("the since");
    assert_eq!(triage.since, Timestamp(1_791_964_800));
    assert_eq!(triage.issues, []);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"],
        serde_json::json!({ "since": 1_791_964_800 })
    );

    let triage = loaded.triage.as_mut().unwrap();
    triage.issues.push(Triaged {
        issue: 12,
        comment: 5001,
        step: TriageStep::Labelled,
        at: Timestamp(0),
        author: String::new(),
        held: None,
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: None,
        ready_body: None,
        seen: Vec::new(),
        asked: None,
        questions: None,
        local: None,
        spam: None,
        begun: None,
    });
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        saved["triage"]["issues"],
        serde_json::json!([{ "issue": 12, "comment": 5001, "at": 0, "step": "labelled" }])
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 62 file has a record with no `duplicate_of`, so it loads with
// none. A closed record keeps the item it was closed as a duplicate of, and
// the step that says the close is still owed.
#[test]
fn a_file_saved_before_this_version_loads_with_no_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 62.into();
    old["triage"] = serde_json::json!({
        "since": 1_791_964_800,
        "issues": [{ "issue": 12, "comment": 5001, "step": "done" }],
    });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    let triage = loaded.triage.as_mut().expect("the since");
    assert_eq!(
        triage.issues,
        [Triaged {
            issue: 12,
            comment: 5001,
            step: TriageStep::Done,
            at: Timestamp(0),
            author: String::new(),
            held: None,
            calls: Vec::new(),
            labels: Vec::new(),
            duplicate_of: None,
            ready_body: None,
            seen: Vec::new(),
            asked: None,
            questions: None,
            local: None,
            spam: None,
            begun: None,
        }]
    );

    triage.issues.push(Triaged {
        issue: 13,
        comment: 5002,
        step: TriageStep::Closed,
        at: Timestamp(0),
        author: String::new(),
        held: None,
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: Some(41),
        ready_body: None,
        seen: Vec::new(),
        asked: None,
        questions: None,
        local: None,
        spam: None,
        begun: None,
    });
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"]["issues"][1],
        serde_json::json!({
            "issue": 13, "comment": 5002, "at": 0, "step": "closed", "duplicate_of": 41
        })
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// Past `TRIAGED_KEPT` finished records the oldest go, and a record still short
// of `done` stays whatever its age.
#[test]
fn the_oldest_finished_records_go_past_the_cap() {
    let record = |issue: u64, step: TriageStep| Triaged {
        issue,
        comment: issue + 1_000,
        step,
        at: Timestamp(0),
        author: String::new(),
        held: None,
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: None,
        ready_body: None,
        seen: Vec::new(),
        asked: None,
        questions: None,
        local: None,
        spam: None,
        begun: None,
    };
    let mut triage = TriageState {
        since: Timestamp(1),
        issues: vec![record(1, TriageStep::Commented)],
        unrecorded: Vec::new(),
        pruned_through: 0,
        splits: Vec::new(),
    };
    let cap = TRIAGED_KEPT as u64;
    triage
        .issues
        .extend((2..cap + 5).map(|n| record(n, TriageStep::Done)));

    triage.prune();

    assert_eq!(triage.issues.len(), TRIAGED_KEPT + 1);
    assert_eq!(triage.issues[0].issue, 1, "an unfinished record stays");
    assert_eq!(triage.issues[1].issue, 5, "the oldest finished ones went");
    assert_eq!(triage.issues.last().map(|t| t.issue), Some(cap + 4));
    assert_eq!(
        triage.pruned_through, 4,
        "the highest issue number a prune dropped is kept"
    );
}

// The daily caps count the calls the records hold, so a record that holds one is
// kept ahead of one that holds none: a burst of issues a limit held cannot push
// out the records the caps count, which is what they hold the burst for.
#[test]
fn a_prune_past_the_cap_lets_the_records_that_hold_no_call_go_first() {
    let cap = TRIAGED_KEPT as u64;
    let mut triage = TriageState {
        since: Timestamp(1),
        issues: (1..cap + 4)
            .map(|issue| Triaged {
                issue,
                comment: 0,
                step: TriageStep::Done,
                at: Timestamp(0),
                author: "newcomer".to_owned(),
                held: Some(TriageHeld::AuthorCap),
                calls: if issue <= 2 {
                    vec![Timestamp(9)]
                } else {
                    Vec::new()
                },
                labels: Vec::new(),
                duplicate_of: None,
                ready_body: None,
                seen: Vec::new(),
                asked: None,
                questions: None,
                local: None,
                spam: None,
                begun: None,
            })
            .collect(),
        unrecorded: Vec::new(),
        pruned_through: 0,
        splits: Vec::new(),
    };

    triage.prune();

    assert_eq!(triage.issues.len(), TRIAGED_KEPT);
    assert_eq!(
        (triage.issues.iter())
            .take(2)
            .map(|t| t.issue)
            .collect::<Vec<_>>(),
        [1, 2],
        "the two oldest records hold calls, and they stayed"
    );
    assert_eq!(
        triage.issues[2].issue, 6,
        "the oldest records that hold no call went instead"
    );
    assert_eq!(triage.issues.last().map(|t| t.issue), Some(cap + 3));
}

// A record that closed an issue as a duplicate is kept past the cap: the issue
// carries no `needs-triage` label, and the record is what keeps a reopened one
// out of the due list.
#[test]
fn a_record_that_closed_a_duplicate_is_kept_past_the_cap() {
    let cap = TRIAGED_KEPT as u64;
    let mut triage = TriageState {
        since: Timestamp(1),
        issues: (1..cap + 3)
            .map(|issue| Triaged {
                issue,
                comment: issue + 1_000,
                step: TriageStep::Done,
                at: Timestamp(0),
                author: String::new(),
                held: None,
                calls: Vec::new(),
                labels: Vec::new(),
                duplicate_of: match issue {
                    2 => Some(41),
                    _ => None,
                },
                ready_body: None,
                seen: Vec::new(),
                asked: None,
                questions: None,
                local: None,
                spam: None,
                begun: None,
            })
            .collect(),
        unrecorded: Vec::new(),
        pruned_through: 0,
        splits: Vec::new(),
    };

    triage.prune();

    assert_eq!(triage.issues.len(), TRIAGED_KEPT + 1);
    assert_eq!(
        triage.issues.first().map(|t| t.issue),
        Some(2),
        "the oldest ordinary record went and the duplicate's stayed"
    );
    assert_eq!(triage.issues[1].duplicate_of, None);
}

// A version 63 file, saved by the first pass before the ready path, has
// records with none of the new steps, no `ready_body` and no `seen`, so they
// load as they were and save again at the current version.
#[test]
fn a_record_saved_by_slice_2_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 63.into();
    old["triage"] = serde_json::json!({
        "since": 1_791_964_800,
        "issues": [
            { "issue": 12, "comment": 5001, "step": "commented", "labels": ["bug", "P2"] },
            { "issue": 13, "comment": 5002, "step": "labelled" },
            { "issue": 14, "comment": 5003, "step": "done" },
            { "issue": 15, "comment": 5004, "step": "closed", "duplicate_of": 41 },
        ],
    });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    let triage = loaded.triage.as_ref().expect("the records");
    assert_eq!(triage.issues.len(), 4);
    assert!(
        triage
            .issues
            .iter()
            .all(|t| t.ready_body.is_none() && t.seen.is_empty()),
        "{:?}",
        triage.issues
    );
    assert_eq!(triage.issues[0].labels, ["bug", "P2"]);
    assert_eq!(triage.issues[3].duplicate_of, Some(41));

    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"]["issues"],
        serde_json::json!([
            { "issue": 12, "comment": 5001, "at": 0, "step": "commented", "labels": ["bug", "P2"] },
            { "issue": 13, "comment": 5002, "at": 0, "step": "labelled" },
            { "issue": 14, "comment": 5003, "at": 0, "step": "done" },
            { "issue": 15, "comment": 5004, "at": 0, "step": "closed", "duplicate_of": 41 },
        ])
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 64 file, saved before the author's round, has records with no
// `asked`, no `questions` and no `answers` step, so they load with no ask and
// save again at the current version. A waiting record keeps the ask and the
// questions with the time each went up, and both are left out while unset.
#[test]
fn a_file_saved_before_this_version_loads_with_no_ask() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 64.into();
    old["triage"] = serde_json::json!({
        "since": 1_791_964_800,
        "issues": [{ "issue": 12, "comment": 5001, "step": "commented", "labels": ["bug"] }],
    });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    let triage = loaded.triage.as_ref().expect("the records");
    assert_eq!(triage.issues[0].asked, None);
    assert_eq!(triage.issues[0].questions, None);

    let triage = loaded.triage.as_mut().unwrap();
    triage.issues[0].asked = Some(TriageAsked {
        at: Timestamp(1_792_051_200),
        asks: 2,
    });
    triage.issues.push(Triaged {
        issue: 13,
        comment: 5002,
        step: TriageStep::Answers,
        at: Timestamp(0),
        author: String::new(),
        held: None,
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: None,
        ready_body: None,
        seen: Vec::new(),
        asked: None,
        questions: Some(TriageQuestions {
            comment: 5010,
            at: Timestamp(1_792_054_800),
        }),
        local: None,
        spam: None,
        begun: None,
    });
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"]["issues"],
        serde_json::json!([
            {
                "issue": 12, "comment": 5001, "at": 0, "step": "commented", "labels": ["bug"],
                "asked": { "at": 1_792_051_200, "asks": 2 },
            },
            {
                "issue": 13, "comment": 5002, "at": 0, "step": "answers",
                "questions": { "comment": 5010, "at": 1_792_054_800 },
            },
        ])
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 67 file, saved before the local round, has records with no `local`
// wait, so they load with none and save again at the current version. A waiting
// record keeps the time its prompt went up and the hash of the body it was
// posted against, and the wait is left out while unset.
#[test]
fn a_file_saved_before_this_version_loads_with_no_local_wait() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 67.into();
    old["triage"] = serde_json::json!({
        "since": 1_791_964_800,
        "issues": [
            { "issue": 12, "comment": 5001, "step": "commented", "labels": ["bug"] },
            { "issue": 13, "comment": 5002, "step": "done" },
        ],
    });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    let triage = loaded.triage.as_ref().expect("the records");
    assert!(
        triage.issues.iter().all(|t| t.local.is_none()),
        "{:?}",
        triage.issues
    );

    let triage = loaded.triage.as_mut().unwrap();
    triage.issues[0].local = Some(TriageLocal {
        at: Timestamp(1_792_051_200),
        body: 18_446_744_073_709_551_615,
    });
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"]["issues"],
        serde_json::json!([
            {
                "issue": 12, "comment": 5001, "at": 0, "step": "commented", "labels": ["bug"],
                "local": { "at": 1_792_051_200, "body": 18_446_744_073_709_551_615_u64 },
            },
            { "issue": 13, "comment": 5002, "at": 0, "step": "done" },
        ])
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A version 68 file has records with no `at`, no `author`, no `held` and no
// `calls`, so they load with none of them: no time, so no cap counts them, and
// no author, so no author's count reaches them. A record a limit held keeps the
// limit it was held by, with no comment id and no call behind it.
#[test]
fn a_file_saved_before_this_version_loads_with_no_limit_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 68.into();
    old["triage"] = serde_json::json!({
        "since": 1_791_964_800,
        "issues": [{ "issue": 12, "comment": 5001, "step": "done" }],
    });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    let triage = loaded.triage.as_mut().expect("the records");
    assert_eq!(triage.issues[0].at, Timestamp(0), "no time to count");
    assert_eq!(triage.issues[0].author, "", "no author to count");
    assert_eq!(triage.issues[0].held, None);
    assert_eq!(triage.issues[0].calls, [], "no call to count");

    triage.issues.push(Triaged {
        issue: 13,
        comment: 0,
        step: TriageStep::Done,
        at: Timestamp(1_792_051_200),
        author: "newcomer".to_owned(),
        held: Some(TriageHeld::AuthorCap),
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: None,
        ready_body: None,
        seen: Vec::new(),
        asked: None,
        questions: None,
        local: None,
        spam: None,
        begun: None,
    });
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"]["issues"],
        serde_json::json!([
            { "issue": 12, "comment": 5001, "at": 0, "step": "done" },
            {
                "issue": 13, "comment": 0, "at": 1_792_051_200, "author": "newcomer",
                "step": "done", "held": "author-cap",
            },
        ])
    );
    assert_eq!(store.load().unwrap(), Some(loaded));
}

// A record keeps the time of every call its issue made, oldest first, so the
// caps count an author's round beside the first pass's call. Past
// `TRIAGE_CALLS_KEPT` the oldest times go.
#[test]
fn a_record_keeps_the_time_of_every_call_its_issue_made() {
    let mut record = Triaged {
        issue: 12,
        comment: 5001,
        step: TriageStep::Done,
        at: Timestamp(1_792_051_200),
        author: "newcomer".to_owned(),
        held: None,
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: None,
        ready_body: None,
        seen: Vec::new(),
        asked: None,
        questions: None,
        local: None,
        spam: None,
        begun: None,
    };
    for n in 0..(TRIAGE_CALLS_KEPT + 3) {
        record.add_call(Timestamp(1_792_051_200 + n as u64));
    }

    assert_eq!(record.calls.len(), TRIAGE_CALLS_KEPT);
    assert_eq!(
        record.calls.first().cloned().unwrap(),
        Timestamp(1_792_051_200 + 3),
        "the oldest calls went first"
    );
    assert_eq!(
        record.calls.last().cloned().unwrap(),
        Timestamp(1_792_051_200 + TRIAGE_CALLS_KEPT as u64 + 2)
    );
}

// A file saved before version 76 does not say what a prune dropped. A project
// holding the cap's worth of finished records may have pruned some, so it loads
// with the highest recorded issue as the floor; a project short of the cap has
// pruned none and loads with none.
#[test]
fn a_file_saved_before_pruned_through_gets_a_floor_only_when_a_prune_may_have_run() {
    let load = |finished: u64| {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut old = serde_json::to_value(ProjectState::new()).unwrap();
        old["version"] = 75.into();
        old["triage"] = serde_json::json!({
            "since": 1,
            "issues": (0..finished)
                .map(|n| serde_json::json!({
                    "issue": 100 + n, "comment": 1, "at": 0, "step": "done"
                }))
                .collect::<Vec<_>>(),
        });
        fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
        store
            .load()
            .unwrap()
            .unwrap()
            .triage
            .unwrap()
            .pruned_through
    };

    assert_eq!(load(TRIAGED_KEPT as u64), 100 + TRIAGED_KEPT as u64 - 1);
    assert_eq!(load(3), 0);
}

// A record kelpie made when it took a hand-labelled `ready-for-agent` issue
// back saves as the `taken` step, holds no comment, and is not a finished
// record, so a prune keeps it until its first pass replaces it.
#[test]
fn a_taken_back_record_saves_as_taken_and_is_not_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let record = |issue: u64, step: TriageStep| Triaged {
        issue,
        comment: 0,
        step,
        at: Timestamp(0),
        author: "Boss".to_owned(),
        held: None,
        calls: Vec::new(),
        labels: Vec::new(),
        duplicate_of: None,
        ready_body: None,
        seen: vec!["needs-triage".to_owned()],
        asked: None,
        questions: None,
        local: None,
        spam: None,
        begun: None,
    };
    let mut state = ProjectState::new();
    let cap = TRIAGED_KEPT as u64;
    let mut triage = TriageState {
        since: Timestamp(1),
        issues: vec![record(1, TriageStep::Taken)],
        unrecorded: Vec::new(),
        pruned_through: 0,
        splits: Vec::new(),
    };
    triage
        .issues
        .extend((2..cap + 5).map(|n| record(n, TriageStep::Done)));
    triage.prune();
    assert_eq!(triage.issues[0].issue, 1, "a taken record stays");
    state.triage = Some(triage);
    store.save(&state).unwrap();

    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        saved["triage"]["issues"][0],
        serde_json::json!({
            "issue": 1, "comment": 0, "at": 0, "author": "Boss", "step": "taken",
            "seen": ["needs-triage"]
        })
    );
    assert_eq!(store.load().unwrap(), Some(state));
}

// A split saves as its drafts and the children filed so far, and a file saved
// before splits existed loads with none. An empty `filed` and `asked` are left
// out.
#[test]
fn a_saved_split_survives_a_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 102.into();
    old["triage"] = serde_json::json!({ "since": 7 });
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();
    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.triage.map(|t| t.splits), Some(Vec::new()));

    let mut state = ProjectState::new();
    state.triage = Some(TriageState {
        since: Timestamp(7),
        issues: Vec::new(),
        unrecorded: Vec::new(),
        pruned_through: 0,
        splits: vec![TriageSplit {
            issue: 30,
            drafts: vec![
                SplitDraft {
                    title: "feat(a): first".to_owned(),
                    body: "Touches: `a.rs`".to_owned(),
                    labels: vec!["kind:feat".to_owned()],
                    after: Vec::new(),
                },
                SplitDraft {
                    title: "feat(b): second".to_owned(),
                    body: "Touches: `b.rs`".to_owned(),
                    labels: Vec::new(),
                    after: vec![0],
                },
            ],
            filed: vec![crate::test::ticket(900)],
            asked: None,
        }],
    });
    store.save(&state).unwrap();

    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["triage"]["splits"],
        serde_json::json!([{
            "issue": 30,
            "drafts": [
                { "title": "feat(a): first", "body": "Touches: `a.rs`", "labels": ["kind:feat"] },
                { "title": "feat(b): second", "body": "Touches: `b.rs`", "after": [0] }
            ],
            "filed": ["900"]
        }])
    );
    assert_eq!(store.load().unwrap(), Some(state));
}
