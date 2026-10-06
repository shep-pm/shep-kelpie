//! Rulings answered while the runner was stopped
//!
//! `shep kelpie rule` reaches a stopped runner through its project's
//! `answers` folder: [`leave`] writes `rule`'s params to a file named for
//! the ruling, so a second answer to one ruling replaces the first. Each
//! pass, its first at the runner's start included, answers every file
//! there as the `rule` trigger would, then removes it, and logs what came
//! of each. A file that cannot be read, or whose answer cannot be saved,
//! stays for the next pass.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::trigger::{lock, number, read_rule};
use super::{RuleError, Runner};
use crate::state::write_atomically;
use crate::totp::private_dir;

/// Leaves `params`, `rule`'s params for one ruling, in `answers` for the
/// runner to act on when it starts
///
/// # Errors
///
/// The OS's reason when `params` names no ruling's id, or the file cannot
/// be written.
pub fn leave(answers: &Path, params: &str) -> io::Result<()> {
    let id = params.split_once(' ').map_or(params, |(id, _)| id);
    let Some(id) = id_of(id) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{params:?} does not start with a ruling's id"),
        ));
    };
    private_dir(answers)?;
    write_atomically(&answers.join(id.to_string()), params.as_bytes())
}

// A ruling's id: digits only, and not 0, so a write's temporary file is not one.
fn id_of(name: &str) -> Option<u64> {
    number(name)
}

/// What `answer_left` has seen in the folder, in memory only
#[derive(Debug, Default)]
pub(super) struct Seen {
    // The entries that are no answer, each logged once
    told: BTreeSet<String>,
}

/// Answers each ruling an answer was left for, oldest id first, and
/// removes its file once the answer is taken or refused
///
/// A file that cannot be read, or an answer whose save fails, stays for the
/// next pass. A write's temporary file a minute old is removed, and any
/// other entry is logged once.
pub(super) fn answer_left(runner: &Mutex<Runner>) {
    let answers = lock(runner).paths.answers.clone();
    let Ok(entries) = fs::read_dir(&answers) else {
        return;
    };
    let mut left: Vec<(u64, String)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        match id_of(&name) {
            Some(id) => left.push((id, name)),
            None => not_an_answer(runner, &answers, &name),
        }
    }
    left.sort_unstable();
    for (id, name) in left {
        let file = answers.join(name);
        let (note, done) = match fs::read_to_string(&file) {
            Ok(params) => taken(runner, id, &params),
            Err(e) => (
                format!("cannot read the answer left for ruling {id}, so it stays: {e}"),
                false,
            ),
        };
        let gone = done.then(|| fs::remove_file(&file).err()).flatten();
        let gone =
            gone.map(|e| format!(" (its file {} could not be removed: {e})", file.display()));
        lock(runner)
            .notes
            .push(format!("{note}{}", gone.unwrap_or_default()));
    }
}

// Removes `name` from `answers` when it is a write's temporary file a minute
// old, and logs anything else there once.
fn not_an_answer(runner: &Mutex<Runner>, answers: &Path, name: &str) {
    const STALE: Duration = Duration::from_secs(60);
    let path = answers.join(name);
    let temporary = name
        .strip_suffix(".tmp")
        .is_some_and(|id| id_of(id).is_some());
    let age = fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok());
    if temporary && age.is_some_and(|age| age >= STALE) && fs::remove_file(&path).is_ok() {
        return;
    }
    if temporary {
        return;
    }
    let mut runner = lock(runner);
    if runner.left.told.insert(name.to_owned()) {
        runner.notes.push(format!(
            "{} is no answer `shep kelpie rule` left, so the runner leaves it there",
            path.display()
        ));
    }
}

// What came of the answer `params` left for ruling `id`, and whether its
// file is done with: kept only when the answer could not be saved.
fn taken(runner: &Mutex<Runner>, id: u64, params: &str) -> (String, bool) {
    let refused = |why: String| {
        format!("the answer to ruling {id} left while the runner was stopped was not taken: {why}")
    };
    let Some((named, answer)) = read_rule(params.trim()).filter(|(named, _)| *named == id) else {
        return (refused(format!("{params:?} is not an answer to it")), true);
    };
    match lock(runner).rule(named, answer) {
        Ok(()) => (
            format!(
                "answered ruling {id} as `shep kelpie rule` left it while the runner was stopped"
            ),
            true,
        ),
        Err(RuleError::State(e)) => (
            format!(
                "the answer to ruling {id} could not be saved, so it stays for the next pass: {e}"
            ),
            false,
        ),
        Err(e) => (refused(e.to_string()), true),
    }
}

#[cfg(test)]
mod tests;
