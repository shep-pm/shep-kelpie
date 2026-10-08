//! A local round's command, run as a subprocess
//!
//! The command is the maintainer's qwen-review script or anything that
//! keeps its contract, which the README sets out. The script takes the GPU
//! lock itself and waits in line for it, up to an hour; a long wait is not
//! a hang. A round's findings and its completion marker both come from
//! disk, never from stdout: only the marker tells a finished `round-N.txt`
//! from one a killed round left.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{LocalReviewer, queue};
use crate::adapters::process::RunError;
use crate::lease::gpu::GpuLock;
use crate::ports::{Finding, ReviewerError, RoundStage, parse_findings};
use crate::settings::LocalCommand;
use crate::worktree::Linked;

/// What every call of one round shares
#[derive(Clone, Copy)]
struct Round<'a> {
    script: &'a Path,
    worktree: Linked<'a>,
    base: &'a str,
    head: &'a str,
    round: u32,
    criteria: Option<&'a Path>,
    watch: Option<&'a (dyn Fn(RoundStage) + Sync)>,
}

impl LocalReviewer {
    /// Runs `local`'s command for round `round`, then again on a hunk of
    /// each file it skipped as too large, folding those findings back in
    /// against the original file
    #[expect(clippy::too_many_arguments, reason = "`round`'s own, and the watch")]
    pub(super) fn command_round(
        &self,
        local: &LocalCommand,
        worktree: Linked<'_>,
        base: &str,
        out: &Path,
        round: u32,
        criteria: &str,
        watch: Option<&(dyn Fn(RoundStage) + Sync)>,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let head = super::head(worktree)?;
        let criteria = write_criteria(out, criteria)?;
        let at = Round {
            script: &local.command,
            worktree,
            base,
            head: &head,
            round,
            criteria: criteria.as_deref(),
            watch,
        };
        let findings = self.run(&at, out, None)?;
        let mut combined = Vec::with_capacity(findings.len());
        let mut skipped_index = 0u32;
        for finding in findings {
            if finding.is_skipped_for_size() {
                // A hunk kelpie could not run leaves its file unreviewed, in
                // the script's own words, and one file's trouble never costs
                // the round every other finding it already has. A hunk whose
                // `git diff` failed keeps its placeholder.
                match self.hunk_round(&at, out, skipped_index, &finding) {
                    Ok(found) => combined.extend(found),
                    Err(ReviewerError::Stopped) => return Err(ReviewerError::Stopped),
                    Err(e) => combined.push(Finding {
                        what: format!("not reviewed: {}", first_line(&e.to_string())),
                        why: "the hunk's review failed".into(),
                        ..finding
                    }),
                }
                skipped_index += 1;
            } else {
                combined.push(finding);
            }
        }
        Ok(combined)
    }

