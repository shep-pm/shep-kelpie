//! Reads of kelpie's checkout made in this process, which spare starting git
//!
//! Starting git costs tens of milliseconds on Windows. The repo is opened
//! with its own config only, never the system, global, user or environment
//! config, so a read answers as git does only where those could not change
//! the answer. Each read gives none where gix cannot answer, and the caller
//! asks git then. Only a checkout is read here: a worker's clone goes through
//! `trusted_git`, and every write stays on git.

use std::path::Path;

use gix::refs::TargetRef;

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
        assert_eq!(
            crate::worktree::commit_of(dir.path(), "refs/heads/main"),
            Ok(head)
        );
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
