//! Writing a round's held findings to the file its fix turn reads

use std::path::{Path, PathBuf};

use super::calls::severity_tag;
use crate::ports::Finding;
use crate::runner::turn;

/// Where a round's held findings are written for the worker's next turn
const FINDINGS_FILE: &str = "review-findings.md";

/// The findings file's path, inside the project's worker folder
pub(in crate::runner) fn findings_path(worker_folder: &Path) -> PathBuf {
    worker_folder.join(FINDINGS_FILE)
}

pub(in crate::runner) fn write_findings_file(
    folder: &Path,
    path: &Path,
    round: u32,
    findings: &[Finding],
) -> Result<(), String> {
    let mut text = format!(
        "Round {round}'s held findings, at the judge's severity. Fix each one, then \
         commit and push.\n\n"
    );
    for f in findings {
        text.push_str(&format!(
            "{}|{}:{}|{}|{}\n",
            severity_tag(f.severity),
            f.file,
            f.line,
            f.what,
            f.why
        ));
    }
    turn::write(folder, path, &text)
}

pub(super) fn fix_prompt(number: u64, round: u32, count: usize, path: &Path) -> String {
    format!(
        "Round {round} of the qwen-review loop on your pull request #{number} held \
         {count} finding(s), in {}. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::ports::Severity;

    #[test]
    fn the_file_lists_one_finding_per_line_in_qwens_own_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review-findings.md");
        let findings = [
            Finding {
                severity: Severity::High,
                file: "src/lib.rs".into(),
                line: 9,
                what: "looks racy".into(),
                why: "two threads write the same field".into(),
            },
            Finding {
                severity: Severity::Low,
                file: "src/main.rs".into(),
                line: 1,
                what: "unused import".into(),
                why: "dead code".into(),
            },
        ];
        write_findings_file(dir.path(), &path, 3, &findings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("Round 3's held findings, at the judge's severity."),
            "{text}"
        );
        assert!(
            text.contains("HIGH|src/lib.rs:9|looks racy|two threads write the same field\n"),
            "{text}"
        );
        assert!(
            text.contains("LOW|src/main.rs:1|unused import|dead code\n"),
            "{text}"
        );
    }

    #[test]
    fn the_fix_prompt_names_the_round_the_pull_request_and_the_file() {
        let prompt = fix_prompt(71, 3, 2, Path::new("/k/worker/review-findings.md"));
        assert_eq!(
            prompt,
            "Round 3 of the qwen-review loop on your pull request #71 held 2 \
             finding(s), in /k/worker/review-findings.md. Fix each one, then \
             commit and push with `git push origin HEAD`."
        );
    }

    #[test]
    fn a_findings_file_that_cannot_be_written_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("readonly");
        std::fs::create_dir(&folder).unwrap();
        let mut perms = std::fs::metadata(&folder).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&folder, perms).unwrap();

        let finding = Finding {
            severity: Severity::Low,
            file: "a.rs".into(),
            line: 1,
            what: "nit".into(),
            why: "style".into(),
        };
        let err = write_findings_file(&folder, &folder.join("review-findings.md"), 1, &[finding])
            .unwrap_err();
        assert!(err.contains("review-findings.md"), "{err}");

        // Restore write access so the tempdir can clean itself up.
        let mut perms = std::fs::metadata(&folder).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&folder, perms).unwrap();
    }
}
