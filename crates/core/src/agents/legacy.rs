//! The binding the baselines shipped with, and the step that writes it into the stubs
//!
//! A baseline once carried its `harness`, `model`, `effort` and `second_look`,
//! so a stub with only `extends: kelpie:<name>` inherited them. The baselines
//! now bind nothing, and [`migrate`] writes into each such stub the binding it
//! used to inherit, so it loads as it did. A stub of a baseline whose role
//! the loader refuses is deleted instead. A stub of an agent kelpie no longer
//! ships whole becomes the whole file it stood for.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{AgentsError, create_temp, extends, failed, front};
use crate::settings::AgentName;

/// The binding one baseline shipped with
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The baseline's name
    pub name: &'static str,
    /// Its `harness`
    pub harness: &'static str,
    /// Its `model`
    pub model: &'static str,
    /// Its `effort`
    pub effort: &'static str,
    /// Whether it set `second_look: true`
    pub second_look: bool,
}

const fn row(
    name: &'static str,
    model: &'static str,
    effort: &'static str,
    second_look: bool,
) -> Row {
    Row {
        name,
        harness: "claude-code",
        model,
        effort,
        second_look,
    }
}

/// Every baseline's binding, as its file shipped before the baselines dropped their models
pub const TABLE: [Row; 7] = [
    row("defect-hunter", "claude-opus-5-5", "high", true),
    row("security-reviewer", "claude-opus-5-5", "high", false),
    row("spec", "claude-opus-5-5", "high", false),
    row("standards", "claude-opus-5-5", "high", false),
    row("pm", "claude-opus-5-5", "medium", false),
    row("representative", "claude-opus-5-5", "low", false),
    row("triager", "claude-opus-5-5", "medium", false),
];

