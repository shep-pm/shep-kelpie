//! A local round kept after its work item ends
//!
//! The round's files are copied out of the build folder, which goes with the
//! work item, into `<folder>/<issue>-r<round>-<UTC time>/`, beside a
//! `round.json` that says what the round was and how it ended. Rounds older
//! than `keep_days` are removed from `folder`, and nothing else in it.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Runner;
use crate::ports::{RoundEnd, Timestamp};
use crate::settings::{AgentName, LocalRound, LocalRounds};
use crate::ticket::TicketId;

const DAY: u64 = 86_400;

/// A kept round's file name time, such as `20260921T141320`, before its `Z`
const STAMP: &str = "%Y%m%dT%H%M%S";

/// What a local round came to, from its reviewer's side
pub(in crate::runner) struct Finished<'a> {
    pub reviewer: &'a AgentName,
    pub round: u32,
    pub ended: RoundEnd,
    pub reason: Option<String>,
    /// The findings that go on to the worker
    pub findings: usize,
    pub started: Timestamp,
    pub finished: Timestamp,
    /// The pushed head the round read, where it recorded one
    pub head: Option<&'a str>,
}

// `round.json`, which a replay reads
#[derive(Serialize)]
struct Record<'a> {
    project: &'a str,
    issue: TicketId,
    pull_request: u64,
    round: u32,
    reviewer: &'a str,
    harness: &'static str,
    model: Option<&'a str>,
    max_tokens: Option<u32>,
    base: String,
    head: String,
    started: String,
    ended: String,
    seconds: u64,
    outcome: RoundEnd,
    reason: Option<String>,
    findings: usize,
}

impl Runner {
    /// Where kept rounds go: the project's `local_rounds.folder`, or
    /// `local-rounds` in its folder
    pub(in crate::runner) fn rounds_folder(&self) -> PathBuf {
        let rounds = &self.settings.local_rounds;
        match &rounds.folder {
            Some(folder) => folder.as_path().to_owned(),
            None => self.paths.folder.join("local-rounds"),
        }
    }

    /// Copies the round that just ended, then removes the kept rounds past
    /// their days
    ///
    /// A round that cannot be kept is a note, never a failed review.
    pub(in crate::runner) fn keep_round(&mut self, finished: &Finished<'_>) {
        let Some(item) = self.current() else {
            return;
        };
        let Some(local) = self
            .listed(finished.reviewer)
            .and_then(|listed| listed.runs.local())
        else {
            return;
        };
        let folder = self.rounds_folder();
        let (model, max_tokens, harness) = match &local {
            LocalRound::Endpoint(e) => (Some(e.model.as_str()), Some(e.reply_tokens()), "endpoint"),
            LocalRound::Command(_) => (None, None, "command"),
        };
        let (repo, worktree) = (&self.settings.git.checkout, &item.worktree);
        let base = item.review_base(self.item_base(item));
        let base = crate::worktree::change_start(repo, worktree, &base).unwrap_or(base);
        let head = match finished.head {
            Some(head) => head.to_owned(),
            None => crate::worktree::head(repo, worktree).unwrap_or_default(),
        };
        let project = self.paths.name();
        let issue = item.issue;
        let record = Record {
            project: &project,
            issue,
            pull_request: item.pull_request.unwrap_or_default(),
            round: finished.round,
            reviewer: finished.reviewer.as_str(),
            harness,
            model,
            max_tokens,
            base,
            head,
            started: utc(finished.started).to_string(),
            ended: utc(finished.finished).to_string(),
            seconds: finished.finished.0.saturating_sub(finished.started.0),
            outcome: finished.ended,
            reason: finished.reason.clone(),
            findings: finished.findings,
        };
        let out = item.build.join("qwen-review");
        let kept = keep(&folder, &out, &record, issue, finished.finished);
        let pruned = prune(
            &folder,
            &project,
            &self.settings.local_rounds,
            finished.finished,
        );
        if let Err(e) = kept {
            self.notes.push(format!(
                "cannot keep local round {} of #{issue}: {e}",
                finished.round
            ));
        }
        if let Err(e) = pruned {
            self.notes.push(format!(
                "cannot remove old local rounds from {}: {e}",
                folder.display()
            ));
        }
    }
}

/// Removes the kept rounds past their days from the project's folder when the
/// runner starts, as a note where that fails
pub(in crate::runner) fn prune_at_start(
    rounds: &LocalRounds,
    project: &str,
    project_folder: &Path,
    now: Timestamp,
) -> Option<String> {
    let folder = match &rounds.folder {
        Some(folder) => folder.as_path().to_owned(),
        None => project_folder.join("local-rounds"),
    };
    prune(&folder, project, rounds, now).err().map(|e| {
        format!(
            "cannot remove old local rounds from {}: {e}",
            folder.display()
        )
    })
}

