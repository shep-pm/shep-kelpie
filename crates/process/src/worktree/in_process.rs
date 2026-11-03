//! Reads of kelpie's checkout made in this process, which spare starting git
//!
//! Starting git costs tens of milliseconds on Windows. The repo is opened
//! with its own config only, never the system, global, user or environment
//! config, so a read answers as git does only where those could not change
//! the answer. Each read gives none where gix cannot answer, and the caller
//! asks git then. Only a checkout is read here: a worker's clone goes through
//! `trusted_git`, and every write stays on git.

use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;
use gix::refs::TargetRef;
use gix::traverse::tree::{Recorder, recorder::Location};

/// The commit the ref `full_ref` names in `repo`, none where git must answer
///
/// The id is not peeled, as `git rev-parse --verify` gives it. A ref that is
/// absent or symbolic, in a reftable or a sha256 repo, or that gix fails to
/// read, is none.
pub(super) fn commit_of(repo: &Path, full_ref: &str) -> Option<String> {
    let repo = open(repo)?;
    let found = repo.try_find_reference(full_ref).ok()??;
    // A name that is not the one asked for was found by a search prefix.
    if found.name().as_bstr() != full_ref {
        return None;
    }
    match found.target() {
        TargetRef::Object(id) => Some(id.to_hex().to_string()),
        TargetRef::Symbolic(_) => None,
    }
}

/// Whether `ancestor` is an ancestor of `descendant` in `repo`, or the same
/// commit, none where git must answer
///
/// Both are revisions as `git merge-base --is-ancestor` reads them. Histories
/// with nothing in common answer no. A commit `repo` lacks, a revision gix
/// cannot resolve or a history it fails to walk is none.
pub(super) fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    let repo = open(repo)?;
    let one = commit_id(&repo, ancestor)?;
    let two = commit_id(&repo, descendant)?;
    // The best base of a commit and its descendant is the commit itself.
    let base = repo.merge_base(one, two).ok()?;
    Some(base.is_some_and(|base| base == one))
}

/// A file in a commit's tree, as `ls-tree -r` lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    /// The path in the tree, from the root, with `/` between folders
    pub path: String,
    /// The blob's id, or the commit's for a submodule
    pub id: String,
    /// Whether it is a regular file (mode 100644 or 100755), not a link or a submodule
    pub regular: bool,
}

/// Every file in the tree of the revision `rev` in `repo`, under `folders` where there are some,
/// in the order `ls-tree -r --name-only -z` lists them, none where git must answer
///
/// A folder is a plain path from the root of the tree, and a file at that
/// path is listed too, as a literal pathspec reads it. A path that is
/// anything else, a revision gix cannot resolve to a tree, a repo with
/// replacement refs or a tree gix fails to read is none.
pub fn tree_files(repo: &Path, rev: &str, folders: &[&Path]) -> Option<Vec<TreeFile>> {
    let folders: Vec<String> = folders
        .iter()
        .map(|f| plain_path(f))
        .collect::<Option<_>>()?;
    let repo = open(repo)?;
    let tree = whole(&repo)?
        .rev_parse_single(rev)
        .ok()?
        .object()
        .ok()?
        .peel_to_tree()
        .ok()?;
    let mut recorder = Recorder::default().track_location(Some(Location::Path));
    tree.traverse().depthfirst(&mut recorder).ok()?;
    let listed = recorder.records.into_iter().filter(|e| !e.mode.is_tree());
    let files = listed.map(|entry| TreeFile {
        path: entry.filepath.to_str_lossy().into_owned(),
        id: entry.oid.to_hex().to_string(),
        regular: entry.mode.is_blob(),
    });
    Some(
        files
            .filter(|file| {
                folders.is_empty()
                    || folders.iter().any(|folder| {
                        let rest = file.path.strip_prefix(folder.as_str());
                        rest.is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
                    })
            })
            .collect(),
    )
}

/// The bytes of the blob each of `specs` names in `repo`, as `cat-file --batch` gives them, none
/// for a spec with no such blob, and none overall where git must answer
///
/// A spec is a full object id or `<revision>:<path>` with a plain path. Any
/// other spec, a revision gix cannot resolve, a spec that names something
/// but a blob, a repo with replacement refs or an object gix fails to read
/// is none overall.
pub fn blobs(repo: &Path, specs: &[&str]) -> Option<Vec<Option<Vec<u8>>>> {
    let repo = open(repo)?;
    let repo = whole(&repo)?;
    specs.iter().map(|spec| blob(repo, spec)).collect()
}

