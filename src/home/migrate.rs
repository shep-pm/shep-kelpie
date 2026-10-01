//! The one-time move from `~/.kelpie` into kelpie's home under shep's
//!
//! Only what kelpie's code reads or writes moves, named one by one, since the
//! old folder also holds files kelpie does not own. A move is a rename, and
//! anything that cannot move stops the start that tried, so no runner opens
//! on half its files. A marker in the new place records each finished move,
//! after which nothing is taken from the old folder again. Shared files leave
//! a link at their old place for runners still on the old build, which
//! [`sweep`] removes once none is left.

use std::fs::{self, DirBuilder, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::path::{Path, PathBuf};

use nix::fcntl::{Flock, FlockArg};
use serde_json::Value;

use crate::runner::ProjectName;
use crate::state::write_atomically;

/// Kelpie's own files, which every project shares, each linked back from its
/// old place for a runner still on the old build
const SHARED: [&str; 5] = ["settings.toml", "totp", "tools", "relay", "rulings"];

/// The build the last upgrade replaced, which only `upgrade --rollback` reads
const PREVIOUS_BUILD: &str = "builds/shep-kelpie.previous";

/// A project's old folders, by the name its own folder now holds each under
const PROJECT: [(&str, &str); 4] = [
    ("wt", "worktrees"),
    ("targets", "builds"),
    ("shots", "shots"),
    ("playwright", "playwright"),
];

/// The file a finished move leaves in its new place, naming the old home
pub const MARKER: &str = ".moved-from";

/// The file the old home keeps once a move began, naming kelpie's new home
pub const CLAIM: &str = ".moved-to";

/// What a shepherd keeps at the top of its home, which kelpie never moves from
const SHEPHERDS: [&str; 3] = ["flock.json", "shep.toml", "run"];

/// One file or folder to move, and whether its old place keeps a link to it
#[derive(Debug, Clone, PartialEq, Eq)]
struct Move {
    from: PathBuf,
    to: PathBuf,
    link: bool,
    // How many folders above `from` are kelpie's, removed once empty
    empties: usize,
}

/// Moves from the old home into a new place, done once
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    old: PathBuf,
    // Kelpie's new home, which may be the old one but not inside it
    home: PathBuf,
    // Whether the old home is claimed for `home`, so no other home takes from it
    claim: bool,
    // The folder the marker goes in, which the moves land under
    place: PathBuf,
    moves: Vec<Move>,
}

/// Kelpie's shared files, from the home `old` to the home `new`
pub fn shared(old: &Path, new: &Path) -> Plan {
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
    Plan {
        old: old.to_owned(),
        home: new.to_owned(),
        claim: true,
        place: new.to_owned(),
        moves: linked.chain([previous]).collect(),
    }
}

/// Project `name`'s files, from the old layout under `old` to its own folder under `new`
///
/// Its worktrees, build folders, shots and Playwright files move first, then
/// what was in `projects/<name>`, with its state file last: a runner finding
/// its state moved finds everything the state names moved too.
///
/// # Errors
///
/// A message when `projects/<name>` is there and cannot be listed.
pub fn project(old: &Path, new: &Path, name: &ProjectName) -> Result<Plan, String> {
    let folder = new.join(name.as_str());
    let mut moves: Vec<Move> = PROJECT
        .iter()
        .map(|(was, now)| Move {
            from: old.join(was).join(name.as_str()),
            to: folder.join(now),
            link: false,
            empties: 1,
        })
        .collect();
    let kept = old.join("projects").join(name.as_str());
    let listed = match fs::read_dir(&kept) {
        Ok(listed) => Some(listed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot list {}: {e}", kept.display())),
    };
    let mut entries: Vec<Move> = listed
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| Move {
            from: entry.path(),
            to: folder.join(entry.file_name()),
            link: false,
            empties: 2,
        })
        .collect();
    entries.sort_by_key(|m| (m.from.ends_with("state.json"), m.from.clone()));
    moves.extend(entries);
    Ok(Plan {
        old: old.to_owned(),
        home: new.to_owned(),
        claim: true,
        place: folder,
        moves,
    })
}

/// The dog's book, from the old home `old` to the dog's folder `dog`
pub fn dog(old: &Path, dog: &Path) -> Plan {
    Plan {
        old: old.to_owned(),
        home: dog.parent().unwrap_or(dog).to_owned(),
        claim: false,
        place: dog.to_owned(),
        moves: vec![Move {
            from: old.join("dog/book.json"),
            to: dog.join("book.json"),
            link: false,
            empties: 0,
        }],
    }
}

