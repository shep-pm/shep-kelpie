//! An open branch's diff from `main`, read for the project manager, which
//! runs no git, cut to a size its context can take

use std::io::{self, Read};
use std::path::Path;
use std::process::Stdio;

use super::git::{Reading, commits};

/// The most of one diff kept, in bytes: about 16k tokens, so one huge
/// branch cannot flood its context, and it reads the worktree for the rest
pub(in crate::runner) const CAP: usize = 64 * 1024;

/// `head`'s diff from where it left `main`, cut at [`CAP`], or why git could not give it
///
/// `files` are the files it changes, as `git diff --name-only --no-renames`
/// lists them. `worktree` is where the worker's copy is. A cut diff's last
/// line names both, so the rest can be read there.
///
/// No external diff or text conversion runs, whatever git's config names.
pub(super) fn read(
    repo: &Reading,
    main: &str,
    head: &str,
    files: Option<&[String]>,
    worktree: &Path,
) -> Result<Vec<u8>, String> {
    cut(repo, &[format!("{main}...{head}")], files, worktree)
}

/// What `branch` changed on `origin/main` when its pull request merged,
/// cut at [`CAP`], or why git could not give it
///
/// Once the merge is fetched, the branch's diff from `main` is empty, so
/// this is the diff from the merge commit's first parent to the merge
/// commit. A merge that left no such commit, a squash or a rebase, has the
/// branch's changes still apart from `main`, so it is the diff `read` gives.
pub(in crate::runner) fn merged(
    repo: &Reading,
    branch: &str,
    worktree: &Path,
    base: &str,
) -> Result<Vec<u8>, String> {
    let main = format!("refs/remotes/origin/{base}");
    let main = commits(repo, [&main])
        .remove(&main)
        .ok_or_else(|| format!("{main} is not in the checkout"))?;
    let head = (repo.head(worktree, branch))
        .ok_or_else(|| format!("refs/heads/{branch} is not in the checkout"))?;
    let in_main = ["merge-base", "--is-ancestor", &head, &main];
    if !git_succeeds(repo, &in_main) {
        return read(repo, &main, &head, None, worktree);
    }
    let since = format!("{head}..{main}");
    let walk = [
        "rev-list",
        "--first-parent",
        "--ancestry-path",
        "--parents",
        "--reverse",
    ];
    let path = git_line(repo, &[&walk[..], &[&since]].concat()).unwrap_or_default();
    // Each line is a commit and its parents. The merge is the one whose
    // second parent is the branch's head: a later merge of another branch
    // also descends from the head, when it was fast-forwarded into main.
    let merge = path.lines().find_map(|line| {
        let mut ids = line.split_whitespace();
        let (merge, _first, second) = (ids.next()?, ids.next()?, ids.next()?);
        (second == head).then(|| merge.to_owned())
    });
    let Some(merge) = merge else {
        return Err(format!(
            "{branch} is on main, with no merge commit that took it in"
        ));
    };
    cut(repo, &[format!("{merge}^1"), merge], None, worktree)
}