// The blob `spec` names: some none if nothing is there, none for what gix cannot say.
fn blob(repo: &gix::Repository, spec: &str) -> Option<Option<Vec<u8>>> {
    let object = match spec.split_once(':') {
        None => {
            let id = gix::ObjectId::from_hex(spec.as_bytes()).ok()?;
            repo.try_find_object(id).ok()?
        }
        Some((rev, path)) => {
            let tree = repo
                .rev_parse_single(rev)
                .ok()?
                .object()
                .ok()?
                .peel_to_tree()
                .ok()?;
            let names: Vec<&str> = path.split('/').collect();
            if names.iter().any(|name| matches!(*name, "" | "." | "..")) {
                return None;
            }
            match tree.lookup_entry(names).ok()? {
                None => None,
                Some(entry) => Some(entry.object().ok()?),
            }
        }
    };
    match object {
        None => Some(None),
        Some(object) if object.kind == gix::object::Kind::Blob => Some(Some(object.data.clone())),
        Some(_) => None,
    }
}

// `path` as a plain path in a tree, none for one git would read otherwise.
fn plain_path(path: &Path) -> Option<String> {
    let mut names = Vec::new();
    for part in path.components() {
        let Component::Normal(name) = part else {
            return None;
        };
        names.push(name.to_str()?);
    }
    (!names.is_empty()).then(|| names.join("/"))
}

// `repo` if it has no replacement refs, which `--no-replace-objects` ignores and gix reads.
fn whole(repo: &gix::Repository) -> Option<&gix::Repository> {
    let refs = repo.references().ok()?;
    let mut replaced = refs.prefixed("refs/replace/").ok()?;
    replaced.next().is_none().then_some(repo)
}

/// The values the local config of `repo` holds for the keys `wanted` accepts, as
/// `config --local --null --get-regexp` gives them, none where git must answer
///
/// Each key is `section.subsection.name` as git writes it: the section and
/// name in lower case and the subsection as it is. Keys come in the order
/// the file holds them, one entry for each value. A config with an include,
/// a legacy `[section.subsection]` header or a key with no value is none.
pub fn local_config(repo: &Path, wanted: impl Fn(&str) -> bool) -> Option<Vec<(String, String)>> {
    use gix::config::Source;

    let repo = open(repo)?;
    let snapshot = repo.config_snapshot();
    let mut found = Vec::new();
    for section in snapshot.plumbing().sections() {
        if section.meta().source != Source::Local {
            continue;
        }
        let header = section.header();
        let section_name = header.name().to_str().ok()?.to_ascii_lowercase();
        if header.is_legacy() || section_name == "include" || section_name == "includeif" {
            return None;
        }
        let body = section.body();
        let mut seen = std::collections::BTreeMap::new();
        for name in body.value_names() {
            let name = name.to_ascii_lowercase();
            let key = match header.subsection_name() {
                Some(sub) => format!("{section_name}.{}.{name}", sub.to_str().ok()?),
                None => format!("{section_name}.{name}"),
            };
            let nth = seen.entry(name.clone()).or_insert(0usize);
            let values = body.values(&name);
            let value = values.get(*nth)?.to_str().ok()?.to_owned();
            *nth += 1;
            if wanted(&key) {
                found.push((key, value));
            }
        }
        // A key with no value is left out of `values`, so the counts differ.
        let counted: usize = seen.values().sum();
        if counted != body.value_names().count() {
            return None;
        }
    }
    Some(found)
}

/// The common git dir of the repo `path` is in, as `rev-parse --path-format=absolute
/// --git-common-dir` gives it, none where git must answer
///
/// `path` is the repo's folder or one inside it.
pub fn common_dir(path: &Path) -> Option<PathBuf> {
    let found = gix::discover_opts(
        path,
        gix::discover::upwards::Options::default(),
        gix::open::Options::isolated(),
    )
    .ok()?;
    let common = found.common_dir();
    common.is_absolute().then(|| common.to_owned())
}

