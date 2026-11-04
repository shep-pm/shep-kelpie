//! What pi reports of its context and its compactions, and the call that
//! compacts a session
//!
//! `-p` does not run pi's built-in `/compact`: the text would go to the model.
//! A call whose prompt is `COMPACT` sends the command of an extension kelpie
//! writes, which compacts and returns when the compaction has ended. pi
//! prints a `compaction_end` event for it, and for each of its own threshold
//! compactions in a turn.

use std::num::NonZeroU32;
use std::path::Path;
use std::process::Output;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{PI, failed};
use crate::adapters::harness::{StepSkill, step_skill, tail};
use crate::ports::{AgentCall, AgentError, AgentReply, SessionId, Usage};

/// What a compact call sends in place of `COMPACT`
pub(super) const COMMAND: &str = "/kelpie-compact";

/// The extension that registers `COMMAND` and hands a turn's prompt back, with
/// `__PROMPT_FILE__` to fill in
const EXTENSION: &str = include_str!("compact.ts");

/// The tokens pi keeps free of the context window unless told otherwise
const DEFAULT_RESERVE: u32 = 16_384;

/// The extension's source, handing back the prompt in `prompt_file` after a
/// compaction pi starts itself
///
/// A call with no file only registers `COMMAND`.
pub(super) fn extension(prompt_file: Option<&Path>) -> String {
    let file = prompt_file.map(|p| p.to_string_lossy().into_owned());
    EXTENSION.replace("__PROMPT_FILE__", &json!(file).to_string())
}

/// pi's `settings.json` for a call bounded to `window` tokens of a `context`-token model
///
/// pi compacts when the context passes the window less `reserveTokens`, so the
/// reserve is the difference. A window at or past the context, or within pi's
/// default reserve of it, is later than pi compacts already, and gets no file.
pub(super) fn settings(context: u32, window: NonZeroU32) -> Option<Value> {
    let reserve = context.checked_sub(window.get())?;
    (reserve > DEFAULT_RESERVE).then(|| json!({ "compaction": { "reserveTokens": reserve } }))
}

/// What a compaction pi starts itself hands back: the turn's prompt again
///
/// A step's prompt is the text of its skill, as pi expands the command
/// `argv` gives it, since pi expands no command inside a message.
pub(super) fn handed_back(call: &AgentCall) -> String {
    let prompt = match step_skill(call) {
        Some(step) => skill_text(&step),
        None => call.prompt.clone(),
    };
    format!(
        "pi compacted your session partway through this turn, so what came before is \
         a summary, and the prompt may have been cut from it. This is the prompt again:\n\n\
         {prompt}\n"
    )
}

// The step's skill without its front matter, then the rest of the prompt.
fn skill_text(step: &StepSkill) -> String {
    let file = step.dir.join("SKILL.md");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let body = match text
        .strip_prefix("---\n")
        .and_then(|t| t.split_once("\n---\n"))
    {
        Some((_, body)) => body,
        None => &text,
    };
    format!(
        "<skill name=\"{}\" location=\"{}\">\n{}\n</skill>\n\n{}",
        step.name,
        file.display(),
        body.trim(),
        step.rest
    )
}

/// The tokens of one request, as pi counts them
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Tokens {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Tokens {
    pub(super) fn usage(&self) -> Usage {
        Usage {
            input: self.input,
            cache_write: self.cache_write,
            cache_write_5m: 0,
            cache_read: self.cache_read,
            output: self.output,
        }
    }

    // What the request's prompt held, as Claude Code's last iteration is read.
    pub(super) fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct End {
    #[serde(default)]
    aborted: bool,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    result: Option<Summary>,
}

#[derive(Deserialize)]
struct Summary {
    #[serde(default)]
    usage: Option<Tokens>,
}

impl End {
    // The summary of a compaction that made one.
    fn summary(&self) -> Option<&Summary> {
        self.result.as_ref().filter(|_| !self.aborted)
    }

    fn usage(&self) -> Usage {
        let usage = self.summary().and_then(|s| s.usage.as_ref());
        usage.map(Tokens::usage).unwrap_or_default()
    }
}

/// What the `compaction_end` event in `line` used, where the compaction made a summary
pub(super) fn usage_of(line: &str) -> Usage {
    // Read apart from `End`, so a field of the event that is not as expected
    // does not lose the tokens beside it.
    let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
        return Usage::default();
    };
    if event["aborted"].as_bool().unwrap_or(false) {
        return Usage::default();
    }
    (event.pointer("/result/usage").cloned())
        .and_then(|tokens| serde_json::from_value::<Tokens>(tokens).ok())
        .map_or_else(Usage::default, |tokens| tokens.usage())
}

/// Whether the `compaction_end` event in `line` made a summary, so the session did compact
pub(super) fn made_summary(line: &str) -> bool {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    !event["aborted"].as_bool().unwrap_or(false) && event["result"].is_object()
}

/// A compact call's reply: the compaction's usage and no text
///
/// The exit status and an unreachable model are read as for any pi call.
pub(super) fn parse(output: &Output, asked: &SessionId) -> Result<AgentReply, AgentError> {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        id: Option<String>,
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut session = None;
    let mut ended = None;
    for text in stdout.lines().filter(|l| l.starts_with('{')) {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            continue;
        };
        match line.kind.as_str() {
            "session" => session = line.id,
            "compaction_end" => ended = serde_json::from_str::<End>(text).ok(),
            _ => {}
        }
    }
    if !output.status.success() {
        let why = ended.and_then(|end| end.error_message);
        return Err(failed(why.as_deref().unwrap_or(tail(&stderr))));
    }
    let Some(end) = ended else {
        return Err(AgentError::Unreadable(PI, tail(&stdout).to_owned()));
    };
    if let Some(session) = session.filter(|s| *s != asked.0) {
        return Err(AgentError::Unreadable(
            PI,
            format!("pi answered in session {session}, not {}", asked.0),
        ));
    }
    if end.summary().is_none() {
        let none = match end.aborted {
            true => "pi aborted the compaction",
            false => "pi ended the compaction with no summary",
        };
        return Err(failed(end.error_message.as_deref().unwrap_or(none)));
    }
    Ok(AgentReply {
        session_id: asked.clone(),
        text: String::new(),
        usage: end.usage(),
        session_cost: None,
        context: None,
        compactions: 0,
        metered: None,
    })
}
