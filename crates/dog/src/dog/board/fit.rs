//! The dog's answer to a `fit` ask, and the marks of what it has answered
//!
//! The board holds every project's records, so it is the one place that can
//! read a pair of agents, and the one that decides which runner raises the
//! ruling. A mark is the worse agent's items of the kind when the dog last
//! answered a finding for it: the dog reads the mark, takes it and writes the
//! file under the board's one lock before it answers, so only one project of
//! many raises a finding for the pair.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{Board, Marks, View, lock};
use crate::agent_board::{self, FIT_AGAIN, FIT_ITEMS, Finding};
use crate::dog::door::{Place, say};
use crate::ipc::ServerWriteHalf;
use crate::lease::door::{Answer, Ask, FitAsk, Fits};
use crate::settings::AgentName;
use crate::state::write_atomically;

/// The dog's marks file, under kelpie's home beside its book
const MARKS: &str = "dog/fit.json";

/// The marks file's format version
const VERSION: u32 = 1;

/// The marks as the file holds them
// wire format: changing this is a breaking change to the marks file
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    /// Each agent and kind, in the order the dog keeps them
    answered: Vec<SavedMark>,
}

/// One agent and kind the dog answered a finding for
// wire format: changing this is a breaking change to the marks file
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedMark {
    /// The agent
    agent: String,
    /// The kind
    kind: String,
    /// Its items of that kind when the dog answered
    items: u32,
}

/// Where the dog keeps its marks under `kelpie_home`
fn marks_file(kelpie_home: &Path) -> PathBuf {
    kelpie_home.join(MARKS)
}

/// Reads the marks under `kelpie_home` into `board`, saying what it could not read
pub(in crate::dog) fn load_marks(board: &mut Board, kelpie_home: &Path) {
    let (marks, why) = read(&marks_file(kelpie_home));
    if let Some(why) = why {
        eprintln!("{why}");
    }
    board.marks = marks;
}

/// The marks in `file`, and a line saying so when it cannot be read
///
/// No file marks nothing. One that cannot be read, that another build wrote,
/// or that names an agent kelpie no longer knows marks nothing of that and
/// says so: the dog answers as if it had raised nothing yet.
fn read(file: &Path) -> (Marks, Option<String>) {
    let none = |why: Option<String>| (Marks::new(), why);
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return none(None),
        Err(e) => return none(Some(format!("cannot read {}: {e}", file.display()))),
    };
    let saved = match serde_json::from_str::<Saved>(&text) {
        Ok(saved) if saved.version == VERSION => saved,
        Ok(_) => return none(Some(format!("{} is from another build", file.display()))),
        Err(e) => return none(Some(format!("{} is not marks: {e}", file.display()))),
    };
    let mut marks = Marks::new();
    let mut dropped = Vec::new();
    for SavedMark { agent, kind, items } in saved.answered {
        match AgentName::try_from(agent.clone()) {
            Ok(agent) => _ = marks.insert((agent, kind), items),
            Err(e) => dropped.push(format!("{agent}: {e}")),
        }
    }
    let why =
        (!dropped.is_empty()).then(|| format!("{}: answering without them", dropped.join(", ")));
    (marks, why)
}

/// Writes `marks` to `file`
///
/// # Errors
///
/// What writing the file failed with. The marks the dog is holding stand either way.
fn save(file: &Path, marks: &Marks) -> io::Result<()> {
    let answered = (marks.iter())
        .map(|((agent, kind), items)| SavedMark {
            agent: agent.as_str().to_owned(),
            kind: kind.clone(),
            items: *items,
        })
        .collect();
    let saved = Saved {
        version: VERSION,
        answered,
    };
    let mut bytes = serde_json::to_vec_pretty(&saved).expect("the marks serialize to JSON");
    bytes.push(b'\n');
    write_atomically(file, &bytes)
}