// The commit the revision `spec` names, none if it is absent or not a commit.
fn commit_id(repo: &gix::Repository, spec: &str) -> Option<gix::ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?;
    Some(id.object().ok()?.peel_to_commit().ok()?.id)
}

// The repo at `path` with its own config only: no system, global, user or
// environment config, which a `GIT_CONFIG_GLOBAL` could point at anything.
fn open(path: &Path) -> Option<gix::Repository> {
    gix::open_opts(path, gix::open::Options::isolated()).ok()
}

#[cfg(test)]
mod tests {
    use shep_kelpie_test_support::git;

    use super::*;
    use crate::worktree::canonical;

    fn repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "-b", "main"]);
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "first"],
        );
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        (dir, head)
    }

    #[test]
    fn a_ref_reads_as_git_reads_it() {
        let (dir, head) = repo();
        git(dir.path(), &["branch", "packed"]);
        git(dir.path(), &["pack-refs", "--all"]);
        git(dir.path(), &["branch", "loose"]);

        for name in ["refs/heads/loose", "refs/heads/packed"] {
            let asked = git(dir.path(), &["rev-parse", "--verify", name]);
            assert_eq!(asked, head);
            assert_eq!(commit_of(dir.path(), name), Some(asked), "{name}");
        }
        assert_eq!(commit_of(dir.path(), "refs/heads/gone"), None);
    }

    // What `git merge-base --is-ancestor` says: yes, no, or none where it fails.
    fn asked_git(repo: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
        let status = std::process::Command::new("git")
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .current_dir(repo)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        match status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    }

    #[test]
    fn ancestry_answers_as_git_merge_base_is_ancestor_does() {
        let (dir, first) = repo();
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "second"],
        );
        let second = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "--quiet", "--orphan", "other"]);
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "unrelated"],
        );
        let unrelated = git(dir.path(), &["rev-parse", "HEAD"]);
        let missing = "0123456789abcdef0123456789abcdef01234567";

        let cases = [
            (&first, &second, Some(true)),
            (&second, &first, Some(false)),
            (&second, &second, Some(true)),
            (&first, &unrelated, Some(false)),
            (&first, &missing.to_owned(), None),
        ];
        for (ancestor, descendant, expected) in cases {
            assert_eq!(
                asked_git(dir.path(), ancestor, descendant),
                expected,
                "git: {ancestor} {descendant}"
            );
            assert_eq!(
                is_ancestor(dir.path(), ancestor, descendant),
                expected,
                "{ancestor} {descendant}"
            );
        }
        // A remote-tracking branch by its short name, as `base_of` asks.
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", &first],
        );
        assert_eq!(asked_git(dir.path(), "origin/main", &second), Some(true));
        assert_eq!(is_ancestor(dir.path(), "origin/main", &second), Some(true));
    }

    // What git prints for `args` in `dir`, byte for byte.
    fn asked(dir: &Path, args: &[&str]) -> Vec<u8> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        output.stdout
    }

    // A repo whose one commit holds `files`.
    fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "-b", "main"]);
        for (file, text) in files {
            shep_kelpie_test_support::write_in(dir.path(), file, text);
        }
        git(dir.path(), &["add", "--all"]);
        git(dir.path(), &["commit", "--quiet", "-m", "first"]);
        dir
    }

    fn names(files: &[TreeFile]) -> Vec<Vec<u8>> {
        files.iter().map(|f| f.path.clone().into_bytes()).collect()
    }

    // What `ls-tree -r --name-only -z` lists, one name each.
    fn listed(dir: &Path, rev: &str, folders: &[&str]) -> Vec<Vec<u8>> {
        let mut args = vec!["ls-tree", "-r", "--name-only", "-z", rev, "--"];
        args.extend(folders);
        let out = asked(dir, &args);
        let split = out.split(|b| *b == 0).filter(|name| !name.is_empty());
        split.map(<[u8]>::to_vec).collect()
    }

    #[test]
    fn a_trees_files_list_as_git_ls_tree_lists_them() {
        let dir = repo_with(&[
            ("README.md", "a\n"),
            (".claude/skills/a/SKILL.md", "a\n"),
            (".claude/skills/a-b/SKILL.md", "b\n"),
            (".claude/skills.md", "not a folder\n"),
            (".claude/agents/with space.md", "s\n"),
            ("docs/caf\u{e9}/\u{65e5}\u{672c}.md", "n\n"),
            ("z", "z\n"),
        ]);
        git(
            dir.path(),
            &["update-index", "--chmod=+x", "--add", "README.md"],
        );
        git(dir.path(), &["commit", "--quiet", "-m", "second"]);

        let all = tree_files(dir.path(), "HEAD", &[]).unwrap();
        assert_eq!(names(&all), listed(dir.path(), "HEAD", &[]));
        assert_eq!(all.len(), 7);

        let folder = [Path::new(".claude/skills")];
        let under = tree_files(dir.path(), "HEAD", &folder).unwrap();
        assert_eq!(
            names(&under),
            listed(dir.path(), "HEAD", &[".claude/skills"])
        );
        assert_eq!(under.len(), 2, "skills.md and a-b are not under skills/");

        let two = [Path::new(".claude/agents"), Path::new("docs")];
        let both = tree_files(dir.path(), "HEAD", &two).unwrap();
        assert_eq!(
            names(&both),
            listed(dir.path(), "HEAD", &[".claude/agents", "docs"])
        );
        let file = [Path::new("z")];
        let one = tree_files(dir.path(), "HEAD", &file).unwrap();
        assert_eq!(names(&one), listed(dir.path(), "HEAD", &["z"]));
        let gone = [Path::new("nowhere")];
        assert_eq!(tree_files(dir.path(), "HEAD", &gone), Some(Vec::new()));

        // A commit by its id, as the board asks, and a remote-tracking branch.
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", &head],
        );
        for rev in [head.as_str(), "origin/main"] {
            let named = tree_files(dir.path(), rev, &[]).unwrap();
            assert_eq!(names(&named), listed(dir.path(), rev, &[]), "{rev}");
        }
    }

    #[test]
    fn a_trees_files_say_which_are_regular_and_their_ids() {
        let dir = repo_with(&[("a.txt", "a\n"), ("run.sh", "#!/bin/sh\n")]);
        git(dir.path(), &["update-index", "--chmod=+x", "run.sh"]);
        let id = git(dir.path(), &["rev-parse", "HEAD"]);
        let link = format!(
            "120000,{},link",
            git(dir.path(), &["rev-parse", "HEAD:a.txt"])
        );
        git(dir.path(), &["update-index", "--add", "--cacheinfo", &link]);
        let module = format!("160000,{id},sub");
        git(
            dir.path(),
            &["update-index", "--add", "--cacheinfo", &module],
        );
        git(dir.path(), &["commit", "--quiet", "-m", "second"]);

        let files = tree_files(dir.path(), "HEAD", &[]).unwrap();
        let entries = git(dir.path(), &["ls-tree", "-r", "HEAD"]);
        for (file, line) in files.iter().zip(entries.lines()) {
            let (meta, path) = line.split_once('\t').unwrap();
            let meta: Vec<&str> = meta.split(' ').collect();
            assert_eq!(file.path, path);
            assert_eq!(file.id, meta[2], "{path}");
            assert_eq!(
                file.regular,
                matches!(meta[0], "100644" | "100755"),
                "{path}"
            );
        }
        assert_eq!(files.len(), 4);
    }

    #[test]
    fn blobs_read_as_git_cat_file_batch_reads_them() {
        let dir = repo_with(&[("a.txt", "line\nbreaks\n\n"), ("deep/er/b.bin", "")]);
        let id = git(dir.path(), &["rev-parse", "HEAD:a.txt"]);
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        let absent = "0123456789abcdef0123456789abcdef01234567";
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", &head],
        );

        let specs = [
            id.as_str(),
            absent,
            "HEAD:a.txt",
            "origin/main:deep/er/b.bin",
            "origin/main:gone.txt",
            "origin/main:a.txt/inside",
        ];
        let read = blobs(dir.path(), &specs).unwrap();

        let batch = |spec: &str| {
            let mut child = std::process::Command::new("git")
                .args(["cat-file", "--batch"])
                .current_dir(dir.path())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(
                &mut child.stdin.take().unwrap(),
                format!("{spec}\n").as_bytes(),
            )
            .unwrap();
            child.wait_with_output().unwrap().stdout
        };
        for (spec, got) in specs.iter().zip(&read) {
            let said = batch(spec);
            match got {
                Some(bytes) => {
                    let mut want = format!("{} blob {}\n", id_of(&said), bytes.len()).into_bytes();
                    want.extend(bytes);
                    want.push(b'\n');
                    assert_eq!(said, want, "{spec}");
                }
                None => assert!(said.ends_with(b" missing\n"), "{spec}"),
            }
        }
        assert_eq!(read[0].as_deref(), Some(&b"line\nbreaks\n\n"[..]));
        assert_eq!(read[1], None);
        assert_eq!(read[2].as_deref(), Some(&b"line\nbreaks\n\n"[..]));
        assert_eq!(read[3].as_deref(), Some(&b""[..]));
        assert_eq!(&read[4..], [None, None]);
    }

    // The id on the first line of `cat-file --batch`'s answer.
    fn id_of(said: &[u8]) -> String {
        String::from_utf8_lossy(said)
            .split(' ')
            .next()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn a_blob_spec_naming_a_tree_or_an_odd_path_is_left_to_git() {
        let dir = repo_with(&[("deep/b.txt", "b\n")]);

        for spec in [
            "HEAD:deep",
            "HEAD:",
            "HEAD:./deep/b.txt",
            "HEAD:deep//b.txt",
            "HEAD^{tree}",
        ] {
            assert_eq!(blobs(dir.path(), &[spec]), None, "{spec}");
        }
    }

    #[test]
    fn local_config_reads_as_git_config_local_does() {
        let (dir, _) = repo();
        let path = dir.path();
        let sets: [&[&str]; 6] = [
            &["config", "remote.origin.url", "first"],
            &["config", "Remote.origin.PushURL", "push"],
            &["config", "core.hooksPath", ".hooks"],
            &["config", "remote.Origin.url", "other case subsection"],
            &["config", "--add", "remote.origin.url", "last wins"],
            &["config", "branch.main.remote", "origin"],
        ];
        for args in sets {
            git(path, args);
        }
        let any = |_: &str| true;
        let ours = local_config(path, any).unwrap();
        let said = asked(path, &["config", "--local", "--null", "--get-regexp", "."]);
        let theirs: Vec<(String, String)> = String::from_utf8(said)
            .unwrap()
            .split('\0')
            .filter_map(|entry| entry.split_once('\n'))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        assert_eq!(ours, theirs);

        let wanted = |key: &str| key == "remote.origin.url";
        let urls = local_config(path, wanted).unwrap();
        let last = urls.last().map(|(_, value)| value.as_str());
        assert_eq!(last, Some("last wins"));
        assert_eq!(urls.len(), 2);
        assert_eq!(
            local_config(path, |key| key == "core.nothing"),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_config_an_include_or_a_bare_key_could_change_is_left_to_git() {
        let (dir, _) = repo();
        let path = dir.path();
        let config = path.join(".git/config");
        let text = std::fs::read_to_string(&config).unwrap();

        std::fs::write(&config, format!("{text}[core]\n\tbare_key\n")).unwrap();
        assert_eq!(local_config(path, |_| true), None, "a key with no value");
        std::fs::write(&config, format!("{text}[remote.legacy]\n\turl = x\n")).unwrap();
        assert_eq!(local_config(path, |_| true), None, "a legacy header");
        std::fs::write(&config, format!("{text}[include]\n\tpath = other\n")).unwrap();
        assert_eq!(local_config(path, |_| true), None, "an include");
        std::fs::write(
            &config,
            format!("{text}[includeIf \"gitdir:/\"]\n\tpath = o\n"),
        )
        .unwrap();
        assert_eq!(local_config(path, |_| true), None, "a conditional include");
    }

    #[test]
    fn the_common_dir_reads_as_git_does() {
        let (dir, _) = repo();
        let inside = dir.path().join("a/b");
        std::fs::create_dir_all(&inside).unwrap();
        let linked = dir.path().join("linked");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );

        for from in [dir.path(), inside.as_path(), linked.as_path()] {
            let said = asked(
                from,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            );
            let said = PathBuf::from(String::from_utf8(said).unwrap().trim_end());
            let read = common_dir(from).unwrap();
            assert_eq!(
                read.canonicalize().unwrap(),
                said.canonicalize().unwrap(),
                "{from:?}"
            );
        }
        assert_eq!(common_dir(&dir.path().join("gone")), None);
    }

    #[test]
    fn a_repo_gix_cannot_read_falls_back_to_git() {
        // A sha256 repo: gix is built for sha1 ids only.
        let dir = tempfile::tempdir().unwrap();
        git(
            dir.path(),
            &["init", "--quiet", "-b", "main", "--object-format=sha256"],
        );
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "first"],
        );
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "second"],
        );
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["pack-refs", "--all"]);

        assert_eq!(commit_of(dir.path(), "refs/heads/main"), None);
        assert_eq!(is_ancestor(dir.path(), "HEAD~1", "HEAD"), None);
        assert_eq!(tree_files(dir.path(), "HEAD", &[]), None);
        assert_eq!(blobs(dir.path(), &["HEAD:gone"]), None);
        assert_eq!(local_config(dir.path(), |_| true), None);
        assert_eq!(common_dir(dir.path()), None);
        assert_eq!(
            crate::worktree::commit_of(dir.path(), "refs/heads/main"),
            Ok(head)
        );
        let common = crate::worktree::common_dir(dir.path()).unwrap();
        assert_eq!(common, canonical(&dir.path().join(".git")));
    }

    #[test]
    fn a_repo_with_replacement_refs_is_read_by_git() {
        let dir = repo_with(&[("a.txt", "a\n")]);
        let blob = git(dir.path(), &["rev-parse", "HEAD:a.txt"]);
        assert_eq!(blobs(dir.path(), &["HEAD:a.txt"]).unwrap().len(), 1);

        git(
            dir.path(),
            &["update-ref", &format!("refs/replace/{blob}"), &blob],
        );

        assert_eq!(tree_files(dir.path(), "HEAD", &[]), None);
        assert_eq!(blobs(dir.path(), &["HEAD:a.txt"]), None);
    }

    // The repo this test's child reads, which a test cannot set in its own
    // environment: `set_var` is unsafe, and the crate forbids that.
    const CHILD_REPO: &str = "KELPIE_IN_PROCESS_CHILD_REPO";

    #[test]
    fn global_config_is_not_read() {
        let (dir, head) = repo();
        let broken = dir.path().join("broken-config");
        std::fs::write(&broken, "[core\n\tbare = = \n[include]\n\tpath = /gone\n").unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "worktree::in_process::tests::child_reads_a_ref",
                "--nocapture",
            ])
            .env(CHILD_REPO, dir.path())
            .env("GIT_CONFIG_GLOBAL", &broken)
            .env("GIT_CONFIG_SYSTEM", &broken)
            .env("GIT_DIR", dir.path().join("nowhere"))
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains(&format!("child read {head}")), "{stdout}");
    }

    #[test]
    fn global_config_is_not_read_by_the_config_read() {
        let (dir, _) = repo();
        git(dir.path(), &["config", "remote.origin.url", "local"]);
        let global = dir.path().join("global-config");
        std::fs::write(
            &global,
            "[remote \"origin\"]\n\turl = global\n\tpushurl = global\n",
        )
        .unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "worktree::in_process::tests::child_reads_the_config",
                "--nocapture",
            ])
            .env(CHILD_REPO, dir.path())
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_CONFIG_SYSTEM", &global)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(
            stdout.contains("child read [remote.origin.url=local]"),
            "{stdout}"
        );
    }

    // Run by `global_config_is_not_read_by_the_config_read`, a no-op elsewhere.
    #[test]
    fn child_reads_the_config() {
        let Some(repo) = std::env::var_os(CHILD_REPO) else {
            return;
        };
        let read = local_config(Path::new(&repo), |key| key.starts_with("remote."));
        let read: Vec<String> = read
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        println!("child read [{}]", read.join(","));
    }

    // Run by `global_config_is_not_read` in a process of its own, and a no-op
    // anywhere else.
    #[test]
    fn child_reads_a_ref() {
        let Some(repo) = std::env::var_os(CHILD_REPO) else {
            return;
        };
        let read = commit_of(Path::new(&repo), "refs/heads/main");
        println!("child read {}", read.expect("the ref reads"));
    }
}