    fn run(
        &self,
        at: &Round<'_>,
        out: &Path,
        files: Option<&str>,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let round = at.round;
        let Linked { repo, worktree } = at.worktree;
        let mut command = Command::new(at.script);
        // The script runs git in the worktree outside the sandbox, so its
        // git gets the git dirs named, as kelpie's own does.
        crate::worktree::trust_git_of(&mut command, repo, worktree)
            .map_err(|e| ReviewerError::Failed(e.to_string()))?;
        super::clear_round(out, round)?;
        command
            .arg("--dir")
            .arg(worktree)
            .arg("--round")
            .arg(round.to_string())
            .env("QWEN_REVIEW_OUT", out)
            .env("KELPIE_REVIEW_HEAD", at.head)
            .env("TMPDIR", &self.temp_dir);
        if let Some(criteria) = at.criteria {
            command.env("KELPIE_REVIEW_CRITERIA", criteria);
        }
        match files {
            Some(files) => {
                command.arg("--files").arg(files);
            }
            None => {
                command.arg("--diff").arg(at.base);
            }
        }
        let lock = GpuLock::under(&self.temp_dir);
        let output = std::thread::scope(|scope| {
            let (spawned, pid) = std::sync::mpsc::channel();
            if let Some(watch) = at.watch {
                let lock = &lock;
                scope.spawn(move || queue::watch_queue(lock, &pid, watch));
            }
            self.processes.output_telling(&mut command, None, &|pid| {
                let _ = spawned.send(pid);
            })
        })
        .map_err(|e| match e {
            RunError::Io(e) => ReviewerError::Spawn(e.to_string()),
            RunError::Stopped => ReviewerError::Stopped,
            // No deadline is set, so this never fires.
            RunError::TimedOut => ReviewerError::Failed("timed out".into()),
        })?;
        if !output.status.success() {
            return Err(ReviewerError::Failed(format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim(),
            )));
        }
        let done = out.join(format!("round-{round}.txt.done"));
        if !done.is_file() {
            return Err(ReviewerError::Incomplete);
        }
        let report = out.join(format!("round-{round}.txt"));
        let text = std::fs::read_to_string(&report)
            .map_err(|e| ReviewerError::Failed(format!("cannot read {}: {e}", report.display())))?;
        Ok(parse_findings(&text))
    }

    // A file the script skipped for size is reviewed again on its own, as a
    // `-U25` hunk against the round's base: the skill's own way of feeding it a
    // file small enough for the chunk limit. Findings against the hunk file
    // are folded back in against the original path.
    fn hunk_round(
        &self,
        at: &Round<'_>,
        out: &Path,
        skipped_index: u32,
        skipped: &Finding,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let round = at.round;
        let Linked { repo, worktree } = at.worktree;
        let diff = crate::worktree::trusted_command(repo, worktree)
            .map_err(|e| ReviewerError::Failed(e.to_string()))?
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                at.base,
                "-U25",
                "--",
            ])
            .arg(&skipped.file)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| ReviewerError::Spawn(e.to_string()))?;
        if !diff.status.success() {
            // The file kelpie cannot cut a hunk for keeps its placeholder,
            // rather than failing the whole round over one file.
            return Ok(vec![skipped.clone()]);
        }
        let hunk_dir = out.join("hunks").join(round.to_string());
        std::fs::create_dir_all(&hunk_dir).map_err(|e| {
            ReviewerError::Failed(format!("cannot create {}: {e}", hunk_dir.display()))
        })?;
        // The whole relative path, flattened: two skipped files that share a
        // basename in different folders (the script's own `raw/` responses
        // use the same convention) must not overwrite each other's hunk.
        let name = skipped.file.replace('/', "_");
        let hunk_path = hunk_dir.join(name);
        std::fs::write(&hunk_path, &diff.stdout).map_err(|e| {
            ReviewerError::Failed(format!("cannot write {}: {e}", hunk_path.display()))
        })?;
        // Its own output folder per skipped file, so a second hunk in the
        // same round does not overwrite the first's `round-N.txt` before
        // anyone can look at it.
        let hunk_out = out
            .join("hunks")
            .join(format!("{round}-out-{skipped_index}"));
        let files = hunk_path.to_string_lossy().into_owned();
        let findings = self.run(at, &hunk_out, Some(&files))?;
        Ok(findings
            .into_iter()
            .map(|f| Finding {
                file: skipped.file.clone(),
                ..f
            })
            .collect())
    }
}

/// Writes what the issue asks for beside the round's findings, for a
/// command that reads `KELPIE_REVIEW_CRITERIA`
fn write_criteria(out: &Path, criteria: &str) -> Result<Option<PathBuf>, ReviewerError> {
    if criteria.trim().is_empty() {
        return Ok(None);
    }
    let path = out.join("criteria.md");
    let cannot =
        |e: std::io::Error| ReviewerError::Failed(format!("cannot write {}: {e}", path.display()));
    std::fs::create_dir_all(out).map_err(cannot)?;
    std::fs::write(&path, criteria).map_err(cannot)?;
    Ok(Some(path))
}

