//! A reviewer's test, run before the fix and after it
//!
//! With `ci.test_command` set, a finding may carry a failing test the reviewer
//! wrote, kept in `<worker>/review-tests-<ticket>` ([`super::attached`]). Once
//! the head a round read is known, each is put in the worktree at its path, run
//! through the [`TestRunner`](crate::ports::TestRunner) and taken out again; when
//! the fix has moved the head, the run is made again. The runs are recorded in
//! `test_checks`. A test that fails after the fix sends the worker back for one
//! more fix turn, and one that still fails raises a `test-still-fails` ruling.
//! Any other outcome blocks nothing.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::super::Runner;
use super::super::git_reads::start_of;
use super::super::report::Begin;
use super::super::turn::ticket_name;
use super::attached::kept_folder;
use super::findings::{Sent, still_failing_prompt};
use crate::ports::{CEILING, Finding, TestOutcome, TestRun};
use crate::state::{FailingTest, Fix, StateError, Stuck};
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, ReviewStage, TestCheck, Turn, WorkItem};
use crate::worker_folder;

/// What a check says when the worktree is not at the head the reviewer read
const NOT_AT_THE_HEAD: &str = "the worktree is not at the head the reviewer read";

// What the worker folder holds while a test is in the worktree: a kelpie
// stopped mid-run leaves the file there, and this says so.
fn placed_name(ticket: TicketId) -> String {
    format!("test-placed-{ticket}")
}

impl Runner {
    /// The runs of `found`'s tests at `head`, the head the round read, one
    /// check each, with no run after the fix yet
    ///
    /// Nothing is run when the worktree is somewhere else: the outcome there
    /// would say nothing of what the reviewer read.
    pub(in crate::runner) fn tests_before(
        &self,
        round: u32,
        head: &str,
        found: &[Finding],
    ) -> Vec<TestCheck> {
        self.clear_stranded();
        let tested: Vec<(&Finding, &str)> = found
            .iter()
            .filter_map(|f| Some((f, f.test.as_deref()?)))
            .collect();
        if tested.is_empty() {
            return Vec::new();
        }
        let own = self.worktree_head().as_deref() == Some(head);
        tested
            .into_iter()
            .map(|(finding, test)| TestCheck {
                file: finding.file.clone(),
                line: finding.line,
                what: finding.what.clone(),
                round,
                before: match own {
                    true => self.run_kept(test),
                    false => TestOutcome::NotRun {
                        reason: NOT_AT_THE_HEAD.to_owned(),
                    },
                },
                after: None,
                sent_back: false,
            })
            .collect()
    }

    /// Runs the tests of `found`, the findings a fix was sent in `round`,
    /// at the worktree's head, and records each as the check's `after`
    ///
    /// Returns the round's checks as they now stand.
    ///
    /// # Errors
    ///
    /// [`StateError`] when the state cannot be saved.
    pub(super) fn tests_after(
        &mut self,
        round: u32,
        found: &[Finding],
    ) -> Result<Vec<TestCheck>, StateError> {
        self.clear_stranded();
        let mut ran = Vec::new();
        if let Some(item) = self.current() {
            for finding in found {
                let Some(test) = finding.test.as_deref() else {
                    continue;
                };
                if let Some(at) = waiting(&item.test_checks, round, finding) {
                    ran.push((at, self.run_kept(test)));
                }
            }
        }
        if !ran.is_empty() {
            self.update(|item| {
                for (at, outcome) in ran {
                    item.test_checks[at].after = Some(outcome);
                }
            })?;
        }
        Ok(self.round_checks(round, found))
    }

    /// The tests of `found` whose latest run after the fix failed, with the
    /// finding each is for and whether the worker was sent back to it
    pub(super) fn still_failing(&self, round: u32, found: &[Finding]) -> Vec<Failing> {
        let Some(item) = self.current() else {
            return Vec::new();
        };
        found
            .iter()
            .filter_map(|finding| {
                let test = finding.test.clone()?;
                let check = &item.test_checks[latest(&item.test_checks, round, finding)?];
                let Some(TestOutcome::Failed { output }) = &check.after else {
                    return None;
                };
                Some(Failing {
                    finding: finding.clone(),
                    test: FailingTest {
                        test,
                        output: output.clone(),
                    },
                    sent_back: check.sent_back,
                })
            })
            .collect()
    }