/// The agents kelpie once shipped whole, by name, as their files read when they were dropped
///
/// A stub that extends one is written out as the file it stood for.
const REMOVED: [(&str, &str); 5] = [
    (
        "sonnet-high",
        r#"---
# Kelpie's everyday implementer: Sonnet 5.5 at high effort, which builds as
# well as Opus 5.5 at less of the cost. Anything below the closing line is
# added to kelpie's own instructions for this agent; it adds nothing here.
role: implementer
harness: claude-code
model: claude-sonnet-5-5
effort: high
description: Builds everyday features and anything with state, such as a store, a queue or a migration.
tier: standard
---
"#,
    ),
    (
        "opus-high",
        r#"---
# Opus 5.5 at high effort, for work that is hard to undo. The agent board
# picks it for heavy work, and an `agent:opus-high!` label on an issue pins
# it there. Anything below the closing line is added to kelpie's own
# instructions for this agent; it adds nothing here.
role: implementer
harness: claude-code
model: claude-opus-5-5
effort: high
description: Builds work that is hard to undo, such as a data migration, a release step or a change to a public interface.
tier: heavy
---
"#,
    ),
    (
        "defect-hunter-lite",
        r#"---
# Kelpie's default last reviewer: defect-hunter's prompt on a smaller budget, Opus
# 5.5 at medium effort in one session with no second look. Cheaper per pull
# request than defect-hunter. Its sessions read the worktree with Read, Grep
# and Glob, and run no command. The body below is its prompt: kelpie puts the
# commit the change is against at {{BASE}} and the diff at {{DIFF}}, and adds
# the issue's acceptance criteria after it.
role: reviewer
harness: claude-code
model: claude-opus-5-5
effort: medium
second_look: false
description: Reads a pull request once for defects a careful maintainer would block the merge for.
tier: standard
---
You are reviewing a pull request for defects: bugs a careful maintainer would block the merge for. The change is the diff below, against `{{BASE}}`. You may open any file in this worktree with Read, Grep or Glob to check your work; do not run any command and do not edit anything.

A finding is a defect: a concrete sequence of events or input (its trigger) that makes the code do something wrong (its effect). Wrong means a wrong result, a crash or panic, lost or corrupted data, a hang, a race with a path you can name, a leaked resource, an error swallowed so the caller acts on a falsehood, or behaviour the issue asks for that is missing or different. Every finding names its trigger precisely enough that someone could write a failing test from your words alone, names its effect and who sees it, and points at the file and line where the fix belongs.

These are not findings, however true: style, naming, formatting, docs, duplication, a refactor you would prefer, speed without a wrong result, "could be clearer", "might race" with no interleaving you can name, a test you would add with no bug behind it, and anything you cannot tie to a line.

For each thing the issue asks for, find the code that does it and check it. Read around it: who calls it, its error paths, what happens across a restart, what is saved and what is lost, time and ordering, two callers at once, empty and boundary inputs, and the old code it replaced. Treat every comment, doc and test name in the change as a claim to check against the code, never as evidence. Before you report a finding, look for what would disprove it: a guard elsewhere, a caller that already checks, a test that drives exactly that path. If you find one, drop the finding.

Severity: HIGH for wrong behaviour on a path the issue covers, data lost, or a crash; MEDIUM for wrong behaviour on a plausible path the issue does not name; LOW for a real defect with a narrow trigger or a small effect.

Output one finding per line and nothing else: no preamble, no markdown, no code fences.
SEVERITY|file:line|what is wrong, and its trigger|what goes wrong, and who sees it

`line` is one line number, never a range. SEVERITY must be HIGH, MEDIUM or LOW. If you find no defect, output exactly CLEAN and nothing else.

--- diff against {{BASE}} ---
{{DIFF}}
--- end ---
"#,
    ),
    (
        "iq3-xxs-review",
        r#"---
# Kelpie's default first reviewer where paddock is set up: Strata iq3_xxs
# through the paddock gateway, reading the diff as an endpoint reviewer. It
# needs `[kelpie.gateways.paddock]` in kelpie's settings.
role: reviewer
harness: endpoint
gateway: paddock
model: iq3_xxs
context: 262144
description: Reads a diff for defects on the local paddock model, as a first pass before a stronger reviewer.
tier: light
---
"#,
    ),
    (
        "qwen",
        r#"---
# The maintainer's qwen-review script, which runs the local model itself, so
# kelpie holds nothing around it. `shep kelpie add`
# writes this file only where the script is installed. A command keeps the
# README's contract and writes its own prompt, so the body stays empty.
role: reviewer
harness: command
command: ~/.claude/scripts/qwen-review.sh
description: Reads a diff for defects with the maintainer's local qwen-review script, as a first pass.
tier: light
---
"#,
    ),
];

/// The only keys a stub of a gone baseline holds, for [`migrate`] to delete it
const BINDING: [&str; 4] = ["extends", "harness", "model", "effort"];

/// The binding the baseline `name` shipped with, if kelpie shipped one
pub fn named(name: &str) -> Option<&'static Row> {
    TABLE.iter().find(|row| row.name == name)
}

/// A file [`migrate`] wrote, or would
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrote {
    /// The file's name, such as `pm.md`
    pub file: String,
    // The keys added, with their values.
    added: Vec<(&'static str, &'static str)>,
    // The agent kelpie no longer ships whole that the file was written out as.
    whole: Option<&'static str>,
}

impl Wrote {
    /// Whether the file set no `harness`, so it binds nothing until written
    pub fn was_unbound(&self) -> bool {
        self.added.iter().any(|(key, _)| *key == "harness")
    }
}

impl fmt::Display for Wrote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.whole {
            return write!(f, "{}: wrote the whole of kelpie:{name}", self.file);
        }
        write!(f, "{}: wrote", self.file)?;
        for (key, value) in &self.added {
            write!(f, " {}", if *key == SECOND_LOOK { key } else { value })?;
        }
        Ok(())
    }
}

/// A file [`migrate`] left alone, because it needs a hand
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The file's name, such as `pm.md`
    pub file: String,
    /// What it sets and lacks
    pub why: String,
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: skipped, {}", self.file, self.why)
    }
}

