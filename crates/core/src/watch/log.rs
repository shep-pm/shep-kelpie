//! The runner's steps file: one step as a JSON object per line, with the time it was logged

use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::{Map, Value};

use super::Scope;
use crate::ticket::TicketId;

/// One step the runner logged
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Step {
    pub at: Timestamp,
    fields: Map<String, Value>,
}

/// The step the runner logs as a session's call starts
const STARTED: &str = "session-started";

/// A step the project manager's session is named in
const PM_STEPS: &str = "pm-";

// The ticket a logged step names. A step written before ticket ids held a
// bare number, which reads as a GitHub ticket.
fn named(fields: &Map<String, Value>) -> Option<TicketId> {
    fields
        .get("issue")
        .and_then(|issue| serde_json::from_value::<TicketId>(issue.clone()).ok())
}

/// The line of the steps file `line` as a step, or `None` for a line that is not one
pub(super) fn parse(line: &str) -> Option<Step> {
    let Value::Object(mut fields) = serde_json::from_str(line.trim()).ok()? else {
        return None;
    };
    let at = fields.remove("at")?.as_str()?.parse::<Timestamp>().ok()?;
    fields.get("step")?.as_str()?;
    Some(Step { at, fields })
}

/// Which program wrote a session's transcript, and so how it reads
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Claude Code's
    Claude,
    /// Codex's
    Codex,
    /// pi's
    Pi,
    /// goose's
    Goose,
}

/// A session a call started in, and where its transcript is
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Started {
    pub at: Timestamp,
    pub who: String,
    pub format: Format,
    pub session: String,
    pub folder: PathBuf,
    pub threads: Option<PathBuf>,
    issue: Option<TicketId>,
    pm: bool,
}

impl Started {
    /// Whether `scope` shows this session
    pub fn in_scope(&self, scope: Scope) -> bool {
        match scope {
            Scope::Project => true,
            Scope::Issue(issue) => self.issue == Some(issue),
            Scope::Pm => self.pm,
        }
    }
}

impl Step {
    fn name(&self) -> &str {
        self.fields["step"].as_str().unwrap_or_default()
    }

    /// Whether this step names `ticket`
    pub fn names(&self, ticket: TicketId) -> bool {
        named(&self.fields) == Some(ticket)
    }

    /// Whether `scope` shows this step
    pub fn in_scope(&self, scope: Scope) -> bool {
        match scope {
            Scope::Project => true,
            Scope::Issue(ticket) => self.names(ticket),
            Scope::Pm => {
                self.name().starts_with(PM_STEPS)
                    || (self.name() == STARTED && self.fields.get("role") == Some(&"pm".into()))
            }
        }
    }

    /// The session this step starts a call in, if it is `session-started`
    /// and names a transcript that can be read
    pub fn started(&self) -> Option<Started> {
        if self.name() != STARTED {
            return None;
        }
        let text = |key: &str| self.fields.get(key)?.as_str();
        let session = text("session")?;
        let plain = !session.is_empty()
            && session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let format = match text("harness")? {
            "claude-code" => Format::Claude,
            "codex" => Format::Codex,
            "pi" => Format::Pi,
            "goose" => Format::Goose,
            _ => return None,
        };
        let transcript = self.fields.get("transcript")?;
        let folder = PathBuf::from(transcript.get("folder")?.as_str()?);
        let threads = transcript
            .get("threads")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let role = text("role")?;
        let issue = named(&self.fields);
        let agent = text("agent").filter(|agent| !agent.is_empty());
        let who = match (role, issue) {
            ("pm", _) | (_, None) => role.to_owned(),
            (role, Some(ticket)) => format!("{} {role}", ticket.shown()),
        };
        let who = match agent {
            Some(agent) if role != "pm" => format!("{who} ({agent})"),
            _ => who,
        };
        plain.then_some(Started {
            at: self.at,
            who,
            format,
            session: session.to_owned(),
            folder,
            threads,
            issue,
            pm: role == "pm",
        })
    }

    /// The step as one line of text: its name, its work item, and its fields
    ///
    /// Clipped, it leaves out what a reader of the stream has no use for:
    /// where a transcript is kept.
    pub fn text(&self, full: bool) -> String {
        let mut parts = vec![self.name().to_owned()];
        if let Some(ticket) = named(&self.fields) {
            parts.push(ticket.shown());
        }
        for (key, value) in &self.fields {
            let shown = match value {
                _ if matches!(key.as_str(), "step" | "issue") => continue,
                _ if key == "transcript" && !full => continue,
                Value::Null => continue,
                Value::String(text) => text.clone(),
                Value::Array(items) if items.is_empty() => continue,
                Value::Object(fields) if fields.is_empty() => continue,
                other => other.to_string(),
            };
            parts.push(format!("{key}={shown}"));
        }
        parts.join(" ")
    }
}