/// Carries out `plan` once, one start at a time, and says what moved
///
/// Nothing moves once the plan's marker is there, or from a folder that is
/// a shepherd's home. The old folders left empty are removed, and nothing
/// else in them is touched.
///
/// # Errors
///
/// A message naming the item when one cannot move or its new place is
/// taken, or when the new place is inside the old home. Nothing after it moves.
pub fn run(plan: &Plan) -> Result<Vec<String>, String> {
    let marker = plan.place.join(MARKER);
    claim(plan, false)?;
    if marker.exists() || plan.moves.iter().all(|m| !owed(m)) {
        return Ok(Vec::new());
    }
    if let Some(found) = SHEPHERDS.iter().find(|f| plan.old.join(f).exists()) {
        return Ok(vec![format!(
            "left {} as it is: it holds {found}, so it is a shepherd's home, not kelpie's old one",
            plan.old.display()
        )]);
    }
    // Its paths would all name the old home, and break when the shepherd leaves it.
    if plan.home != plan.old && plan.home.starts_with(&plan.old) {
        return Err(format!(
            "kelpie's home {} is inside its old one, {}: move the shepherd out of it first, \
             as the README's \"Moving an install from ~/.kelpie/shep\" says",
            plan.home.display(),
            plan.old.display()
        ));
    }
    claim(plan, true)?;
    private_dir(&plan.place)?;
    let path = plan.place.join(".migrate.lock");
    let file = File::create(&path).map_err(|e| format!("cannot make {}: {e}", path.display()))?;
    let _lock = Flock::lock(file, FlockArg::LockExclusive)
        .map_err(|(_, e)| format!("cannot lock {}: {e}", path.display()))?;
    // Another start may have finished while this one waited.
    if marker.exists() {
        return Ok(Vec::new());
    }
    let mut lines = Vec::new();
    for m in &plan.moves {
        lines.extend(one(m)?);
    }
    for m in &plan.moves {
        remove_empty_parents(&m.from, m.empties);
    }
    let old = plan.old.as_os_str().as_encoded_bytes();
    fs::write(&marker, old).map_err(|e| format!("cannot write {}: {e}", marker.display()))?;
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
        serde_json::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", file.display()))?;
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

/// The links a move left at the old home `old`, each to its place under `new`
pub fn links(old: &Path, new: &Path) -> Vec<PathBuf> {
    SHARED
        .iter()
        .map(|name| old.join(name))
        .filter(|from| {
            let name = from.file_name().expect("a shared file has a name");
            fs::read_link(from).is_ok_and(|to| to == new.join(name))
        })
        .collect()
}

/// Removes the [`links`] at `old`, and the old door if no dog answers there, and says what went
pub fn sweep(old: &Path, new: &Path) -> Vec<String> {
    let mut lines: Vec<String> = links(old, new)
        .into_iter()
        .map(|link| match fs::remove_file(&link) {
            Ok(()) => format!("removed the link {}", link.display()),
            Err(e) => format!("cannot remove the link {}: {e}", link.display()),
        })
        .collect();
    let door = old.join("dog/lease.sock");
    let is_socket = fs::symlink_metadata(&door).is_ok_and(|m| m.file_type().is_socket());
    if is_socket && std::os::unix::net::UnixStream::connect(&door).is_err() {
        if fs::remove_file(&door).is_ok() {
            lines.push(format!("removed the old door {}", door.display()));
        }
        let _ = fs::remove_dir(old.join("dog"));
    }
    lines
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

// Refuses an old home a move into another home began on, and with `take`
// claims it otherwise.
fn claim(plan: &Plan, take: bool) -> Result<(), String> {
    if !plan.claim || plan.old == plan.home {
        return Ok(());
    }
    let file = plan.old.join(CLAIM);
    match fs::read(&file) {
        Ok(named) if named == plan.home.as_os_str().as_encoded_bytes() => Ok(()),
        Ok(named) => Err(format!(
            "{} was moved into {}, not {}: start this runner with the SHEP_HOME or KELPIE_HOME \
             that names that home",
            plan.old.display(),
            String::from_utf8_lossy(&named),
            plan.home.display()
        )),
        Err(_) if !take => Ok(()),
        Err(_) => fs::write(&file, plan.home.as_os_str().as_encoded_bytes())
            .map_err(|e| format!("cannot write {}: {e}", file.display())),
    }
}

// Whether `m` has work left: its old place to move, or a moved item's link to make.
fn owed(m: &Move) -> bool {
    if m.from == m.to {
        return false;
    }
    match fs::symlink_metadata(&m.from) {
        Ok(meta) => !meta.file_type().is_symlink(),
        Err(_) => m.link && fs::symlink_metadata(&m.to).is_ok(),
    }
}

// Moves `m`, or makes the link a start that died after the move did not.
fn one(m: &Move) -> Result<Option<String>, String> {
    if !owed(m) {
        return Ok(None);
    }
    let (from, to) = (m.from.display(), m.to.display());
    let mut line = if fs::symlink_metadata(&m.from).is_ok() {
        if fs::symlink_metadata(&m.to).is_ok() {
            return Err(format!(
                "cannot move {from}: {to} is already there. Keep one of them and remove the other"
            ));
        }
        m.to.parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::rename(&m.from, &m.to))
            .map_err(|e| format!("cannot move {from} to {to}: {e}"))?;
        format!("moved {from} to {to}")
    } else {
        format!("found {to} moved")
    };
    if m.link {
        std::os::unix::fs::symlink(&m.to, &m.from)
            .map_err(|e| format!("moved {from} to {to}, and cannot link it back: {e}"))?;
        line.push_str(", linked from its old place");
    }
    if let Some(unlinked) = relink_worktrees(&m.to) {
        line.push_str(&unlinked);
    }
    Ok(Some(line))
}

fn private_dir(dir: &Path) -> Result<(), String> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("cannot make {}: {e}", dir.display()))
}

// Git's own link from the repo to each worktree names the worktree's path,
// so a moved folder of worktrees is pointed at again. A runner's start runs
// `git worktree repair` as well, which finishes what this cannot.
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
    (!missed.is_empty()).then(|| {
        format!(
            ", and left git's links to {} for its repair",
            missed.join(", ")
        )
    })
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
