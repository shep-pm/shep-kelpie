//! The dog's answer to a `fit` ask, and the marks of what it has answered
//!
//! The board holds every project's records, so it is the one place that can
//! read a pair of agents, and the one that decides which runner raises the
//! ruling. A mark is the worse agent's items of the kind when the dog last
//! answered a finding for it: the dog writes it before it answers, so only
//! one project of many raises a finding for the pair.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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

/// Answers a runner's `fit` ask and ends the visit
pub(in crate::dog) async fn visit(place: &Place, ask: Ask, write: &mut ServerWriteHalf) {
    let Some(ask) = ask.fit else {
        let why = "a fit ask names the agent and the kind".to_owned();
        return say(write, &Answer::Error(why)).await;
    };
    let raised = raises(&lock(&place.board).view(), &ask);
    let Some((finding, items)) = raised else {
        return say(write, &Answer::Fits(Fits::Yes)).await;
    };
    // The mark goes down before the answer, so the next runner to ask about
    // this pair is the one that raises nothing.
    let marks = {
        let mut board = lock(&place.board);
        board
            .marks
            .insert((ask.agent.clone(), ask.kind.clone()), items);
        board.marks.clone()
    };
    if let Err(e) = save(&marks_file(&place.kelpie_home), &marks) {
        println!("{e}: the mark stands until the dog writes it again");
    }
    say(write, &Answer::Fits(Fits::No(finding))).await;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn name(agent: &str) -> AgentName {
        AgentName::try_from(agent.to_owned()).unwrap()
    }

    fn marks() -> Marks {
        Marks::from([
            ((name("slow"), "fix".to_owned()), 12),
            ((name("quick"), "docs".to_owned()), 30),
        ])
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
}