// The line a `|`-separated report can carry.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ports::{Reviewer, Severity};
    use crate::settings::LocalRound;
    use crate::test::{Elsewhere, write_script};

    fn local(script: &Path) -> LocalRound {
        LocalRound::Command(LocalCommand {
            command: script.to_owned(),
            ollama: None,
            ollama_model: None,
            lease: None,
        })
    }

    // A repo and its worktree, from `crate::test::linked_worktree`.
    fn linked((repo, worktree): &(PathBuf, PathBuf)) -> Linked<'_> {
        Linked { repo, worktree }
    }

    // A stand-in that writes the head it was given as a finding's file.
    #[test]
    fn a_round_is_told_the_worktrees_head() {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("review");
        write_script(
            &script,
            "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\n\
             printf 'LOW|%s:1|seen|seen\\n' \"$KELPIE_REVIEW_HEAD\" > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
        );
        let wt = crate::test::linked_worktree(home.path());
        let head = crate::test::git(&wt.1, &["rev-parse", "HEAD"]);
        let out = home.path().join("out");
        let findings = LocalReviewer::default()
            .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
            .unwrap();
        assert_eq!(findings[0].file, head);
    }

    // A stand-in for the real script: it takes and releases a lock of its
    // own around the round, the way the maintainer's script takes the GPU
    // lock. Kelpie's side of the call never touches that path, so the round
    // still succeeds and the lock is gone once the script exits.
    #[test]
    fn a_round_never_touches_the_lock_the_script_takes_itself() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        let lock = home.path().join("gpu.lock");
        let contents = format!(
            "#!/bin/sh\nmkdir '{lock}'\nsleep 0.05\nrmdir '{lock}'\n\
             mkdir -p \"$QWEN_REVIEW_OUT\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
            lock = lock.display(),
        );
        write_script(&script, &contents);

        let wt = crate::test::linked_worktree(home.path());
        let out = home.path().join("out");
        let reviewer = LocalReviewer::default();

        assert_eq!(
            reviewer
                .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
                .unwrap(),
            vec![]
        );
        assert!(
            !lock.exists(),
            "the script's own lock is released once it finishes, \
             and kelpie never created or left it behind"
        );
    }

    // A stand-in that records the `TMPDIR` it was started with, as the real
    // script would build its GPU lock from it.
    #[test]
    fn a_round_runs_with_the_given_temp_folder() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        write_script(
            &script,
            "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\n\
             printf 'LOW|%s:1|seen|seen\\n' \"$TMPDIR\" > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
        );

        let wt = crate::test::linked_worktree(home.path());
        let out = home.path().join("out");
        let reviewer =
            LocalReviewer::default().with_temp_dir(PathBuf::from("/var/folders/xx/yy/T/"));

        let findings = reviewer
            .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "/var/folders/xx/yy/T/");
    }

    // A script that exits 0 but never wrote the marker: a round killed
    // between its exit and the marker write, or one whose exit code lied.
    #[test]
    fn a_round_with_no_completion_marker_is_incomplete() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        write_script(
            &script,
            "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\nexit 0\n",
        );

        let wt = crate::test::linked_worktree(home.path());
        let out = home.path().join("out");
        let reviewer = LocalReviewer::default();

        assert_eq!(
            reviewer.round(&local(&script), linked(&wt), "origin/main", &out, 1, ""),
            Err(ReviewerError::Incomplete)
        );
    }

    // The script runs outside the worker's sandbox, and runs git in the
    // worktree its worker writes.
    #[test]
    fn the_scripts_git_never_follows_a_git_file_pointed_elsewhere() {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("review");
        write_script(
            &script,
            "#!/bin/sh\ngit -C \"$2\" diff --name-only HEAD || exit 1\n\
             mkdir -p \"$QWEN_REVIEW_OUT\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
        );
        let wt = crate::test::linked_worktree(home.path());
        let elsewhere = Elsewhere::copy_of(&wt.0, home.path());
        elsewhere.as_git_dir_of(&wt.1);
        let out = home.path().join("out");

        let findings = LocalReviewer::default()
            .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
            .unwrap();
        assert_eq!(findings, vec![]);
        assert!(
            !elsewhere.ran.exists(),
            "the script's git started the program"
        );
        elsewhere.assert_plain_git_starts_it(&wt.1);
    }

    // The last run's findings and marker are still on disk: a rework, or a
    // retried round, under the same issue's folder.
    #[test]
    fn a_round_never_reads_the_last_runs_findings() {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("review");
        write_script(
            &script,
            "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\nexit 0\n",
        );
        let wt = crate::test::linked_worktree(home.path());
        let out = home.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("round-1.txt"), "HIGH|old.rs:1|old|old\n").unwrap();
        std::fs::write(out.join("round-1.txt.done"), "").unwrap();
        assert_eq!(
            LocalReviewer::default().round(
                &local(&script),
                linked(&wt),
                "origin/main",
                &out,
                1,
                ""
            ),
            Err(ReviewerError::Incomplete)
        );
    }

    // A fake script whose main round reports one file as skipped for size,
    // and whose hunk round (given `--files`) reports one real finding
    // against whatever path it was handed. Proves the hunk's own finding is
    // folded back in against the ORIGINAL file, whatever the hunk round
    // named it, and that a real `-U25` diff runs without error.
    #[test]
    fn a_skipped_file_is_cut_into_a_hunk_and_its_findings_are_remapped() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        let contents = "#!/bin/sh