    /// What a fix whose tests still fail comes to: the worker's one more fix
    /// turn on them, or once it has had that, a ruling
    ///
    /// Returns the ruling's step when it raised one, and none when the worker
    /// was sent back, with its turn and the round's fix stage saved.
    ///
    /// # Errors
    ///
    /// [`StateError`] when the state cannot be saved.
    pub(super) fn tests_failed(
        &mut self,
        number: u64,
        review: Review,
        head: &str,
        sent: Sent<'_>,
        failing: Vec<Failing>,
    ) -> Result<Option<Begin>, StateError> {
        let round = review.round;
        let build = self
            .current()
            .expect("a fix is a work item's")
            .build
            .clone();
        let tests: Vec<FailingTest> = failing.iter().map(|f| f.test.clone()).collect();
        let prompt = still_failing_prompt(number, round, &build, &tests);
        let review = Review {
            stage: ReviewStage::Fixing {
                head: Some(head.to_owned()),
                sent: failing.iter().map(|f| f.finding.clone()).collect(),
                deferred_before: sent.deferred_before.to_vec(),
            },
            ..review
        };
        if failing.iter().all(|f| f.sent_back) {
            let kind = Stuck::TestStillFails {
                fix: Fix::Review(review),
                head: head.to_owned(),
                tests,
                prompt,
            };
            return self.raise(number, kind.into()).map(Some);
        }
        let marked: Vec<Finding> = failing.into_iter().map(|f| f.finding).collect();
        self.update(|item| {
            for finding in &marked {
                if let Some(at) = latest(&item.test_checks, round, finding) {
                    item.test_checks[at].sent_back = true;
                }
            }
            item.turn = Turn::Next { prompt };
            item.counts.fix_turns = item.counts.fix_turns.saturating_add(1);
            item.phase = Phase::Review(review);
        })?;
        Ok(None)
    }

    /// The checks `found`'s tests have in `round`, as recorded
    pub(super) fn round_checks(&self, round: u32, found: &[Finding]) -> Vec<TestCheck> {
        let Some(item) = self.current() else {
            return Vec::new();
        };
        found
            .iter()
            .filter(|f| f.test.is_some())
            .filter_map(|f| latest(&item.test_checks, round, f))
            .map(|at| item.test_checks[at].clone())
            .collect()
    }

    // Takes out the test a stopped kelpie left in the worktree, and says so: a
    // stray file is one the worker may commit and the next clean-worktree
    // check would rule on.
    fn clear_stranded(&self) {
        let Some(item) = self.current() else {
            return;
        };
        let ticket = item.issue;
        let name = placed_name(ticket);
        let text = match worker_folder::read(&self.paths.worker, Path::new(&name)) {
            Ok(Some(text)) => text,
            Ok(None) => return,
            Err(e) => {
                self.notes.push(format!(
                    "{}: cannot read {name}, so a test a stopped run left in the worktree is not looked for: {e}",
                    ticket_name(ticket)
                ));
                return;
            }
        };
        let mut paths = text.lines().map(PathBuf::from);
        let file = paths.next().filter(|file| inside(file));
        let kept = kept_folder(&self.paths.worker, ticket);
        // Only the reviewer's own text is taken out: a file that reads
        // otherwise is not what kelpie put there.
        let ours =
            file.as_deref()
                .is_some_and(|file| match worker_folder::read(&item.worktree, file) {
                    Ok(None) => true,
                    Ok(Some(held)) => {
                        matches!(worker_folder::read(&kept, file), Ok(Some(copy)) if copy == held)
                    }
                    Err(_) => false,
                });
        let record = self.paths.worker.join(&name);
        let left = match file {
            Some(file) if ours => Placed {
                root: item.worktree.clone(),
                file: Some(file),
                made: paths.filter(|dir| inside(dir)).collect(),
                record: Some(record),
            }
            .take_out(),
            Some(file) => {
                let mut left = vec![format!("{}: it is not the reviewer's text", file.display())];
                left.extend(
                    std::fs::remove_file(&record)
                        .err()
                        .map(|e| format!("{name}: {e}")),
                );
                left
            }
            None => vec![format!("{name}: it names no path inside the worktree")],
        };
        self.notes.push(match left.is_empty() {
            true => format!(
                "{}: kelpie was stopped during a test run, and took the test out of the worktree",
                ticket_name(ticket)
            ),
            false => format!(
                "{}: kelpie was stopped during a test run, and the worktree may be dirty, remove by hand: {}",
                ticket_name(ticket),
                left.join("; ")
            ),
        });
    }