// The finding the dog raises for `ask`, with the agent's items of the kind,
// or None when it raises nothing: no record of that agent and kind, one with
// fewer than [`FIT_ITEMS`] items, a pair answered within [`FIT_AGAIN`] of
// them, or no agent the board reads as doing clearly worse at it.
fn raises(view: &View, ask: &FitAsk) -> Option<(Finding, u32)> {
    let tallies = view.tallies();
    let key = (ask.agent.as_str().to_owned(), Some(ask.kind.clone()));
    let items = tallies.get(&key)?.items;
    if items < FIT_ITEMS {
        return None;
    }
    let last = view
        .marks
        .get(&(ask.agent.clone(), ask.kind.clone()))
        .copied();
    if last.is_some_and(|last| items < last.saturating_add(FIT_AGAIN)) {
        return None;
    }
    let takers = (view.implementers()).map(|(name, agent)| agent_board::Taker {
        agent: name.as_str(),
        fit: &agent.fit,
    });
    (agent_board::fit(&tallies, takers).into_iter())
        .find(|finding| finding.worse == ask.agent.as_str() && finding.kind == ask.kind)
        .map(|finding| (finding, items))
}

// The finding the dog raises for `ask`, having marked the pair and written the
// marks, all under the board's one lock: two runners asking about one pair
// cannot both read no mark, and no save can drop another's mark. A save that
// fails says so, and the mark the dog holds stands until the next one.
fn raise(board: &Mutex<Board>, kelpie_home: &Path, ask: &FitAsk) -> Option<Finding> {
    let mut board = lock(board);
    let (finding, items) = raises(&board.view(), ask)?;
    board
        .marks
        .insert((ask.agent.clone(), ask.kind.clone()), items);
    if let Err(e) = save(&marks_file(kelpie_home), &board.marks) {
        println!("{e}: the mark stands until the dog writes the marks file again");
    }
    Some(finding)
}