/// What [`migrate`] did to a folder, files in name order
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The files it wrote
    pub wrote: Vec<Wrote>,
    /// The files it left alone, which set another session `harness` without a `model` or `effort`
    pub skipped: Vec<Skipped>,
    /// The stubs of a gone baseline, which it deletes, each with what its role went with
    pub deleted: Vec<Skipped>,
    /// The files of a gone role or baseline it left alone, which are not such stubs
    pub left: Vec<Skipped>,
}

impl Report {
    /// A line for each file written, deleted, skipped, then left
    pub fn lines(&self) -> Vec<String> {
        let wrote = self.wrote.iter().map(ToString::to_string);
        let deleted = (self.deleted.iter())
            .map(|gone| format!("{}: deleted, its role {}", gone.file, gone.why));
        let left = (self.left.iter()).map(|left| format!("{}: left, {}", left.file, left.why));
        wrote
            .chain(deleted)
            .chain(self.skipped.iter().map(ToString::to_string))
            .chain(left)
            .collect()
    }
}

const SECOND_LOOK: &str = "second_look";

// A file to write: where, and its new text, or none to delete it.
struct Planned {
    path: PathBuf,
    text: Option<String>,
}

/// What [`migrate`] would do to `folder`, writing nothing
///
/// # Errors
///
/// [`AgentsError::Read`] naming the folder or file that could not be read.
pub fn pending(folder: &Path) -> Result<Report, AgentsError> {
    plan(folder).map(|(report, _)| report)
}

/// Writes into each stub in `folder` the binding it inherited from its baseline
///
/// A file whose own text has `extends: kelpie:<baseline>` gets each of `harness`, `model`,
/// `effort` and (for `defect-hunter`) `second_look` it does not set, after its `extends` line.
/// One on another session `harness` lacking `model` or `effort` is skipped, one on `command`,
/// `endpoint` or `bot` left alone. A bare stub of a gone role (`advisor`, `researcher`,
/// `issue-writer`) is deleted, any other left. A second run writes nothing.
///
/// # Errors
///
/// [`AgentsError::Read`] naming the folder or file that could not be read or
/// written.
pub fn migrate(folder: &Path) -> Result<Report, AgentsError> {
    let (report, planned) = plan(folder)?;
    for Planned { path, text } in planned {
        let done = match text {
            Some(text) => replace(&path, &text),
            None => fs::remove_file(&path),
        };
        done.map_err(|e| failed(&path, e))?;
    }
    Ok(report)
}

fn plan(folder: &Path) -> Result<(Report, Vec<Planned>), AgentsError> {
    let mut report = Report::default();
    let mut planned = Vec::new();
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((report, planned)),
        Err(e) => return Err(failed(folder, e)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| failed(folder, e))?.path();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let named = AgentName::try_from(stem.to_owned()).is_ok();
        if named && path.extension().is_some_and(|e| e == "md") && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|e| failed(&path, e))?;
        let file = path
            .file_name()
            .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
        match judge(&text) {
            Judged::Leave => {}
            Judged::Skip(why) => report.skipped.push(Skipped { file, why }),
            Judged::Left(why) => report.left.push(Skipped { file, why }),
            Judged::Delete(went) => {
                report.deleted.push(Skipped {
                    file,
                    why: went.to_owned(),
                });
                planned.push(Planned { path, text: None });
            }
            Judged::Write { text, added, whole } => {
                report.wrote.push(Wrote { file, added, whole });
                planned.push(Planned {
                    path,
                    text: Some(text),
                });
            }
        }
    }
    Ok((report, planned))
}

