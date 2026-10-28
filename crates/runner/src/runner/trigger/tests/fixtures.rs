//! The JSON each trigger answers, recorded from `main` before the wire types landed
//!
//! A fixture is what a rig answers, with what a run makes different marked out:
//! its temp folder, the build and commit the test ran from, the session ids it
//! made, the head its git stand-in committed, and the holds of the runner's
//! lock, which are how long this run's passes took. The `attach` fixtures name
//! a project under `/home/me`, since a live `attach` asks `ps`, which a
//! worker's sandbox cannot run.

use std::path::PathBuf;

use super::*;
use crate::ports::BranchRun;
use crate::runner::{Attaching, PmAttaching};
use crate::state::ProjectState;
use crate::terminal::Foreground;

const STATUS: &str = include_str!("../../../../../protocol/fixtures/status.json");
const TIMINGS: &str = include_str!("../../../../../protocol/fixtures/timings.json");
const BOARD: &str = include_str!("../../../../../protocol/fixtures/board.json");
const ATTACH: &str = include_str!("../../../../../protocol/fixtures/attach.json");
const PM_ATTACH: &str = include_str!("../../../../../protocol/fixtures/pm-attach.json");
const REFUSAL: &str = include_str!("../../../../../protocol/fixtures/refusal.json");

// A fixture as the answer it was recorded from.
fn parse(shown: &str) -> Value {
    serde_json::from_str(shown).expect("a recorded fixture is JSON")
}

pub(in crate::runner) fn status() -> Value {
    parse(STATUS)
}

pub(in crate::runner) fn timings() -> Value {
    parse(TIMINGS)
}

pub(in crate::runner) fn board() -> Value {
    parse(BOARD)
}

pub(super) fn attach() -> Value {
    parse(ATTACH)
}

pub(super) fn pm_attach() -> Value {
    parse(PM_ATTACH)
}

pub(in crate::runner) fn refusal() -> Value {
    parse(REFUSAL)
}

/// `answer` marked out as [`settled`] marks it, so a test can read it back
pub(super) fn asked(runner: &Mutex<Runner>, postings: &Posted, action: &str, head: &str) -> Value {
    let body = answer(runner, postings, action, None);
    let mut shown = parse(&body);
    settled(&mut shown, head);
    shown
}

/// Marks out what a run of the rig makes different from the run that recorded
/// the fixture, and drops what this run alone decides
pub(in crate::runner) fn settled(value: &mut Value, head: &str) {
    match value {
        Value::Object(fields) => {
            // How long this run's passes held the lock, which no fixture can name.
            fields.remove("lock");
            for key in ["board", "folder", "worktree", "build", "commit", "session"] {
                let marker = match key {
                    "build" => "<build>",
                    "commit" => "<commit>",
                    "session" => "<session>",
                    _ => "<tempdir>",
                };
                if matches!(fields.get(key), Some(Value::String(_))) {
                    fields.insert(key.to_owned(), json!(marker));
                }
            }
            for ruling in fields
                .get_mut("rulings")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if ruling.get("kind").is_some_and(|k| k.get("head").is_some()) {
                    ruling["kind"]["head"] = json!("<head>");
                }
                if let Some(question) = ruling.get("question").and_then(Value::as_str) {
                    ruling["question"] = json!(match head.is_empty() {
                        true => question.to_owned(),
                        false => question
                            .replace(head, "<head>")
                            .replace(head.get(..7).unwrap_or(head), "<head>"),
                    });
                }
            }
            for field in fields.values_mut() {
                settled(field, head);
            }
        }
        Value::Array(items) => {
            for item in items {
                settled(item, head);
            }
        }
        _ => {}
    }
}

/// Work items that finished before the rig's clock began, 101 on
fn earlier(count: u64) -> Vec<Finished> {
    (101..101 + count)
        .map(|issue| Finished {
            issue: ticket(issue),
            title: format!("Issue {issue}"),
            pull_request: Some(issue + 60),
            merged: true,
            closed: false,
            at: Timestamp(Rig::EPOCH - 1000 + issue),
            wall: 600,
            seconds: Seconds::default(),
            spend: None,
        })
        .collect()
}

// A project with `count` work items finished before its clock began
pub(in crate::runner) fn seed(rig: &Rig, count: u64) {
    let mut state = ProjectState::new();
    state.history = earlier(count);
    let file = rig.paths().state;
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    StateStore::new(file).save(&state).unwrap();
}

// The project manager the fixture's `pm` names
fn name_pm(rig: &Rig) {
    rig.edit_settings(|s| {
        let listed = "\nimplementers = [\"sonnet-high\"]\n";
        assert!(s.contains(listed), "the example's implementers moved");
        s.replace(listed, "\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n")
    });
}

/// A project manager, a red workflow on `main`, and three finished work items
pub(in crate::runner) fn rich(rig: &Rig) {
    name_pm(rig);
    seed(rig, 3);
    rig.forge.set_branch_runs(
        "main",
        vec![BranchRun {
            workflow: "test".into(),
            id: 91,
            head: "head91".into(),
            event: "push".into(),
            conclusion: "failure".into(),
            created: Timestamp(Rig::EPOCH - 3 * 3600),
        }],
    );
}

/// Two ready issues blocked by #32, which is open
pub(in crate::runner) fn ready(rig: &Rig) {
    rig.forge.open_issue(32, "Blocker", "Body of #32.\n");
    for issue in [282, 284] {
        rig.forge.list_ready_unchecked(issue, false);
        rig.forge.block(issue, 32);
    }
}

// The session the fixture names, and the command it names, for a project under
// `/home/me`. A live `attach` asks `ps`, which a worker's sandbox
// cannot run, so the fixture names a project rather than this run's temp folder.
fn session() -> SessionId {
    SessionId("11111111-2222-4333-8444-555555555555".to_owned())
}

fn command(settings: &str, cwd: &str) -> Foreground {
    Foreground {
        program: "claude".to_owned(),
        args: vec![
            "--settings".to_owned(),
            settings.to_owned(),
            "--resume".to_owned(),
            session().0,
        ],
        cwd: PathBuf::from(cwd),
        env: BTreeMap::new(),
    }
}

/// What `attach` answers for a held work item whose session is ready to run
pub(super) fn attaching() -> Attaching {
    Attaching::Ready {
        issue: ticket(7),
        session: session(),
        worktree: PathBuf::from("/home/me/koji/.kelpie/worktrees/7"),
        command: command(
            "/home/me/koji/.kelpie/workers/settings-7.json",
            "/home/me/koji/.kelpie/worktrees/7",
        ),
    }
}

/// What `pm attach` answers for a held project manager whose session is ready
pub(super) fn pm_attaching() -> PmAttaching {
    PmAttaching::Ready {
        session: session(),
        folder: PathBuf::from("/home/me/koji/.kelpie/pm"),
        command: command(
            "/home/me/koji/.kelpie/pm/settings.json",
            "/home/me/koji/.kelpie/pm",
        ),
    }
}
