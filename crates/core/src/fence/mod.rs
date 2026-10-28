//! Each harness's own files in a worktree, which no worker writes
//!
//! A harness loads its own files from the worktree: Claude Code its `.claude`
//! folder and `.mcp.json`, Codex its `.codex` folder, goose its `.goose`
//! folder. Settings, hooks and MCP servers there run code, so the sandbox and `kelpie confine` refuse writes
//! to every harness's files, each call checks them against `main`'s, and a
//! pull request that changes them waits on the maintainer. Names match in any
//! case, since macOS reads `.CLAUDE` as `.claude`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::worktree::{self, Remote, WorktreeError, git};

mod commit;
mod nested;

/// The files in a worktree that one harness loads and may run code from, and
/// the files in the home folder that hold its login
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnFiles {
    /// The harness, as a refusal names it
    pub harness: &'static str,
    /// Folders it loads, at the root or at any depth below
    pub folders: &'static [&'static str],
    /// Folders it loads that sit in a folder of another name, as `.config/goose`,
    /// at the root or at any depth below. Each is a parent's name and its own.
    pub nested: &'static [[&'static str; 2]],
    /// Files it loads at the root only
    pub root_files: &'static [&'static str],
    /// Its login and secrets, under `~/`, which no other harness's call reads
    pub credentials: &'static [&'static str],
    /// The program that starts it, which no worker runs from its commands
    pub program: &'static str,
}

/// Every harness's own files, each fenced whichever harness a call runs on
///
/// A worker on one harness could otherwise plant files that the next call,
/// on another, runs.
pub const HARNESSES: [OwnFiles; 4] = [
    OwnFiles {
        harness: "Claude Code",
        folders: &[".claude"],
        nested: &[],
        root_files: &[".mcp.json"],
        // Linux keeps the login in the file; `.claude.json` holds MCP servers' secrets.
        credentials: &["~/.claude/.credentials.json", "~/.claude.json"],
        program: "claude",
    },
    // The maintainer's own. Kelpie's Codex login is in `codex_home`, which
    // `ports::Unreadable` keeps from every call but Codex's link to it.
    // Codex lists skills in a repo's `.agents` whether or not it trusts the repo.
    OwnFiles {
        harness: "Codex",
        folders: &[".codex", ".agents"],
        nested: &[],
        root_files: &[],
        credentials: &["~/.codex/**"],
        program: "codex",
    },
    // Kelpie runs pi with a home of its own, so the maintainer's is never read.
    OwnFiles {
        harness: PI,
        folders: &[".pi"],
        nested: &[],
        root_files: &[],
        credentials: &["~/.pi/**"],
        program: "pi",
    },
    // goose also loads `.goosehints` from the working folder. It is not listed
    // here: #788 sets `CONTEXT_FILE_NAMES` so goose never reads it. Its config,
    // secrets and session database sit in four places, by platform.
    OwnFiles {
        harness: "goose",
        folders: &[".goose"],
        // `settings.local.json` and `settings.json` there can disable a plugin by name.
        nested: &[[".config", "goose"]],
        root_files: &[],
        credentials: &[
            "~/.config/goose/**",
            "~/.local/share/goose/**",
            "~/.local/state/goose/**",
            "~/Library/Application Support/Block/goose/**",
        ],
        program: "goose",
    },
];

/// pi, as the fence names it
pub const PI: &str = "pi";

/// Every harness's credentials, Claude Code's included, which a call on a harness
/// that keeps its login elsewhere may not read
pub fn credentials() -> Vec<String> {
    others_credentials("")
}

/// Every other harness's credentials, which a call on `harness` may not read
pub fn others_credentials(harness: &str) -> Vec<String> {
    HARNESSES
        .iter()
        .filter(|own| own.harness != harness)
        .flat_map(|own| own.credentials.iter().map(|&path| path.to_owned()))
        .collect()
}