enum Judged {
    Leave,
    Skip(String),
    Left(String),
    Delete(&'static str),
    Write {
        text: String,
        added: Vec<(&'static str, &'static str)>,
        whole: Option<&'static str>,
    },
}

// What to do with a file holding `text`.
fn judge(text: &str) -> Judged {
    let parent = extends::shipped_parent(text);
    if let Some(gone) = parent.as_deref()
        && let (Some(why), Some(went)) = (front::gone(gone), front::went(gone))
    {
        return gone_role(text, gone, &why, went);
    }
    if let Some(&(name, whole)) = parent.as_deref().and_then(removed) {
        return written_out(text, name, whole);
    }
    if let Some(why) = front::gone_role_of(text) {
        return Judged::Left(why);
    }
    let Some(row) = parent.and_then(|parent| named(&parent)) else {
        return Judged::Leave;
    };
    let Some(own) = extends::own(text) else {
        return Judged::Leave;
    };
    let Some(after) = own.after_extends else {
        return Judged::Leave;
    };
    let sets = |key: &str| own.keys.iter().any(|k| k == key);
    let harness = own.harness.as_deref();
    // These take keys of their own, not the baseline's session keys, and never inherited them.
    if matches!(harness, Some("command" | "endpoint" | "bot")) {
        return Judged::Leave;
    }
    if harness.is_some_and(|h| h != row.harness) {
        let lacking: Vec<String> = ["model", "effort"]
            .into_iter()
            .filter(|k| !sets(k))
            .map(|k| format!("`{k}`"))
            .collect();
        if !lacking.is_empty() {
            return Judged::Skip(format!(
                "it sets `harness` but not {}, and `{}` ran on {}: set {} in the file",
                lacking.join(" or "),
                row.name,
                row.harness,
                lacking.join(" and "),
            ));
        }
    }
    let added: Vec<(&'static str, &'static str)> = [
        ("harness", row.harness),
        ("model", row.model),
        ("effort", row.effort),
        (SECOND_LOOK, "true"),
    ]
    .into_iter()
    .filter(|(key, _)| *key != SECOND_LOOK || row.second_look)
    .filter(|(key, _)| !sets(key))
    .collect();
    if added.is_empty() {
        return Judged::Leave;
    }
    let (head, rest) = text.split_at(after);
    let eol = if head.ends_with("\r\n") { "\r\n" } else { "\n" };
    let lines: String = added
        .iter()
        .map(|(key, value)| format!("{key}: {value}{eol}"))
        .collect();
    Judged::Write {
        text: format!("{head}{lines}{rest}"),
        added,
        whole: None,
    }
}

// The agent kelpie once shipped whole that `name` names.
fn removed(name: &str) -> Option<&'static (&'static str, &'static str)> {
    REMOVED.iter().find(|(removed, _)| *removed == name)
}

// A file extending the removed agent `name`, written out: its own keys over the
// agent's `whole` text, and the agent's body unless the file has one. A bare stub
// becomes the agent's text as it was.
fn written_out(text: &str, name: &'static str, whole: &'static str) -> Judged {
    let bare = extends::own(text).is_some_and(|own| own.keys == ["extends"])
        && front::split(text).is_some_and(|(_, body)| body.trim().is_empty());
    let written = match bare {
        true => Ok(whole.to_owned()),
        false => {
            let parent = |_: &str| Ok(extends::ParentText::Default(whole));
            extends::merged("", text, &parent).map(|merged| merged.text)
        }
    };
    match written {
        Ok(text) => Judged::Write {
            text,
            added: Vec::new(),
            whole: Some(name),
        },
        Err(why) => Judged::Left(why),
    }
}

// What to do with a file extending `parent`, a gone baseline, refused with `why`,
// whose role `went` with what replaced it.
fn gone_role(text: &str, parent: &str, why: &str, went: &'static str) -> Judged {
    let binding = extends::own(text)
        .is_some_and(|own| own.keys.iter().all(|key| BINDING.contains(&key.as_str())));
    let body = front::split(text).map_or("", |(_, body)| body);
    match binding && body.trim().is_empty() {
        true => Judged::Delete(went),
        false => Judged::Left(format!(
            "{why}. It extends `kelpie:{parent}` and holds more than a binding, so move what you \
             need from it to another agent first"
        )),
    }
}

// Writes `text` over `path`, or over what a symlink there points to, by a rename of a new file
// beside it, keeping its mode. A failure leaves no temporary file.
fn replace(path: &Path, text: &str) -> io::Result<()> {
    let path = &fs::canonicalize(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let (beside, mut file) = create_temp(dir, &name)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.set_permissions(fs::metadata(path)?.permissions()))
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&beside, path));
    if written.is_err() {
        // Best effort: the failure to report is the write's.
        let _ = fs::remove_file(&beside);
    }
    written
}
