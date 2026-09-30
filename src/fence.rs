//! Claude Code's own files in a worktree, which no worker writes
//!
//! Every Claude call in a worktree loads its `.claude` folder and `.mcp.json`:
//! settings and their hooks, agents, skills and MCP servers. A hook there runs
//! outside the sandbox, so the sandbox and `shep-kelpie confine` refuse writes to
//! them, each call checks them against `main`'s, and a pull request that
//! changes them waits on the maintainer. Names match in any case, since macOS
//! reads `.CLAUDE` as `.claude`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::worktree::{self, BASE, WorktreeError, git};

const CLAUDE: &str = ".claude";
const MCP: &str = ".mcp.json";

/// The sandbox's `denyWrite` paths that fence these files in `worktree`
pub fn deny_write(worktree: &Path) -> [PathBuf; 3] {
    [
        worktree.join(CLAUDE),
        worktree.join("**").join(CLAUDE),
        worktree.join(MCP),
    ]
}

/// Whether `path`, relative to a worktree, is one of Claude Code's own files
pub fn fenced(path: &Path) -> bool {
    let parts: Vec<&OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    matches!(parts.as_slice(), [only] if folds_to(only, MCP))
        || parts.iter().any(|p| folds_to(p, CLAUDE))
}

// Whether a case-insensitive file system could read `part` as `name`. Case
// is folded the Unicode way (`ſ` reads as `s`), and whatever is still not
// ASCII is dropped, which errs toward refusing a name no one would read.
fn folds_to(part: &OsStr, name: &str) -> bool {
    let folded: String = part
        .to_string_lossy()
        .chars()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .filter(char::is_ascii)
        .collect();
    folded == name
}

/// Fetches `branch`, and names Claude Code's own files its head changes
///
/// A file counts when the branch changes it from `main`, and from `accepted`,
/// a head whose change to them the maintainer accepted, if there is one.
/// Returns the head and those files.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn changed(
    repo: &Path,
    branch: &str,
    accepted: Option<&str>,
) -> Result<(String, Vec<String>), WorktreeError> {
    git(repo, ["fetch", "--quiet", "origin", BASE, branch])?;
    let tracking = format!("refs/remotes/origin/{branch}");
    let head = git(repo, ["rev-parse", "--verify", "--quiet", &tracking])?;
    let between = |range: &str| -> Result<Vec<String>, WorktreeError> {
        let names = git(repo, ["diff", "--name-only", "--no-renames", "-z", range])?;
        Ok(names
            .split('\0')
            .filter(|name| fenced(Path::new(name)))
            .map(str::to_owned)
            .collect())
    };
    let from_main = between(&format!("origin/{BASE}...{head}"))?;
    if from_main.is_empty() {
        return Ok((head, from_main));
    }
    if let Some(accepted) = accepted
        && between(&format!("{accepted}..{head}"))?.is_empty()
    {
        return Ok((head, Vec::new()));
    }
    Ok((head, from_main))
}