    // The kept copy of `test` run in the worktree, which is left as it was.
    fn run_kept(&self, test: &str) -> TestOutcome {
        match self.try_run_kept(test) {
            Ok(outcome) => outcome,
            Err(reason) => TestOutcome::NotRun { reason },
        }
    }

    fn try_run_kept(&self, test: &str) -> Result<TestOutcome, String> {
        let item = self.current().ok_or("the work item is gone")?;
        let template = (self.settings.ci.test_command.as_ref())
            .ok_or("the project has no `ci.test_command` now")?;
        let command = fill(template.as_str(), test)?;
        let rel = Path::new(test);
        let ticket = item.issue;
        let kept = kept_folder(&self.paths.worker, ticket);
        let text = worker_folder::read(&kept, rel)
            .map_err(|e| format!("cannot read the kept copy of {test}: {e}"))?
            .ok_or_else(|| format!("the kept copy of {test} is gone"))?;
        // The worker's own fence, never the maintainer's reach: the file and
        // the code under it are a model's.
        let reach = self.worker_reach(item, start_of(item), None)?;
        let record = self.paths.worker.join(placed_name(ticket));
        let placed = Placed::put(&item.worktree, rel, &text, &record)?;
        let outcome = self.ports.test_runner.run(&TestRun {
            command,
            cwd: item.worktree.clone(),
            reach,
            settings: (self.paths.worker).join(format!("test-run-{}.sandbox.json", item.issue)),
            door: Some(self.paths.door.clone()),
            ceiling: CEILING,
        });
        // The outcome stands, but a worktree the run left dirty is the
        // maintainer's to know: the next run of this path refuses to start
        // over a file that is still there.
        let left = placed.take_out();
        if !left.is_empty() {
            self.notes.push(format!(
                "{}: the test {test} left the worktree dirty, remove by hand: {}",
                ticket_name(ticket),
                left.join("; ")
            ));
        }
        Ok(outcome)
    }
}

// Whether `path` is a path of one or more plain steps, so it stays below the
// folder it is joined to.
fn inside(path: &Path) -> bool {
    path.components().next().is_some()
        && (path.components()).all(|step| matches!(step, Component::Normal(_)))
}

// `template` with the test's path at `{test}` and its file stem at `{name}`.
// The command goes to a shell, so a path it could read as anything but a
// path is refused.
fn fill(template: &str, test: &str) -> Result<String, String> {
    // The path is joined to the worktree, so it must stay below it, whatever
    // the state file holds.
    if !inside(Path::new(test)) {
        return Err(format!(
            "the path {test} is not a relative path inside the worktree, so it is not run"
        ));
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/');
    if !test.chars().all(plain) || test.starts_with('-') {
        return Err(format!(
            "the path {test} holds characters a shell would read, so no command is built from it"
        ));
    }
    let name = Path::new(test)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("the path {test} has no file name"))?;
    let command = template.replace("{test}", test).replace("{name}", name);
    // An empty command is a shell that exits 0 having run nothing.
    match command.trim().is_empty() {
        true => Err("`ci.test_command` is empty, so there is nothing to run".to_owned()),
        false => Ok(command),
    }
}

// The newest check of `finding` in `round`, if any.
fn latest(checks: &[TestCheck], round: u32, finding: &Finding) -> Option<usize> {
    checks.iter().rposition(|c| c.is_for(round, finding))
}

/// Whether the worker was already sent back to `finding` in `round`, so an
/// earlier fix turn of the round was sent it
pub(super) fn was_sent_back(item: &WorkItem, round: u32, finding: &Finding) -> bool {
    latest(&item.test_checks, round, finding).is_some_and(|at| item.test_checks[at].sent_back)
}

