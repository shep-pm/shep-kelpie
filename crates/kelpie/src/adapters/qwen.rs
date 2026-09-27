//! The maintainer's qwen-review script, run as a subprocess
//!
//! The script takes the GPU lock itself and waits in line for it, up to an
//! hour; a long wait here is normal, not a hang. This module only spawns
//! the script and waits on its exit status: it never reads or writes a lock
//! folder itself, so a round holding one for the whole hour is simply a
//! long-running child process from kelpie's side, never a wait on the lock.
//! A round's findings and its completion marker both come from disk, never
//! from stdout: a round killed mid-flight can leave a `round-N.txt` behind
//! with no `.done` beside it, and only the marker tells the two apart.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::process::{Processes, RunError};
use crate::ports::{Finding, Reviewer, ReviewerError, Severity, parse_findings};
use crate::worktree;

/// `qwen-review.sh`, under the maintainer's `~/.claude/scripts/`
const SCRIPT: &str = ".claude/scripts/qwen-review.sh";

/// `origin/main`: the ref every round diffs against
fn base_ref() -> String {
    format!("origin/{}", worktree::BASE)
}

/// The maintainer's qwen-review script
///
/// Clones share their rounds in flight, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct QwenReviewer {
    script: PathBuf,
    processes: Processes,
}

impl QwenReviewer {
    /// A reviewer running the script under the maintainer's `home`
    pub fn new(home: &Path) -> Self {
        Self {
            script: home.join(SCRIPT),
            processes: Processes::default(),
        }
    }

    /// Ends every round in flight, and refuses new ones, as the runner stops
    pub fn stop(&self) {
        self.processes.stop();
    }

    fn run(
        &self,
        worktree: &Path,
        out: &Path,
        round: u32,
        files: Option<&str>,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let mut command = Command::new(&self.script);
        command
            .arg("--dir")
            .arg(worktree)
            .arg("--round")
            .arg(round.to_string())
            .env("QWEN_REVIEW_OUT", out);
        match files {
            Some(files) => {
                command.arg("--files").arg(files);
            }
            None => {
                command.arg("--diff").arg(base_ref());
            }
        }
        let output = self.processes.output(&mut command).map_err(|e| match e {
            RunError::Io(e) => ReviewerError::Spawn(e.to_string()),
            RunError::Stopped => ReviewerError::Stopped,
            // `output` never sets a deadline, so this never fires.
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
    // `-U25` hunk against `origin/main`: the skill's own way of feeding it a
    // file small enough for the chunk limit. Findings against the hunk file
    // are folded back in against the original path.
    fn hunk_round(
        &self,
        worktree: &Path,
        out: &Path,
        round: u32,
        skipped: &Finding,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let diff = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["diff", &base_ref(), "-U25", "--"])
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
        let hunk_out = out.join("hunks").join(format!("{round}-out"));
        let files = hunk_path.to_string_lossy().into_owned();
        let findings = self.run(worktree, &hunk_out, round, Some(&files))?;
        Ok(findings
            .into_iter()
            .map(|f| Finding {
                file: skipped.file.clone(),
                ..f
            })
            .collect())
    }
}

impl Reviewer for QwenReviewer {
    fn round(
        &self,
        worktree: &Path,
        out: &Path,
        round: u32,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let findings = self.run(worktree, out, round, None)?;
        let mut combined = Vec::with_capacity(findings.len());
        for finding in findings {
            if is_skipped_for_size(&finding) {
                combined.extend(self.hunk_round(worktree, out, round, &finding)?);
            } else {
                combined.push(finding);
            }
        }
        Ok(combined)
    }
}

fn is_skipped_for_size(finding: &Finding) -> bool {
    finding.severity == Severity::Low
        && finding.line == 0
        && finding.what.starts_with("not reviewed: ")
        && finding.what.contains("exceeds the chunk limit")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

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
        std::fs::write(&script, contents).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let worktree = home.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let out = home.path().join("out");
        let reviewer = QwenReviewer::new(home.path());

        assert_eq!(reviewer.round(&worktree, &out, 1).unwrap(), vec![]);
        assert!(
            !lock.exists(),
            "the script's own lock is released once it finishes, \
             and kelpie never created or left it behind"
        );
    }

    // A script that exits 0 but never wrote the marker: a round killed
    // between its exit and the marker write, or one whose exit code lied.
    #[test]
    fn a_round_with_no_completion_marker_is_incomplete() {
        let home = tempfile::tempdir().unwrap();
        let script_dir = home.path().join(".claude/scripts");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("qwen-review.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\nexit 0\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let worktree = home.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let out = home.path().join("out");
        let reviewer = QwenReviewer::new(home.path());

        assert_eq!(
            reviewer.round(&worktree, &out, 1),
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
        std::fs::write(&script, contents).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let worktree = home.path().join("repo");
        std::fs::create_dir_all(worktree.join("sub/dir")).unwrap();
        crate::test::git(&worktree, &["init", "--quiet", "-b", "main"]);
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
        let reviewer = QwenReviewer::new(home.path());

        assert_eq!(
            reviewer.round(&worktree, &out, 1).unwrap(),
            vec![Finding {
                severity: Severity::Medium,
                file: "sub/dir/big.rs".into(),
                line: 5,
                what: "leftover debug print".into(),
                why: "noisy logs".into(),
            }]
        );
    }

    // Not a git repository, so the `-U25` diff itself fails.
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
        std::fs::write(&script, contents).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let worktree = home.path().join("not-a-repo");
        std::fs::create_dir_all(&worktree).unwrap();
        let out = home.path().join("out");
        let reviewer = QwenReviewer::new(home.path());

        let skip = Finding {
            severity: Severity::Low,
            file: "nope.rs".into(),
            line: 0,
            what: "not reviewed: 900 lines exceeds the chunk limit".into(),
            why: "split the file or review it by hand".into(),
        };
        assert_eq!(reviewer.round(&worktree, &out, 1).unwrap(), vec![skip]);
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
        std::fs::write(&script, contents).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let worktree = home.path().join("repo");
        std::fs::create_dir_all(worktree.join("sub/a")).unwrap();
        std::fs::create_dir_all(worktree.join("sub/b")).unwrap();
        crate::test::git(&worktree, &["init", "--quiet", "-b", "main"]);
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
        let reviewer = QwenReviewer::new(home.path());
        let findings = reviewer.round(&worktree, &out, 1).unwrap();
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
        assert!(is_skipped_for_size(&skip));

        let real_low = Finding {
            what: "unused import".into(),
            ..skip.clone()
        };
        assert!(!is_skipped_for_size(&real_low));

        let wrong_severity = Finding {
            severity: Severity::High,
            ..skip.clone()
        };
        assert!(!is_skipped_for_size(&wrong_severity));

        let has_a_line = Finding { line: 4, ..skip };
        assert!(!is_skipped_for_size(&has_a_line));
    }
}