/// Names Claude Code's own files in `worktree` that differ from `main`'s
///
/// The worktree passes when all of them match one commit: `origin/main`,
/// the commit its branch left `main` at, or `accepted`. A file missing on
/// either side differs. Returns the files that differ from `origin/main`.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn differ(
    repo: &Path,
    worktree: &Path,
    accepted: Option<&str>,
) -> Result<Vec<String>, WorktreeError> {
    let disk = on_disk(worktree)?;
    let files: Vec<&PathBuf> = disk
        .values()
        .filter_map(|entry| match entry {
            Disk::File { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    let mut hashes = BTreeMap::new();
    if !files.is_empty() {
        let mut args: Vec<&OsStr> = ["hash-object", "--no-filters", "--"].map(OsStr::new).into();
        args.extend(files.iter().map(|p| p.as_os_str()));
        let ids = git(repo, args)?;
        hashes.extend(
            files
                .into_iter()
                .cloned()
                .zip(ids.lines().map(str::to_owned)),
        );
    }
    let main = format!("origin/{BASE}");
    let head = worktree::head(repo, worktree)?;
    let base = git(repo, ["merge-base", &head, &main])?;
    let mut first = None;
    for commit in [Some(main.as_str()), Some(base.as_str()), accepted]
        .into_iter()
        .flatten()
    {
        let trusted = in_commit(repo, commit)?;
        let differing = compare(repo, &disk, &hashes, &trusted)?;
        if differing.is_empty() {
            return Ok(differing);
        }
        first.get_or_insert(differing);
    }
    Ok(first.unwrap_or_default())
}

// A fenced entry in the worktree, never followed if it is a link.
enum Disk {
    File { path: PathBuf, executable: bool },
    Link(String),
    Other,
}

// Every file Claude Code would load from the worktree's root, by its path
// from the root. The names are opened as Claude Code opens them, so the
// file system folds case and Unicode its own way. Folders count only for
// what they hold, as in git.
fn on_disk(worktree: &Path) -> Result<BTreeMap<String, Disk>, WorktreeError> {
    let mut found = BTreeMap::new();
    let mut pending: Vec<PathBuf> = Vec::new();
    for name in [CLAUDE, MCP] {
        let path = worktree.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => pending.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(folder(&path, &e)),
        }
    }
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).map_err(|e| folder(&path, &e))?;
        let relative = path
            .strip_prefix(worktree)
            .expect("found under the worktree")
            .to_string_lossy()
            .into_owned();
        let kind = meta.file_type();
        if kind.is_dir() {
            pending.extend(read_dir(&path)?.map(|entry| entry.path()));
            continue;
        }
        let disk = if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(|e| folder(&path, &e))?;
            Disk::Link(target.to_string_lossy().into_owned())
        } else if kind.is_file() {
            let executable = meta.permissions().mode() & 0o111 != 0;
            Disk::File { path, executable }
        } else {
            Disk::Other
        };
        found.insert(relative, disk);
    }
    Ok(found)
}

// Each fenced file in `commit`, by path with its root folder named as
// `on_disk` names it, with its mode and blob. Git matches a pathspec by exact
// case, so the root is listed and matched here. Two spellings of one path
// never match anything.
fn in_commit(
    repo: &Path,
    commit: &str,
) -> Result<BTreeMap<String, (String, String)>, WorktreeError> {
    let entries = |listing: String| -> Vec<(String, String, String)> {
        listing
            .split('\0')
            .filter_map(|line| {
                let (meta, path) = line.split_once('\t')?;
                let mut meta = meta.split(' ');
                let (mode, _, blob) = (meta.next()?, meta.next()?, meta.next()?);
                Some((path.to_owned(), mode.to_owned(), blob.to_owned()))
            })
            .collect()
    };
    let mut found = BTreeMap::new();
    for (root, ..) in entries(git(repo, ["ls-tree", "-z", commit])?) {
        let Some(name) = [CLAUDE, MCP]
            .into_iter()
            .find(|name| folds_to(OsStr::new(&root), name))
        else {
            continue;
        };
        let args = [
            "--literal-pathspecs",
            "ls-tree",
            "-r",
            "-z",
            commit,
            "--",
            &root,
        ];
        for (path, mode, blob) in entries(git(repo, args)?) {
            let key = format!("{name}{}", &path[root.len()..]);
            let entry = (mode, blob);
            let conflict = || ("conflict".to_owned(), String::new());
            found
                .entry(key)
                .and_modify(|seen| *seen = conflict())
                .or_insert(entry);
        }
    }
    Ok(found)
}