mkdir -p \"$QWEN_REVIEW_OUT\"
case \"$*\" in
  *--files*)
    printf 'MEDIUM|whatever-the-hunk-file-is-called:5|leftover debug print|noisy logs\\n' \\
      > \"$QWEN_REVIEW_OUT/round-1.txt\"
    ;;
  *)
    printf 'LOW|sub/dir/big.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\\n' \\
      > \"$QWEN_REVIEW_OUT/round-1.txt\"
    ;;
esac
: > \"$QWEN_REVIEW_OUT/round-1.txt.done\"
";
        write_script(&script, contents);

        let wt = crate::test::linked_worktree(home.path());
        let worktree = wt.1.clone();
        std::fs::create_dir_all(worktree.join("sub/dir")).unwrap();
        std::fs::write(worktree.join("sub/dir/big.rs"), "fn old() {}\n").unwrap();
        crate::test::git(&worktree, &["add", "."]);
        crate::test::git(&worktree, &["commit", "--quiet", "-m", "init"]);
        let base = crate::test::git(&worktree, &["rev-parse", "HEAD"]);
        crate::test::git(
            &worktree,
            &["update-ref", "refs/remotes/origin/main", &base],
        );
        std::fs::write(
            worktree.join("sub/dir/big.rs"),
            "fn old() {}\nfn new_one() {}\n",
        )
        .unwrap();
        crate::test::git(&worktree, &["commit", "--quiet", "-am", "change"]);

        let out = home.path().join("out");
        let reviewer = LocalReviewer::default();

        assert_eq!(
            reviewer
                .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
                .unwrap(),
            vec![Finding {
                severity: Severity::Medium,
                file: "sub/dir/big.rs".into(),
                line: 5,
                what: "leftover debug print".into(),
                why: "noisy logs".into(),
            }]
        );
    }

    // A repo with no `origin/main`, so the `-U25` diff itself fails.
    #[test]
    fn a_git_diff_failure_keeps_the_skip_placeholder() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        let contents = "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\n\
             printf 'LOW|nope.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\\n' \\
             > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n";
        write_script(&script, contents);

        let wt = crate::test::linked_worktree(home.path());
        let out = home.path().join("out");
        let reviewer = LocalReviewer::default();

        let skip = Finding {
            severity: Severity::Low,
            file: "nope.rs".into(),
            line: 0,
            what: "not reviewed: 900 lines exceeds the chunk limit".into(),
            why: "split the file or review it by hand".into(),
        };
        assert_eq!(
            reviewer
                .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
                .unwrap(),
            vec![skip]
        );
    }

    // The hunk's own script invocation fails outright (not the `git diff`
    // step): the file is left unreviewed, as a script that could not reach
    // its model would say, and everything else the round already found
    // survives.
    #[test]
    fn a_hunk_script_failure_leaves_the_file_unreviewed_and_keeps_the_rounds_other_findings() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        let contents = "#!/bin/sh
mkdir -p \"$QWEN_REVIEW_OUT\"
case \"$*\" in
  *--files*)
    echo 'curl: (7) Failed to connect to gpu.box port 8080' >&2
    exit 1
    ;;
  *)
    printf 'MEDIUM|src/lib.rs:9|a real finding|it matters\\n' > \"$QWEN_REVIEW_OUT/round-1.txt\"
    printf 'LOW|sub/big.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\\n' \\
      >> \"$QWEN_REVIEW_OUT/round-1.txt\"
    ;;