// The first line git prints for `args`, if it succeeds and prints one
fn git_line(repo: &Reading, args: &[&str]) -> Option<String> {
    let output = repo
        .git()
        .arg("--no-replace-objects")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

// Whether git exits 0 for `args`
fn git_succeeds(repo: &Reading, args: &[&str]) -> bool {
    repo.git()
        .arg("--no-replace-objects")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

// The diff of `revs`, two for a pair of commits and one for a range, cut
// as `read` says
fn cut(
    repo: &Reading,
    revs: &[String],
    files: Option<&[String]>,
    worktree: &Path,
) -> Result<Vec<u8>, String> {
    let range = revs.join(" ");
    let mut child = repo
        .git()
        .arg("--no-replace-objects")
        .args(["diff", "--no-color", "--no-ext-diff", "--no-textconv"])
        // One section a file, in the order `--name-only` lists them.
        .arg("--no-renames")
        .args(revs)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("git did not run: {}", e.kind()))?;
    let read = match child.stdout.take() {
        Some(out) => kept(out),
        None => Err(io::Error::other("no output")),
    };
    let status = child
        .wait()
        .map_err(|e| format!("git did not end: {}", e.kind()))?;
    let (diff, total) = read.map_err(|e| format!("cannot read git's diff: {}", e.kind()))?;
    if !status.success() {
        return Err(format!("git diff {range} failed ({status})"));
    }
    Ok(capped(diff, total, files, worktree))
}

// The first `CAP` bytes of `out` and the one after, and how many it held,
// the rest drained.
fn kept(mut out: impl Read) -> io::Result<(Vec<u8>, u64)> {
    let mut diff = Vec::new();
    (&mut out).take(CAP as u64 + 1).read_to_end(&mut diff)?;
    let rest = io::copy(&mut out, &mut io::sink())?;
    let total = (diff.len() as u64).saturating_add(rest);
    Ok((diff, total))
}

// `diff`, the start of a diff `total` long, cut within `CAP` at a line's
// end, or at a character's where that would drop over half of what fits,
// as in a minified file's one line, with a last line naming its size, the
// files from the one cut on, and the worktree.
fn capped(mut diff: Vec<u8>, total: u64, files: Option<&[String]>, worktree: &Path) -> Vec<u8> {
    if total <= CAP as u64 {
        return diff;
    }
    let held = diff.get(..CAP).unwrap_or(&diff);
    let end = match held.iter().rposition(|&b| b == b'\n') {
        Some(at) if at >= CAP / 2 => at + 1,
        // A UTF-8 continuation byte is never the first of a character.
        _ => (0..=CAP)
            .rev()
            .find(|&at| diff.get(at).is_none_or(|b| b & 0xC0 != 0x80))
            .unwrap_or(0),
    };
    diff.truncate(end);
    // Each section opens with such a line, and no line of a file's own
    // starts with "diff": it starts with a space, a plus or a minus. A path
    // that changes between a file and a link has two sections under one
    // header line, which `--name-only` lists once.
    let mut headers = diff
        .split(|&b| b == b'\n')
        .filter(|line| line.starts_with(b"diff --git "))
        .collect::<Vec<_>>();
    headers.dedup();
    let shown = headers.len();
    let past = match files {
        // The last file shown is cut part-way, so it is read in the worktree too.
        Some(files) => format!(
            "; files cut or past the cut: {}",
            files[shown.saturating_sub(1).min(files.len())..]
                .iter()
                .map(|name| spelled(name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => String::new(),
    };
    let said = format!(
        "\n[kelpie cut this diff at {} KiB of its {total} bytes{past}; read the worktree for the rest at {}]\n",
        CAP / 1024,
        worktree.display()
    );
    diff.extend_from_slice(said.as_bytes());
    diff
}

// `name`, quoted where a control character would break the line it is on.
fn spelled(name: &str) -> String {
    match name.chars().any(char::is_control) {
        true => format!("{name:?}"),
        false => name.to_owned(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test::{git, write_in};

    #[test]
    fn a_fetched_merge_commit_gives_the_branchs_changes_though_it_is_on_main() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        write_in(repo, "a.txt", "base\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "base"]);
        git(repo, &["checkout", "--quiet", "-b", "feature"]);
        write_in(repo, "b.txt", "from the branch\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "feature"]);
        git(repo, &["checkout", "--quiet", "main"]);
        write_in(repo, "c.txt", "main moved\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "main moved"]);
        git(
            repo,
            &["merge", "--quiet", "--no-ff", "-m", "merge", "feature"],
        );
        write_in(repo, "d.txt", "after the merge\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "after"]);
        git(repo, &["update-ref", "refs/remotes/origin/main", "main"]);

        let diff = merged(&Reading::of(repo), "feature", Path::new("/work/7"), "main").unwrap();

        let diff = String::from_utf8(diff).unwrap();
        assert!(
            diff.contains("+++ b/b.txt\n@@ -0,0 +1 @@\n+from the branch\n"),
            "{diff}"
        );
        for other in ["a.txt", "c.txt", "d.txt"] {
            assert!(!diff.contains(other), "{other} is in:\n{diff}");
        }
    }

    #[test]
    fn a_missing_ref_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        write_in(repo, "a.txt", "base\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "base"]);
        let work = Path::new("/work/7");

        let neither = merged(&Reading::of(repo), "feature", work, "main").unwrap_err();
        assert_eq!(neither, "refs/remotes/origin/main is not in the checkout");

        git(repo, &["update-ref", "refs/remotes/origin/main", "main"]);
        let no_branch = merged(&Reading::of(repo), "feature", work, "main").unwrap_err();
        assert_eq!(no_branch, "refs/heads/feature is not in the checkout");

        git(repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
        git(repo, &["branch", "feature"]);
        let no_main = merged(&Reading::of(repo), "feature", work, "main").unwrap_err();
        assert_eq!(no_main, "refs/remotes/origin/main is not in the checkout");
    }

    #[test]
    fn a_fast_forwarded_branch_is_not_given_a_later_merge_of_another_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        write_in(repo, "a.txt", "base\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "base"]);
        git(repo, &["checkout", "--quiet", "-b", "feature"]);
        write_in(repo, "b.txt", "from the branch\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "feature"]);
        git(repo, &["checkout", "--quiet", "main"]);
        git(repo, &["merge", "--quiet", "--ff-only", "feature"]);
        git(repo, &["checkout", "--quiet", "-b", "other"]);
        write_in(repo, "other.txt", "unrelated\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "other"]);
        git(repo, &["checkout", "--quiet", "main"]);
        write_in(repo, "c.txt", "main moved\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "main moved"]);
        git(
            repo,
            &["merge", "--quiet", "--no-ff", "-m", "merge", "other"],
        );
        git(repo, &["update-ref", "refs/remotes/origin/main", "main"]);

        let diff = merged(&Reading::of(repo), "feature", Path::new("/work/7"), "main");

        let why = diff.expect_err("no commit took the branch in");
        assert!(why.contains("no merge commit that took it in"), "{why}");
    }

    #[test]
    fn a_squashed_branch_still_apart_from_main_gives_its_diff_from_main() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        write_in(repo, "a.txt", "base\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "base"]);
        git(repo, &["checkout", "--quiet", "-b", "feature"]);
        write_in(repo, "b.txt", "from the branch\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "feature"]);
        git(repo, &["checkout", "--quiet", "main"]);
        git(repo, &["merge", "--quiet", "--squash", "feature"]);
        git(repo, &["commit", "--quiet", "-m", "squashed"]);
        git(repo, &["update-ref", "refs/remotes/origin/main", "main"]);

        let diff = merged(&Reading::of(repo), "feature", Path::new("/work/7"), "main").unwrap();

        let diff = String::from_utf8(diff).unwrap();
        assert!(diff.contains("+++ b/b.txt\n"), "{diff}");
    }

    #[test]
    fn a_type_change_before_the_cut_is_one_file_and_the_cut_file_is_still_named() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        write_in(repo, "a.txt", "was a file\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "main"]);
        git(repo, &["checkout", "--quiet", "-b", "feature"]);
        // A file becoming a link is two sections of the diff, one name.
        std::fs::remove_file(repo.join("a.txt")).unwrap();
        std::os::unix::fs::symlink("elsewhere", repo.join("a.txt")).unwrap();
        let block = "one line of a file that fills the cap\n".repeat(1_200);
        for file in ["b.txt", "c.txt"] {
            write_in(repo, file, &block);
        }
        write_in(repo, "d.txt", "small\n");
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "feature"]);
        let files: Vec<String> = ["a.txt", "b.txt", "c.txt", "d.txt"]
            .map(str::to_owned)
            .into();

        let diff = read(
            &Reading::of(repo),
            "main",
            "feature",
            Some(&files),
            Path::new("/work/7"),
        )
        .unwrap();

        let diff = String::from_utf8(diff).unwrap();
        let last = diff.trim_end().lines().last().unwrap();
        assert!(diff.contains("+++ b/b.txt\n"), "b.txt is shown");
        assert!(
            last.contains("files cut or past the cut: c.txt, d.txt;"),
            "{last}"
        );
    }
}