fn compare(
    repo: &Path,
    disk: &BTreeMap<String, Disk>,
    hashes: &BTreeMap<PathBuf, String>,
    trusted: &BTreeMap<String, (String, String)>,
) -> Result<Vec<String>, WorktreeError> {
    let mut differing: Vec<String> = trusted
        .keys()
        .filter(|path| !disk.contains_key(*path))
        .cloned()
        .collect();
    for (path, entry) in disk {
        let same = match (entry, trusted.get(path)) {
            (Disk::File { path, executable }, Some((mode, blob))) => {
                let want = if *executable { "100755" } else { "100644" };
                mode == want && hashes.get(path) == Some(blob)
            }
            (Disk::Link(target), Some((mode, blob))) if mode == "120000" => {
                git(repo, ["cat-file", "blob", blob])? == *target
            }
            _ => false,
        };
        if !same {
            differing.push(path.clone());
        }
    }
    differing.sort();
    Ok(differing)
}

fn read_dir(path: &Path) -> Result<impl Iterator<Item = fs::DirEntry>, WorktreeError> {
    let entries = fs::read_dir(path).map_err(|e| folder(path, &e))?;
    let entries: Result<Vec<_>, _> = entries.collect();
    Ok(entries.map_err(|e| folder(path, &e))?.into_iter())
}