/// A test that failed after the fix
pub(super) struct Failing {
    /// The finding it is attached to
    pub finding: Finding,
    /// The test and its output
    pub test: FailingTest,
    /// Whether the worker was already sent back to it
    pub sent_back: bool,
}

// The newest check of `finding` in `round` that the fix has not been run for:
// none was made yet, or the worker was sent back after a failed one.
fn waiting(checks: &[TestCheck], round: u32, finding: &Finding) -> Option<usize> {
    latest(checks, round, finding).filter(|at| {
        let check = &checks[*at];
        check.after.is_none()
            || (check.sent_back && matches!(check.after, Some(TestOutcome::Failed { .. })))
    })
}

// A test file put in the worktree, and the folders made for it
//
// Until it is taken out, a record of it sits in the worker folder, so a kelpie
// that is stopped in the middle of a run can take it out when it starts again.
struct Placed {
    root: PathBuf,
    // none when the worktree held the file already: it is the worker's
    file: Option<PathBuf>,
    // below `root`, top down
    made: Vec<PathBuf>,
    record: Option<PathBuf>,
}

impl Placed {
    // `text` at `rel` below `root`, with `record` written first.
    //
    // A file already there with this very text is the worker's committed copy: it
    // is run as it is, and left. Any other file there is left alone.
    fn put(root: &Path, rel: &Path, text: &str, record: &Path) -> Result<Self, String> {
        match root.join(rel).symlink_metadata() {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return match worker_folder::read(root, rel) {
                    Ok(Some(held)) if held == text => Ok(Self {
                        root: root.to_owned(),
                        file: None,
                        made: Vec::new(),
                        record: None,
                    }),
                    _ => Err("the worktree already holds another file of that name".to_owned()),
                };
            }
            Err(e) => return Err(format!("cannot look for it in the worktree: {e}")),
        }
        let mut made = Vec::new();
        let mut dir = PathBuf::new();
        for folder in rel.parent().into_iter().flat_map(Path::components) {
            dir.push(folder);
            if matches!(root.join(&dir).symlink_metadata(), Err(e) if e.kind() == io::ErrorKind::NotFound)
            {
                made.push(dir.clone());
            }
        }
        let placed = Self {
            root: root.to_owned(),
            file: Some(rel.to_owned()),
            made,
            record: Some(record.to_owned()),
        };
        if let Err(e) = placed.write_record() {
            return Err(format!(
                "cannot record the run, so the test is not put in the worktree: {e}"
            ));
        }
        match worker_folder::write(root, rel, text) {
            Ok(()) => Ok(placed),
            Err(e) => {
                let left = placed.take_out();
                let left = match left.is_empty() {
                    true => String::new(),
                    false => format!(" and cannot clear it: {}", left.join("; ")),
                };
                Err(format!("cannot put the test in the worktree: {e}{left}"))
            }
        }
    }

    // The file's path, then the path of each folder made, a line each.
    fn write_record(&self) -> Result<(), String> {
        let (Some(record), Some(file)) = (&self.record, &self.file) else {
            return Ok(());
        };
        let (Some(folder), Some(name)) = (record.parent(), record.file_name()) else {
            return Err("the record has no place".to_owned());
        };
        let paths = std::iter::once(file).chain(&self.made);
        let text = paths
            .map(|p| format!("{}\n", p.display()))
            .collect::<String>();
        worker_folder::write(folder, Path::new(name), &text).map_err(|e| e.to_string())
    }

    // Removes the file, then the folders made for it, then the record, and
    // names each that stays: a folder the run put something in is still the
    // worktree's mess.
    fn take_out(self) -> Vec<String> {
        let mut left = Vec::new();
        let mut gone = |shown: &Path, result: io::Result<()>| match result {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                left.push(format!("{}: {e}", shown.display()));
            }
            _ => {}
        };
        if let Some(rel) = &self.file {
            let file = self.root.join(rel);
            gone(rel, std::fs::remove_file(&file));
        }
        for rel in self.made.iter().rev() {
            let dir = self.root.join(rel);
            gone(rel, std::fs::remove_dir(&dir));
        }
        if let Some(record) = &self.record {
            gone(record, std::fs::remove_file(record));
        }
        left
    }
}

#[cfg(test)]
mod tests;
