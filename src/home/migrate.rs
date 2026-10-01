//! The one-time move from `~/.kelpie` into kelpie's home under shep's
//!
//! Only what kelpie's code reads or writes moves, named one by one, since the
//! old folder also holds files kelpie does not own. A move is a rename, so
//! nothing is copied, and an item whose new place is taken stays where it is.
//! Each shared file a runner still on the old build reads gets a link from
//! its old place, until every runner has restarted. Two starts at once take
//! turns on a lock, and a second run finds nothing left to move.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use nix::fcntl::{Flock, FlockArg};
use serde_json::Value;

use crate::runner::ProjectName;
use crate::state::write_atomically;

/// Kelpie's own files, which every project shares, each linked back from its
/// old place for a runner still on the old build
const SHARED: [&str; 6] = [
    "settings.toml",
    "totp",
    "tools",
    "codex",
    "relay",
    "rulings",
];

/// The build the last upgrade replaced, which only `upgrade --rollback` reads
const PREVIOUS_BUILD: &str = "builds/shep-kelpie.previous";

/// A project's old folders, by the name its own folder now holds each under
const PROJECT: [(&str, &str); 4] = [
    ("wt", "worktrees"),
    ("targets", "builds"),
    ("shots", "shots"),
    ("playwright", "playwright"),
];

/// One file or folder to move, and whether its old place keeps a link to it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    from: PathBuf,
    to: PathBuf,
    link: bool,
    // How many folders above `from` are kelpie's, removed once empty
    empties: usize,
}

/// Kelpie's shared files, from the home `old` to the home `new`
pub fn shared(old: &Path, new: &Path) -> Vec<Move> {
    let linked = SHARED.iter().map(|name| Move {
        from: old.join(name),
        to: new.join(name),
        link: true,
        empties: 0,
    });
    let previous = Move {
        from: old.join(PREVIOUS_BUILD),
        to: new.join(PREVIOUS_BUILD),
        link: false,
        empties: 0,
    };
    linked.chain([previous]).collect()
}

/// Project `name`'s files, from the old layout under `old` to its own folder under `new`
///
/// Its state, settings and worker files were in `projects/<name>`, and its
/// worktrees, build folders, shots and Playwright files each in a folder of
/// their own, under the project's name.
pub fn project(old: &Path, new: &Path, name: &ProjectName) -> Vec<Move> {
    let folder = new.join(name.as_str());
    let kept = old.join("projects").join(name.as_str());
    let entries = fs::read_dir(&kept).into_iter().flatten().flatten();
    let mut moves: Vec<Move> = entries
        .map(|entry| Move {
            from: entry.path(),
            to: folder.join(entry.file_name()),
            link: false,
            empties: 2,
        })
        .collect();
    moves.sort_by(|a, b| a.from.cmp(&b.from));
    moves.extend(PROJECT.iter().map(|(was, now)| Move {
        from: old.join(was).join(name.as_str()),
        to: folder.join(now),
        link: false,
        empties: 1,
    }));
    moves
}

/// The dog's book, from the old home `old` to the dog's folder `dog`
pub fn dog(old: &Path, dog: &Path) -> Vec<Move> {
    vec![Move {
        from: old.join("dog/book.json"),
        to: dog.join("book.json"),
        link: false,
        empties: 1,
    }]
}

/// Makes `moves` into kelpie's home `new`, one start at a time, and says what moved
///
/// An item that is gone, or is already a link, is skipped. The old folders
/// left empty are removed, and nothing else in them is touched.
///
/// # Errors
///
/// A message when kelpie's home or its lock cannot be made or taken.
pub fn run(new: &Path, moves: &[Move]) -> Result<Vec<String>, String> {
    let pending: Vec<&Move> = moves.iter().filter(|m| waiting(m)).collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    fs::create_dir_all(new).map_err(|e| format!("cannot make {}: {e}", new.display()))?;
    let path = new.join(".migrate.lock");
    let file = File::create(&path).map_err(|e| format!("cannot make {}: {e}", path.display()))?;
    let _lock = Flock::lock(file, FlockArg::LockExclusive)
        .map_err(|(_, e)| format!("cannot lock {}: {e}", path.display()))?;
    // Another start may have moved them while this one waited.
    let lines = pending
        .into_iter()
        .filter(|m| waiting(m))
        .filter_map(one)
        .collect();
    for m in moves {
        remove_empty_parents(&m.from, m.empties);
    }
    Ok(lines)
}