fn folder(path: &Path, e: &std::io::Error) -> WorktreeError {
    WorktreeError::Unreadable {
        path: path.to_owned(),
        kind: e.kind(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::test::git;

    const SETTINGS: &str = ".claude/settings.json";

    // A clone whose `main` holds a settings file, and a worktree on
    // `kelpie/7` cut from it, as kelpie cuts one.
    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl World {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            git(
                &root,
                &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
            );
            git(&root, &["clone", "--quiet", "origin.git", "repo"]);
            let w = Self { _dir: dir, root };
            w.commit(&w.repo(), SETTINGS, "{}\n");
            git(&w.repo(), &["push", "--quiet", "origin", "main"]);
            git(&w.repo(), &["fetch", "--quiet", "origin"]);
            let wt = w.wt();
            git(
                &w.repo(),
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "kelpie/7",
                    wt.to_str().unwrap(),
                    "origin/main",
                ],
            );
            w
        }

        fn repo(&self) -> PathBuf {
            self.root.join("repo")
        }

        fn wt(&self) -> PathBuf {
            self.root.join("wt")
        }

        fn commit(&self, cwd: &Path, file: &str, text: &str) -> String {
            let path = cwd.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
            git(cwd, &["add", file]);
            git(cwd, &["commit", "--quiet", "-m", file]);
            git(cwd, &["rev-parse", "HEAD"])
        }

        fn write(&self, file: &str, text: &str) {
            let path = self.wt().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn differ(&self, accepted: Option<&str>) -> Vec<String> {
            differ(&self.repo(), &self.wt(), accepted).unwrap()
        }

        fn changed(&self, accepted: Option<&str>) -> Vec<String> {
            changed(&self.repo(), "kelpie/7", accepted).unwrap().1
        }

        fn push_branch(&self, file: &str, text: &str) -> String {
            let head = self.commit(&self.wt(), file, text);
            git(&self.wt(), &["push", "--quiet", "origin", "HEAD"]);
            head
        }
    }

    #[test]
    fn claude_codes_own_files_are_named_in_any_case_and_anywhere_for_dot_claude() {
        for path in [
            ".claude",
            ".claude/settings.local.json",
            "./.claude/agents/a.md",
            ".CLAUDE/settings.json",
            "src/.Claude/skills/x/SKILL.md",
            ".mcp.json",
            ".MCP.JSON",
            // U+017F, which APFS reads as `s`
            ".mcp.j\u{17f}on",
            // A combining accent, which is refused rather than read
            ".cla\u{301}ude/settings.json",
        ] {
            assert!(fenced(Path::new(path)), "{path}");
        }
        for path in [
            // A Cyrillic `а`, which no file system reads as `a`
            ".cl\u{430}ude/settings.json",
            "claude/notes.md",
            "src/.mcp.json",
            ".mcp.json.bak",
            "README.md",
            "",
        ] {
            assert!(!fenced(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn a_worktree_as_main_left_it_passes() {
        let w = World::new();
        w.write("src/lib.rs", "work\n");
        assert_eq!(w.differ(None), Vec::<String>::new());
    }

    #[test]
    fn a_file_added_changed_or_removed_differs() {
        let w = World::new();
        w.write(".claude/settings.local.json", "{}\n");
        assert_eq!(w.differ(None), [".claude/settings.local.json"]);

        let w = World::new();
        w.write(SETTINGS, "{\"hooks\":{}}\n");
        w.write(".mcp.json", "{}\n");
        assert_eq!(w.differ(None), [".claude/settings.json", ".mcp.json"]);

        let w = World::new();
        fs::remove_file(w.wt().join(SETTINGS)).unwrap();
        assert_eq!(w.differ(None), [SETTINGS]);
    }

    #[test]
    fn a_link_or_a_new_exec_bit_differs() {
        let w = World::new();
        symlink("/etc/hosts", w.wt().join(".mcp.json")).unwrap();
        assert_eq!(w.differ(None), [".mcp.json"]);

        let w = World::new();
        let path = w.wt().join(SETTINGS);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(w.differ(None), [SETTINGS]);
    }

    #[test]
    fn a_worktree_cut_before_main_changed_its_settings_still_passes() {
        let w = World::new();
        w.commit(&w.repo(), SETTINGS, "{\"model\":\"x\"}\n");
        git(&w.repo(), &["push", "--quiet", "origin", "main"]);
        git(&w.repo(), &["fetch", "--quiet", "origin"]);
        assert_eq!(w.differ(None), Vec::<String>::new());
    }

    #[test]
    fn a_worktree_at_an_accepted_head_passes_and_no_other() {
        let w = World::new();
        let accepted = w.commit(&w.wt(), SETTINGS, "{\"hooks\":{}}\n");
        assert_eq!(w.differ(None), [SETTINGS]);
        assert_eq!(w.differ(Some(&accepted)), Vec::<String>::new());
        w.write(".claude/agents/a.md", "hi\n");
        assert_eq!(w.differ(Some(&accepted)), [".claude/agents/a.md", SETTINGS]);
    }

    #[test]
    fn an_accepted_head_that_spells_the_folder_in_capitals_still_passes() {
        let w = World::new();
        w.write(".claude/agents/a.md", "hi\n");
        let blob = git(&w.wt(), &["hash-object", "-w", ".claude/agents/a.md"]);
        let entry = format!("100644,{blob},.CLAUDE/agents/a.md");
        git(&w.wt(), &["update-index", "--add", "--cacheinfo", &entry]);
        git(&w.wt(), &["commit", "--quiet", "-m", "capitals"]);
        let accepted = git(&w.wt(), &["rev-parse", "HEAD"]);
        assert_eq!(w.differ(None), [".claude/agents/a.md"]);
        assert_eq!(w.differ(Some(&accepted)), Vec::<String>::new());
    }

    // Kelpie runs on macOS, whose APFS opens `.mcp.jſon` as `.mcp.json`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_file_the_file_system_reads_as_mcp_json_differs() {
        let w = World::new();
        w.write(".mcp.j\u{17f}on", "{}\n");
        assert_eq!(w.differ(None), [".mcp.json"]);
    }

    #[test]
    fn a_branch_changing_them_in_any_case_is_named_until_accepted() {
        let w = World::new();
        w.push_branch("src/lib.rs", "work\n");
        assert_eq!(w.changed(None), Vec::<String>::new());

        let accepted = w.push_branch(".CLAUDE/agents/a.md", "hi\n");
        let changed = w.changed(None);
        assert_eq!(changed.len(), 1);
        assert!(
            changed[0].eq_ignore_ascii_case(".claude/agents/a.md"),
            "{changed:?}"
        );
        assert_eq!(w.changed(Some(&accepted)), Vec::<String>::new());

        w.push_branch("src/more.rs", "work\n");
        assert_eq!(w.changed(Some(&accepted)), Vec::<String>::new());
        w.push_branch(".mcp.json", "{}\n");
        assert_eq!(w.changed(Some(&accepted)).len(), 2);
    }
}