// Every name a harness loads from the worktree's root.
fn root_names() -> impl Iterator<Item = &'static str> {
    HARNESSES
        .iter()
        .flat_map(|own| own.folders.iter().chain(own.root_files))
        .copied()
}

// Every folder a harness loads from a folder of another name in the worktree's root.
fn nested_names() -> impl Iterator<Item = [&'static str; 2]> {
    HARNESSES.iter().flat_map(|own| own.nested).copied()
}

/// The sandbox's `denyWrite` paths that fence these files in `worktree`
///
/// srt skips every glob in `denyWrite` on Linux, so there the harness folders
/// already below the worktree are listed as paths too. One made during the
/// call is caught by [`differ`] after it.
pub fn deny_write(worktree: &Path) -> Vec<PathBuf> {
    deny_write_on(worktree, cfg!(target_os = "linux"))
}

// The same, as on Linux or not, which a test runs on either.
fn deny_write_on(worktree: &Path, linux: bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for own in &HARNESSES {
        for folder in own.folders {
            paths.push(worktree.join(folder));
            paths.push(worktree.join("**").join(folder));
        }
        for [parent, folder] in own.nested {
            let inside = Path::new(parent).join(folder);
            paths.push(worktree.join(&inside));
            paths.push(worktree.join("**").join(inside));
        }
        paths.extend(own.root_files.iter().map(|file| worktree.join(file)));
    }
    if linux {
        for path in nested::existing(worktree) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// Whether `path`, relative to a worktree, is one of a harness's own files
pub fn fenced(path: &Path) -> bool {
    owner(path).is_some()
}

/// The harness whose own file `path`, relative to a worktree, is
pub fn owner(path: &Path) -> Option<&'static str> {
    let parts: Vec<&OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let any = |names: &[&str], part: &OsStr| names.iter().any(|name| folds_to(part, name));
    HARNESSES
        .iter()
        .find(|own| {
            matches!(parts.as_slice(), [only] if any(own.root_files, only))
                || parts.iter().any(|part| any(own.folders, part))
                || (parts.windows(2)).any(|pair| {
                    own.nested.iter().any(|[parent, folder]| {
                        folds_to(pair[0], parent) && folds_to(pair[1], folder)
                    })
                })
        })
        .map(|own| own.harness)
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

/// Fetches `branch` from `remote`, and names the harnesses' own files its head
/// changes
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
    remote: &Remote,
    accepted: Option<&str>,
    base: &str,
) -> Result<(String, Vec<String>), WorktreeError> {
    let head = worktree::head_with_main(repo, branch, remote, base)?;
    let between = |range: &str| -> Result<Vec<String>, WorktreeError> {
        let names = git(repo, ["diff", "--name-only", "--no-renames", "-z", range])?;
        Ok(names
            .split('\0')
            .filter(|name| fenced(Path::new(name)))
            .map(str::to_owned)
            .collect())
    };
    let from_main = between(&format!("origin/{base}...{head}"))?;
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

/// Names the harnesses' own files in `worktree` that differ from `main`'s
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
    base: &str,
) -> Result<Vec<String>, WorktreeError> {
    let disk = on_disk(worktree)?;
    let hashes = hashed(repo, &disk)?;
    let (head, main) = commit::head_and_main(repo, worktree, base)?;
    let cut_at = commit::cut_at(repo, &head, &main)?;
    let mut first = None;
    for commit in [Some(main.as_str()), Some(cut_at.as_str()), accepted]
        .into_iter()
        .flatten()
    {
        let trusted = commit::files_in(repo, commit)?;
        let differing = compare(repo, &disk, &hashes, &trusted)?;
        if differing.is_empty() {
            return Ok(differing);
        }
        first.get_or_insert(differing);
    }
    Ok(first.unwrap_or_default())
}

/// Whether the harnesses' own files in `worktree` are exactly `origin/main`'s
///
/// Unlike [`differ`], a commit the branch left `main` at or a head the
/// maintainer accepted does not count: a call in such a worktree loads
/// `main`'s own files and nothing else. A folder that is no worktree matches
/// when `main` holds no such files. Any error counts as a mismatch.
pub fn matches_main(repo: &Path, worktree: &Path, base: &str) -> bool {
    let Ok(disk) = on_disk(worktree) else {
        return false;
    };
    let Ok(hashes) = hashed(repo, &disk) else {
        return false;
    };
    commit::main_of(repo, base)
        .and_then(|main| commit::files_in(repo, &main))
        .and_then(|trusted| compare(repo, &disk, &hashes, &trusted))
        .is_ok_and(|differing| differing.is_empty())
}

// The blob id git gives each file on disk, byte for byte as it is.
fn hashed(
    repo: &Path,
    disk: &BTreeMap<String, Disk>,
) -> Result<BTreeMap<PathBuf, String>, WorktreeError> {
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
    Ok(hashes)
}

// A fenced entry in the worktree, never followed if it is a link.
enum Disk {
    // `executable` is `None` where the file system keeps no mode: see `executable_bit`.
    File {
        path: PathBuf,
        executable: Option<bool>,
    },
    Link(String),
    Other,
}

// Every file a harness would load from the worktree's root, by its path
// from the root. The names are opened as a harness opens them, so the
// file system folds case and Unicode its own way. Folders count only for
// what they hold, as in git.
fn on_disk(worktree: &Path) -> Result<BTreeMap<String, Disk>, WorktreeError> {
    let mut found = BTreeMap::new();
    let mut pending: Vec<PathBuf> = Vec::new();
    let nested = nested_names().map(|[parent, folder]| Path::new(parent).join(folder));
    for name in root_names().map(PathBuf::from).chain(nested) {
        let path = worktree.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => pending.push(path),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(e) => return Err(folder(&path, &e)),
        }
    }
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).map_err(|e| folder(&path, &e))?;
        let relative = git_path(
            path.strip_prefix(worktree)
                .expect("found under the worktree"),
        );
        let kind = meta.file_type();
        if kind.is_dir() {
            pending.extend(read_dir(&path)?.map(|entry| entry.path()));
            continue;
        }
        let disk = if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(|e| folder(&path, &e))?;
            Disk::Link(target.to_string_lossy().into_owned())
        } else if kind.is_file() {
            let executable = executable_bit(&meta);
            Disk::File { path, executable }
        } else {
            Disk::Other
        };
        found.insert(relative, disk);
    }
    Ok(found)
}