/// Answers a runner's `fit` ask and ends the visit
pub(in crate::dog) async fn visit(place: &Place, ask: Ask, write: &mut ServerWriteHalf) {
    let Some(ask) = ask.fit else {
        let why = "a fit ask names the agent and the kind".to_owned();
        return say(write, &Answer::Error(why)).await;
    };
    match raise(&place.board, &place.kelpie_home, &ask) {
        Some(finding) => say(write, &Answer::Fits(Fits::No(finding))).await,
        None => say(write, &Answer::Fits(Fits::Yes)).await,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agents::Agents;
    use crate::dog::board::{refresh, tests::finished};
    use crate::test::write_in;

    fn name(agent: &str) -> AgentName {
        AgentName::try_from(agent.to_owned()).unwrap()
    }

    fn marks() -> Marks {
        Marks::from([
            ((name("slow"), "fix".to_owned()), 12),
            ((name("quick"), "docs".to_owned()), 30),
        ])
    }

    // An implementer on the stand-in harness whose file names `fit`'s lines.
    fn implementer(fit: &str) -> String {
        format!(
            "---\nrole: implementer\nharness: stand-in\nmodel: lite\neffort: low\n\
             usage: none\n{fit}---\n"
        )
    }

    // A test dog's kelpie home, with the `dog` folder a real dog's start makes.
    fn dog_home(dir: &Path) -> PathBuf {
        let home = dir.join("kelpie");
        fs::create_dir_all(home.join("dog")).unwrap();
        home
    }

    // `slow`, whose file takes `fix` and `docs`, `quick`, which takes `fix`,
    // and `bulk`, which takes `docs`.
    fn trio(dir: &Path) -> Agents {
        write_in(dir, "slow.md", &implementer("kinds: [fix, docs]\n"));
        write_in(dir, "quick.md", &implementer("kinds: [fix]\n"));
        write_in(dir, "bulk.md", &implementer("kinds: [docs]\n"));
        Agents::load(dir).unwrap()
    }

    // The board of `agents`: each pair of them has ten items of a kind, the
    // worse one of each pair taking twelve times as long at it.
    fn board_of(agents: &Agents, home: &Path) -> Mutex<Board> {
        finished(home, "koji", ("slow", "fix"), 10, (7200, 0.5));
        finished(home, "koji", ("quick", "fix"), 10, (600, 0.5));
        finished(home, "koji", ("slow", "docs"), 10, (600, 0.5));
        finished(home, "koji", ("bulk", "docs"), 10, (7200, 0.5));
        let board = Mutex::new(Board::new(agents));
        refresh(&board, home);
        board
    }

    fn asked(agent: &str, kind: &str) -> FitAsk {
        FitAsk {
            agent: name(agent),
            kind: kind.to_owned(),
        }
    }

    #[test]
    fn the_marks_write_and_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = marks_file(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();

        save(&file, &marks()).unwrap();

        let (read, why) = read(&file);
        assert_eq!((read, why), (marks(), None));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&file).unwrap()).unwrap(),
            json!({
                "version": 1,
                "answered": [
                    { "agent": "quick", "kind": "docs", "items": 30 },
                    { "agent": "slow", "kind": "fix", "items": 12 },
                ],
            }),
            "the file the dog leaves behind"
        );
    }

    #[test]
    fn no_marks_file_marks_nothing_and_says_nothing() {
        let dir = tempfile::tempdir().unwrap();

        let (read, why) = read(&marks_file(dir.path()));

        assert_eq!((read, why), (Marks::new(), None));
    }

    #[test]
    fn a_marks_file_that_cannot_be_read_marks_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let file = marks_file(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not marks at all").unwrap();

        let (read, why) = read(&file);

        assert_eq!(read, Marks::new());
        let why = why.expect("a line saying the file is no marks");
        assert!(why.contains("is not marks"), "{why}");
    }

    #[test]
    fn a_marks_file_another_build_wrote_marks_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let file = marks_file(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, json!({ "version": 2, "answered": [] }).to_string()).unwrap();

        let (read, why) = read(&file);

        assert_eq!(read, Marks::new());
        let why = why.expect("a line saying another build wrote it");
        assert!(why.contains("another build"), "{why}");
    }

    #[test]
    fn a_mark_for_an_agent_kelpie_no_longer_knows_is_dropped_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let file = marks_file(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            json!({
                "version": 1,
                "answered": [{ "agent": "not a name!", "kind": "fix", "items": 12 }],
            })
            .to_string(),
        )
        .unwrap();

        let (read, why) = read(&file);

        assert_eq!(read, Marks::new());
        let why = why.expect("a line naming the mark it dropped");
        assert!(why.contains("not a name!"), "{why}");
    }

    #[test]
    fn two_runners_asking_about_one_pair_at_once_raise_one_finding() {
        let dir = tempfile::tempdir().unwrap();
        let home = dog_home(dir.path());
        let agents = trio(dir.path());
        let board = board_of(&agents, &home);

        let raised = std::thread::scope(|scope| {
            let one = scope.spawn(|| raise(&board, &home, &asked("slow", "fix")).is_some());
            let two = scope.spawn(|| raise(&board, &home, &asked("slow", "fix")).is_some());
            (one.join().unwrap(), two.join().unwrap())
        });

        assert_eq!(
            usize::from(raised.0) + usize::from(raised.1),
            1,
            "one runner of many raises a finding for the pair"
        );
    }

    #[test]
    fn two_runners_raising_at_once_leave_both_marks_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dog_home(dir.path());
        let agents = trio(dir.path());
        let board = board_of(&agents, &home);

        let raised = std::thread::scope(|scope| {
            let fix = scope.spawn(|| raise(&board, &home, &asked("slow", "fix")).is_some());
            let docs = scope.spawn(|| raise(&board, &home, &asked("bulk", "docs")).is_some());
            (fix.join().unwrap(), docs.join().unwrap())
        });

        let (saved, why) = read(&marks_file(&home));
        assert_eq!(
            (raised, saved, why),
            (
                (true, true),
                Marks::from([
                    ((name("slow"), "fix".to_owned()), 10),
                    ((name("bulk"), "docs".to_owned()), 10),
                ]),
                None,
            ),
            "the save that lands last holds both pairs, not only the one it raised"
        );
    }
}