esac
: > \"$QWEN_REVIEW_OUT/round-1.txt.done\"
";
        write_script(&script, contents);

        let wt = crate::test::linked_worktree(home.path());
        let worktree = wt.1.clone();
        std::fs::create_dir_all(worktree.join("sub")).unwrap();
        std::fs::write(worktree.join("sub/big.rs"), "fn big() {}\n").unwrap();
        crate::test::git(&worktree, &["add", "."]);
        crate::test::git(&worktree, &["commit", "--quiet", "-m", "init"]);
        let base = crate::test::git(&worktree, &["rev-parse", "HEAD"]);
        crate::test::git(
            &worktree,
            &["update-ref", "refs/remotes/origin/main", &base],
        );
        std::fs::write(worktree.join("sub/big.rs"), "fn big() {}\nfn more() {}\n").unwrap();
        crate::test::git(&worktree, &["commit", "--quiet", "-am", "change"]);

        let out = home.path().join("out");
        let reviewer = LocalReviewer::default();
        let findings = reviewer
            .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
            .unwrap();
        assert_eq!(
            findings,
            vec![
                Finding {
                    severity: Severity::Medium,
                    file: "src/lib.rs".into(),
                    line: 9,
                    what: "a real finding".into(),
                    why: "it matters".into(),
                },
                Finding {
                    severity: Severity::Low,
                    file: "sub/big.rs".into(),
                    line: 0,
                    what: "not reviewed: the local round failed: curl: (7) Failed to connect to \
                            gpu.box port 8080"
                        .into(),
                    why: "the hunk's review failed".into(),
                },
            ]
        );
        assert!(findings[1].is_unreviewed());
    }

    // Two files skipped in the same round, sharing a basename in different
    // folders: their hunk files must not overwrite each other on disk.
    #[test]
    fn two_skipped_files_sharing_a_basename_keep_separate_hunk_files() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        let contents = "#!/bin/sh
mkdir -p \"$QWEN_REVIEW_OUT\"
case \"$*\" in
  *--files*)
    printf 'MEDIUM|whatever:1|a finding|a reason\\n' > \"$QWEN_REVIEW_OUT/round-1.txt\"
    ;;
  *)
    printf 'LOW|sub/a/util.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\\n' \\
      > \"$QWEN_REVIEW_OUT/round-1.txt\"
    printf 'LOW|sub/b/util.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand\\n' \\
      >> \"$QWEN_REVIEW_OUT/round-1.txt\"
    ;;
esac
: > \"$QWEN_REVIEW_OUT/round-1.txt.done\"
";
        write_script(&script, contents);

        let wt = crate::test::linked_worktree(home.path());
        let worktree = wt.1.clone();
        std::fs::create_dir_all(worktree.join("sub/a")).unwrap();
        std::fs::create_dir_all(worktree.join("sub/b")).unwrap();
        std::fs::write(worktree.join("sub/a/util.rs"), "fn a() {}\n").unwrap();
        std::fs::write(worktree.join("sub/b/util.rs"), "fn b() {}\n").unwrap();
        crate::test::git(&worktree, &["add", "."]);
        crate::test::git(&worktree, &["commit", "--quiet", "-m", "init"]);
        let base = crate::test::git(&worktree, &["rev-parse", "HEAD"]);
        crate::test::git(
            &worktree,
            &["update-ref", "refs/remotes/origin/main", &base],
        );
        std::fs::write(worktree.join("sub/a/util.rs"), "fn a() {}\nfn a2() {}\n").unwrap();
        std::fs::write(worktree.join("sub/b/util.rs"), "fn b() {}\nfn b2() {}\n").unwrap();
        crate::test::git(&worktree, &["commit", "--quiet", "-am", "change"]);

        let out = home.path().join("out");
        let reviewer = LocalReviewer::default();
        let findings = reviewer
            .round(&local(&script), linked(&wt), "origin/main", &out, 1, "")
            .unwrap();
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert_eq!(findings[0].file, "sub/a/util.rs");
        assert_eq!(findings[1].file, "sub/b/util.rs");

        let hunk_dir = out.join("hunks").join("1");
        let mut names: Vec<_> = std::fs::read_dir(&hunk_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["sub_a_util.rs", "sub_b_util.rs"]);
    }

    #[test]
    fn only_the_scripts_own_skip_placeholder_is_treated_as_skipped_for_size() {
        let skip = Finding {
            severity: Severity::Low,
            file: "big.rs".into(),
            line: 0,
            what: "not reviewed: 900 lines exceeds the chunk limit".into(),
            why: "split the file or review it by hand".into(),
        };
        assert!(skip.is_skipped_for_size());

        let real_low = Finding {
            what: "unused import".into(),
            ..skip.clone()
        };
        assert!(!real_low.is_skipped_for_size());

        let wrong_severity = Finding {
            severity: Severity::High,
            ..skip.clone()
        };
        assert!(!wrong_severity.is_skipped_for_size());

        let has_a_line = Finding { line: 4, ..skip };
        assert!(!has_a_line.is_skipped_for_size());
    }
}