fn utc(at: Timestamp) -> jiff::Timestamp {
    let seconds = i64::try_from(at.0).unwrap_or(i64::MAX);
    jiff::Timestamp::from_second(seconds).unwrap_or(jiff::Timestamp::MAX)
}

// Copies the round's files from `out`, then writes its record beside them.
fn keep(
    folder: &Path,
    out: &Path,
    record: &Record<'_>,
    issue: TicketId,
    at: Timestamp,
) -> io::Result<()> {
    let stamp = format!("{}Z", utc(at).strftime(STAMP));
    let name = format!("{issue}-r{}-{stamp}", record.round);
    std::fs::create_dir_all(folder)?;
    let dest = (1..)
        .map(|n| match n {
            1 => folder.join(&name),
            n => folder.join(format!("{name}-{n}")),
        })
        .find_map(|path| match std::fs::create_dir(&path) {
            Ok(()) => Some(Ok(path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
            Err(e) => Some(Err(e)),
        })
        .expect("an unbounded range of names")?;
    let filled = (|| {
        if out.is_dir() {
            copy(out, &dest, Path::new(""), record.round)?;
        }
        let json = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
        std::fs::write(dest.join("round.json"), json + "\n")
    })();
    if filled.is_err() {
        // Without its `round.json` no pruning would ever know it.
        let _ = std::fs::remove_dir_all(&dest);
    }
    filled
}

// Copies what round `round` wrote under `from` into `to`. Symlinks are left,
// since a command's files are not trusted to point only inside the folder.
fn copy(from: &Path, to: &Path, relative: &Path, round: u32) -> io::Result<()> {
    for entry in std::fs::read_dir(from.join(relative))? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        if !wanted(&relative, round) {
            continue;
        }
        let target = to.join(&relative);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy(from, to, &relative, round)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

// Whether `relative`, a path under the build folder's `qwen-review`, is a file
// round `round` wrote: its own reply folder and report, its hunks, and the
// criteria a command was given. Anything else there is not the round's.
fn wanted(relative: &Path, round: u32) -> bool {
    let mut parts = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy());
    let top = parts.next().unwrap_or_default();
    if let Some(rest) = top.strip_prefix("round-") {
        let number = rest.split('.').next().unwrap_or(rest);
        return number.parse() == Ok(round);
    }
    match (top.as_ref(), parts.next()) {
        ("criteria.md", None) | ("hunks", None) => true,
        ("hunks", Some(child)) => {
            child == round.to_string() || child.starts_with(&format!("{round}-out-"))
        }
        _ => false,
    }
}

// Removes `project`'s kept rounds older than `rounds.keep_days` from `folder`.
// An entry not named as a kept round, or whose `round.json` names another
// project (several may share a folder), is not one of its own, and stays. A
// round that cannot be removed does not hold back the rest.
fn prune(folder: &Path, project: &str, rounds: &LocalRounds, now: Timestamp) -> io::Result<()> {
    let days = u64::from(rounds.keep_days.get());
    if days == 0 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let cutoff = now.0.saturating_sub(days * DAY);
    let mut failed = Vec::new();
    for entry in entries {
        let removed = entry.and_then(|entry| {
            let old = entry.file_type()?.is_dir()
                && kept_at(&entry.file_name().to_string_lossy()).is_some_and(|at| at < cutoff)
                && is_of(&entry.path(), project);
            if old {
                std::fs::remove_dir_all(entry.path()).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("{}: {e}", entry.file_name().to_string_lossy()),
                    )
                })?;
            }
            Ok(())
        });
        failed.extend(removed.err());
    }
    match failed.as_slice() {
        [] => Ok(()),
        [only] => Err(io::Error::new(only.kind(), only.to_string())),
        [first, ..] => Err(io::Error::new(
            first.kind(),
            format!("{} kept rounds, such as {first}", failed.len()),
        )),
    }
}

// Whether the kept round in `dir` says it is `project`'s
fn is_of(dir: &Path, project: &str) -> bool {
    std::fs::read_to_string(dir.join("round.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|record| record["project"] == project)
}

// When a kept round named `<issue>-r<round>-<UTC time>`, with a `-<n>` where
// two ended in a second, ended.
fn kept_at(name: &str) -> Option<u64> {
    let mut parts = name.splitn(3, '-');
    let (issue, round, rest) = (parts.next()?, parts.next()?, parts.next()?);
    issue.parse::<u64>().ok()?;
    round.strip_prefix('r')?.parse::<u32>().ok()?;
    let stamp = rest.split('-').next()?.strip_suffix('Z')?;
    let time = jiff::fmt::strtime::parse(STAMP, stamp).ok()?;
    let at = time
        .to_datetime()
        .ok()?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?;
    u64::try_from(at.timestamp().as_second()).ok()
}