// `relative` spelled as git spells a path, with `/` between its parts on
// every platform, so a file on disk finds the same file in a commit.
fn git_path(relative: &Path) -> String {
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}

// Whether a file on disk is executable, as git records it.
#[cfg(unix)]
fn executable_bit(meta: &fs::Metadata) -> Option<bool> {
    Some(meta.permissions().mode() & 0o111 != 0)
}

// Windows keeps no executable bit on a file, and git there sets
// `core.fileMode` off, so the disk cannot say. A worker cannot set the bit
// through the file system either, so the commit's regular mode stands.
#[cfg(windows)]
fn executable_bit(_: &fs::Metadata) -> Option<bool> {
    None
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
                let same_mode = match executable {
                    Some(true) => mode == "100755",
                    Some(false) => mode == "100644",
                    None => mode == "100644" || mode == "100755",
                };
                same_mode && hashes.get(path) == Some(blob)
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
    #[cfg(unix)]
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
            differ(&self.repo(), &self.wt(), accepted, "main").unwrap()
        }

        fn changed(&self, accepted: Option<&str>) -> Vec<String> {
            changed(&self.repo(), "kelpie/7", &Remote::Origin, accepted, "main")
                .unwrap()
                .1
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
    fn codexs_own_files_are_named_in_any_case_and_anywhere() {
        for path in [
            ".codex",
            ".codex/config.toml",
            ".CODEX/config.toml",
            "src/.Codex/config.toml",
            // A combining accent, which is refused rather than read
            ".co\u{301}dex/config.toml",
        ] {
            assert_eq!(owner(Path::new(path)), Some("Codex"), "{path}");
        }
        for path in ["codex/notes.md", ".codex.toml", "src/codex.rs"] {
            assert!(!fenced(Path::new(path)), "{path}");
        }
        assert_eq!(owner(Path::new(".mcp.json")), Some("Claude Code"));
    }

    #[test]
    fn a_call_reads_no_other_harnesss_login() {
        assert_eq!(
            others_credentials("Claude Code"),
            [
                "~/.codex/**",
                "~/.pi/**",
                "~/.config/goose/**",
                "~/.local/share/goose/**",
                "~/.local/state/goose/**",
                "~/Library/Application Support/Block/goose/**",
            ]
        );
        assert_eq!(
            others_credentials("Codex"),
            [
                "~/.claude/.credentials.json",
                "~/.claude.json",
                "~/.pi/**",
                "~/.config/goose/**",
                "~/.local/share/goose/**",
                "~/.local/state/goose/**",
                "~/Library/Application Support/Block/goose/**",
            ]
        );
    }

    #[test]
    fn a_goose_folder_is_fenced_and_its_credentials_are_unreadable_to_every_other_harness() {
        for path in [
            ".goose",
            ".goose/config.yaml",
            "src/.GOOSE/x",
            "a/b/.Goose/recipes",
        ] {
            assert_eq!(owner(Path::new(path)), Some("goose"), "{path}");
        }
        for path in ["goose/notes.md", ".goosehints", "src/goose.rs"] {
            assert!(!fenced(Path::new(path)), "{path}");
        }
        assert_eq!(
            owner(Path::new(".agents/skills")),
            Some("Codex"),
            ".agents stays Codex's"
        );

        let goose = [
            "~/.config/goose/**",
            "~/.local/share/goose/**",
            "~/.local/state/goose/**",
            "~/Library/Application Support/Block/goose/**",
        ];
        for harness in ["Claude Code", "Codex", PI] {
            let unread = others_credentials(harness);
            for path in goose {
                assert!(unread.iter().any(|p| p == path), "{harness}: {path}");
            }
        }
        let own = others_credentials("goose");
        assert!(goose.iter().all(|path| !own.iter().any(|p| p == path)));
        assert!(own.iter().any(|p| p == "~/.pi/**"));
    }

    // goose reads `settings.local.json` and `settings.json` under `.config/goose`
    // in its working folder, and a `disabledPlugins` entry there switches off the
    // plugin that fences a worker.
    #[test]
    fn a_goose_settings_file_in_the_worktree_at_any_depth_is_fenced() {
        for path in [
            ".config/goose",
            ".config/goose/settings.local.json",
            ".config/goose/settings.json",
            "packages/app/.config/goose/settings.local.json",
            ".CONFIG/Goose/settings.json",
        ] {
            assert_eq!(owner(Path::new(path)), Some("goose"), "{path}");
        }
        // Other tools' files under `.config`, and a goose folder elsewhere, are the project's.
        for path in [
            ".config",
            ".config/other/settings.json",
            "config/goose/settings.json",
            "goose/.config/x",
            "src/.config.rs",
        ] {
            assert!(!fenced(Path::new(path)), "{path}");
        }
        let wt = Path::new("/k/wt/7");
        let deny = deny_write(wt);
        for path in ["/k/wt/7/.config/goose", "/k/wt/7/**/.config/goose"] {
            assert!(deny.contains(&PathBuf::from(path)), "{path}");
        }
    }

    #[test]
    fn the_sandbox_denies_every_harnesss_files_in_the_worktree() {
        let wt = Path::new("/k/wt/7");
        let deny: Vec<String> = deny_write(wt)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            deny,
            [
                "/k/wt/7/.claude",
                "/k/wt/7/**/.claude",
                "/k/wt/7/.mcp.json",
                "/k/wt/7/.codex",
                "/k/wt/7/**/.codex",
                "/k/wt/7/.agents",
                "/k/wt/7/**/.agents",
                "/k/wt/7/.pi",
                "/k/wt/7/**/.pi",
                "/k/wt/7/.goose",
                "/k/wt/7/**/.goose",
                "/k/wt/7/.config/goose",
                "/k/wt/7/**/.config/goose",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn deny_write_lists_existing_nested_harness_folders_on_linux() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        for folder in [
            ".claude/agents",
            "src/.claude/skills",
            "lib/deep/.Claude",
            "docs/.CODEX",
            "docs/.agents",
            "tools/.pi/extensions",
            "app/.Goose/recipes",
            "src/plain",
            "src/claude",
        ] {
            fs::create_dir_all(wt.join(folder)).unwrap();
        }
        // A link is listed and never followed.
        symlink("/etc", wt.join("lib/.codex")).unwrap();
        let listed = |linux| -> Vec<String> {
            deny_write_on(wt, linux)
                .iter()
                .map(|p| p.strip_prefix(wt).unwrap().display().to_string())
                .collect()
        };

        let linux = listed(true);
        assert_eq!(linux[..13], listed(false)[..]);
        assert_eq!(
            linux[13..],
            [
                "app/.Goose",
                "docs/.CODEX",
                "docs/.agents",
                "lib/.codex",
                "lib/deep/.Claude",
                "src/.claude",
                "tools/.pi",
            ]
        );
    }

    #[test]
    fn a_nested_harness_folder_past_the_walks_bounds_is_left_to_the_check_after_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path();
        let mut deep = PathBuf::new();
        for level in 0..12 {
            deep.push(format!("d{level}"));
        }
        fs::create_dir_all(wt.join(&deep).join(".claude")).unwrap();
        fs::create_dir_all(wt.join(".git/.claude")).unwrap();
        fs::create_dir_all(wt.join("near/.claude")).unwrap();
        let listed = deny_write_on(wt, true);
        assert_eq!(listed[13..], [wt.join("near/.claude")]);
    }

    #[test]
    fn a_pi_extension_added_to_a_branch_is_named() {
        let w = World::new();
        w.push_branch(".pi/extensions/run.ts", "export default () => {};\n");
        assert_eq!(w.changed(None).len(), 1);
        assert_eq!(owner(Path::new("src/.pi/x.ts")), Some("pi"));
    }

    #[test]
    fn a_codex_file_added_to_the_worktree_or_a_branch_is_named() {
        let w = World::new();
        w.write(".codex/config.toml", "[mcp_servers]\n");
        assert_eq!(w.differ(None), [".codex/config.toml"]);

        let w = World::new();
        w.push_branch(".Codex/config.toml", "[mcp_servers]\n");
        assert_eq!(w.changed(None).len(), 1);
    }

    // The Windows fence lists only what exists when the call starts, so a
    // harness file or folder made during the call is caught here alone.
    #[test]
    fn a_harness_file_or_folder_made_during_a_call_differs() {
        for made in [
            ".agents/skills/x/SKILL.md",
            ".codex/config.toml",
            ".goose/config.yaml",
            ".pi/extensions/run.ts",
            ".config/goose/config.yaml",
            ".mcp.json",
        ] {
            let w = World::new();
            assert!(deny_write_on(&w.wt(), false).contains(&w.wt().join(".codex")));
            w.write(made, "made during the call\n");
            assert_eq!(w.differ(None), [made]);
        }
    }

    #[test]
    fn goose_files_in_the_config_folder_are_compared_with_an_accepted_commit() {
        let w = World::new();
        let accepted = w.commit(&w.wt(), ".config/goose/config.yaml", "extensions: {}\n");
        let file = ".config/goose/config.yaml";
        assert_eq!(w.differ(None), [file]);
        assert_eq!(w.differ(Some(&accepted)), Vec::<String>::new());
        w.write(file, "changed\n");
        assert_eq!(w.differ(Some(&accepted)), [file]);
    }

    #[test]
    fn a_file_named_config_in_the_worktree_root_is_no_harness_file() {
        let w = World::new();
        w.write(".config", "not a folder\n");
        assert_eq!(w.differ(None), Vec::<String>::new());
        assert!(matches_main(&w.repo(), &w.wt(), "main"));
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

    #[cfg(unix)]
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
    fn a_path_on_disk_is_spelled_with_slashes_as_git_spells_it() {
        let nested = Path::new(".claude").join("hooks").join("a.sh");
        assert_eq!(git_path(&nested), ".claude/hooks/a.sh");
        assert_eq!(git_path(Path::new(".mcp.json")), ".mcp.json");
    }

    // Windows keeps no mode on a file, so the disk names none: see `executable_bit`.
    #[test]
    fn a_file_with_no_mode_on_disk_matches_either_regular_mode_and_nothing_else() {
        let on_disk = |text: &str| {
            let path = PathBuf::from("/wt").join(text);
            let disk = BTreeMap::from([(
                text.to_owned(),
                Disk::File {
                    path: path.clone(),
                    executable: None,
                },
            )]);
            (disk, BTreeMap::from([(path, "blob".to_owned())]))
        };
        let differ = |mode: &str, blob: &str| {
            let (disk, hashes) = on_disk("f");
            let trusted = BTreeMap::from([("f".to_owned(), (mode.to_owned(), blob.to_owned()))]);
            compare(Path::new("."), &disk, &hashes, &trusted).unwrap()
        };
        assert_eq!(differ("100644", "blob"), Vec::<String>::new());
        assert_eq!(differ("100755", "blob"), Vec::<String>::new());
        assert_eq!(differ("120000", "blob"), ["f"], "main has a link there");
        assert_eq!(differ("100644", "other"), ["f"], "other contents");
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
    fn a_worktree_matches_main_only_when_its_files_are_origin_mains_now() {
        let w = World::new();
        assert!(matches_main(&w.repo(), &w.wt(), "main"));

        // Cut before main changed its settings: it passes the check, but is not main's.
        w.commit(&w.repo(), SETTINGS, "{\"model\":\"x\"}\n");
        git(&w.repo(), &["push", "--quiet", "origin", "main"]);
        git(&w.repo(), &["fetch", "--quiet", "origin"]);
        assert!(!matches_main(&w.repo(), &w.wt(), "main"));

        let w = World::new();
        w.write(".claude/agents/a.md", "hi\n");
        assert!(!matches_main(&w.repo(), &w.wt(), "main"));

        // A folder with none of the files differs from a main that holds some.
        let bare = w.root.join("bare");
        fs::create_dir(&bare).unwrap();
        assert!(!matches_main(&w.repo(), &bare, "main"));
    }

    #[test]
    fn a_second_pass_over_the_same_commits_asks_git_for_no_tree_or_ancestor() {
        let w = World::new();
        let asked = || commit::ASKED.with(std::cell::Cell::get);
        w.write(SETTINGS, "{\"other\":true}\n");
        let first = w.differ(None);
        let after_first = asked();
        assert!(after_first > 0, "the first pass reads the commits");

        assert_eq!(w.differ(None), first);
        assert!(matches_main(&w.repo(), &w.wt(), "main") == first.is_empty());

        assert_eq!(asked(), after_first);
    }

    #[test]
    fn a_moved_origin_main_is_read_again() {
        let w = World::new();
        w.write(SETTINGS, "{\"moved\":true}\n");
        assert_eq!(w.differ(None), [SETTINGS]);
        assert!(!matches_main(&w.repo(), &w.wt(), "main"));

        w.commit(&w.repo(), SETTINGS, "{\"moved\":true}\n");
        git(&w.repo(), &["push", "--quiet", "origin", "main"]);

        assert_eq!(w.differ(None), Vec::<String>::new());
        assert!(matches_main(&w.repo(), &w.wt(), "main"));
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