/// Points project `name`'s state file, under `new`, at its moved folders
///
/// A work item keeps its worktree's and build folder's paths, so each path
/// under an old folder of the project's is rewritten to the new one. Says
/// so when it changed the file, and does nothing the second time.
///
/// # Errors
///
/// A message when the file is there and cannot be read, parsed or written.
pub fn repoint(old: &Path, new: &Path, name: &ProjectName) -> Result<Option<String>, String> {
    let file = new.join(name.as_str()).join("state.json");
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let mut state: Value =
        serde_json::from_str(&text).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let folder = new.join(name.as_str());
    let pairs: Vec<(PathBuf, PathBuf)> = PROJECT
        .iter()
        .map(|(was, now)| (old.join(was).join(name.as_str()), folder.join(now)))
        .collect();
    if !rewrite(&mut state, &pairs) {
        return Ok(None);
    }
    let text = serde_json::to_vec_pretty(&state).expect("a JSON value serializes");
    write_atomically(&file, &text).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(Some(format!(
        "pointed {}'s work items at {}",
        name,
        folder.display()
    )))
}

// Rewrites every string in `value` that is a path under a pair's first to
// the same path under its second. Returns whether any changed.
fn rewrite(value: &mut Value, pairs: &[(PathBuf, PathBuf)]) -> bool {
    match value {
        Value::String(text) => {
            let path = Path::new(text.as_str());
            let moved = pairs.iter().find_map(|(was, now)| {
                let rest = path.strip_prefix(was).ok()?;
                Some(if rest.as_os_str().is_empty() {
                    now.clone()
                } else {
                    now.join(rest)
                })
            });
            let Some(moved) = moved else { return false };
            *text = moved.display().to_string();
            true
        }
        Value::Array(items) => items
            .iter_mut()
            .fold(false, |any, v| rewrite(v, pairs) | any),
        Value::Object(map) => map
            .values_mut()
            .fold(false, |any, v| rewrite(v, pairs) | any),
        _ => false,
    }
}

// Whether `m` still has something to move: its old place is there and is not a link.
fn waiting(m: &Move) -> bool {
    m.from != m.to && fs::symlink_metadata(&m.from).is_ok_and(|meta| !meta.file_type().is_symlink())
}

fn one(m: &Move) -> Option<String> {
    let (from, to) = (m.from.display(), m.to.display());
    if fs::symlink_metadata(&m.to).is_ok() {
        return Some(format!("left {from} where it is: {to} is already there"));
    }
    let moved =
        m.to.parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::rename(&m.from, &m.to));
    if let Err(e) = moved {
        return Some(format!("could not move {from} to {to}: {e}"));
    }
    let mut line = format!("moved {from} to {to}");
    if m.link {
        match std::os::unix::fs::symlink(&m.to, &m.from) {
            Ok(()) => line.push_str(", linked from its old place"),
            Err(e) => line.push_str(&format!(", and could not link it back: {e}")),
        }
    }
    if let Some(unlinked) = relink_worktrees(&m.to) {
        line.push_str(&unlinked);
    }
    Some(line)
}

// Git's own link from the repo to each worktree names the worktree's path,
// so a moved folder of worktrees is pointed at again. Returns what it could not.
fn relink_worktrees(folder: &Path) -> Option<String> {
    if folder.file_name()? != "worktrees" {
        return None;
    }
    let missed: Vec<String> = fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|tree| tree.join(".git").is_file())
        .filter_map(|tree| {
            relink(&tree)
                .err()
                .map(|e| format!("{}: {e}", tree.display()))
        })
        .collect();
    (!missed.is_empty()).then(|| format!(", but git cannot find {}", missed.join(", ")))
}

fn relink(tree: &Path) -> io::Result<()> {
    let dot_git = tree.join(".git");
    let text = fs::read_to_string(&dot_git)?;
    let admin = text
        .trim()
        .strip_prefix("gitdir: ")
        .map(PathBuf::from)
        .filter(|admin| admin.is_absolute())
        .ok_or_else(|| io::Error::other("its .git file names no absolute git dir"))?;
    let mut line = dot_git.into_os_string();
    line.push("\n");
    fs::write(admin.join("gitdir"), line.as_encoded_bytes())
}

// The `levels` folders above a moved item, while they are empty.
fn remove_empty_parents(from: &Path, levels: usize) {
    let mut folder = from.parent();
    for _ in 0..levels {
        let Some(dir) = folder else { return };
        if fs::remove_dir(dir).is_err() {
            return;
        }
        folder = dir.parent();
    }
}

#[cfg(test)]
mod tests;
