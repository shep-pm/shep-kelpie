//! Kelpie's own local reviewer, over an OpenAI-compatible server
//!
//! It diffs the worktree from the round's base and cuts the diff into
//! chunks that fit the model's context, whole hunks where they fit. Each
//! chunk goes to the server's `/chat/completions` with kelpie's fixed
//! prompt, through `curl`, and each reply is read in the findings format.
//! Requests and replies are kept under the round's folder, beside the
//! `round-N.txt` and its completion marker a command would write.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use super::LocalReviewer;
use crate::adapters::process::RunError;
use crate::ports::{Finding, ReviewerError, parse_findings};
use crate::settings::Endpoint;

/// The review prompt every chunk is sent with
const PROMPT: &str = include_str!("review-prompt.md");

/// Unchanged lines kept around each change
const CONTEXT_LINES: &str = "-U10";

// Code runs three to four bytes a token in common tokenizers. Three
// overcounts, so a chunk errs small rather than overflowing the context.
const BYTES_PER_TOKEN: usize = 3;

/// Tokens a chat request spends on its own framing
const FRAMING_TOKENS: usize = 64;

/// The share of the context kept for the reply, which a thinking model fills
const REPLY_SHARE: usize = 4;

// A local model can queue behind another round for its whole run. This is
// the qwen-review script's own per-call timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1800);

/// Seconds `curl` gets to connect, and to finish the start check
const CONNECT_TIMEOUT: &str = "10";
const CHECK_TIMEOUT: &str = "15";

/// How much of a reply an error quotes
const QUOTE: usize = 300;

impl LocalReviewer {
    /// Asks the server for its models, so an endpoint that cannot answer
    /// stops the runner at start
    pub(super) fn check_endpoint(&self, endpoint: &Endpoint) -> Result<(), String> {
        let url = format!("{}/models", endpoint.url.as_str());
        let output = Command::new("curl")
            .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}"])
            .args([
                "--connect-timeout",
                CONNECT_TIMEOUT,
                "--max-time",
                CHECK_TIMEOUT,
            ])
            .arg(&url)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        let status = String::from_utf8_lossy(&output.stdout);
        match status.trim() {
            s if output.status.success() && s.starts_with('2') => Ok(()),
            _ if !output.status.success() => Err(format!(
                "cannot reach {url}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            s => Err(format!("{url} answered HTTP {s}")),
        }
    }

    /// Reviews the diff from `base` chunk by chunk, and returns every finding
    pub(super) fn endpoint_round(
        &self,
        endpoint: &Endpoint,
        worktree: &Path,
        base: &str,
        out: &Path,
        round: u32,
    ) -> Result<Vec<Finding>, ReviewerError> {
        let diff = diff(worktree, base)?;
        let context = endpoint.context.get() as usize;
        let reply = context / REPLY_SHARE;
        let prompt = PROMPT.len().div_ceil(BYTES_PER_TOKEN) + FRAMING_TOKENS;
        let budget = context.saturating_sub(reply + prompt) * BYTES_PER_TOKEN;
        let folder = out.join(format!("round-{round}"));
        std::fs::create_dir_all(&folder).map_err(|e| failed(&folder, "create", &e))?;
        let mut text = String::new();
        for (index, chunk) in chunks(&parse(&diff), budget).iter().enumerate() {
            let body = json!({
                "model": endpoint.model.as_str(),
                "temperature": 0,
                "stream": false,
                "max_tokens": reply,
                "messages": [
                    { "role": "system", "content": PROMPT },
                    { "role": "user", "content": chunk },
                ],
            });
            let request = folder.join(format!("request-{index}.json"));
            std::fs::write(&request, body.to_string())
                .map_err(|e| failed(&request, "write", &e))?;
            let answer = self.ask(
                endpoint,
                &request,
                &folder.join(format!("reply-{index}.json")),
            )?;
            text.push_str(answer.trim());
            text.push('\n');
        }
        let report = out.join(format!("round-{round}.txt"));
        std::fs::write(&report, &text).map_err(|e| failed(&report, "write", &e))?;
        let done = out.join(format!("round-{round}.txt.done"));
        std::fs::write(&done, "").map_err(|e| failed(&done, "write", &e))?;
        Ok(parse_findings(&text))
    }

    // One chunk's request, answered with the model's words and no thinking.
    fn ask(
        &self,
        endpoint: &Endpoint,
        request: &Path,
        reply: &Path,
    ) -> Result<String, ReviewerError> {
        let url = format!("{}/chat/completions", endpoint.url.as_str());
        // curl leaves the file alone on a reply with no body, and a retried
        // round reuses the name.
        std::fs::remove_file(reply)
            .or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            })
            .map_err(|e| failed(reply, "remove", &e))?;
        let mut command = Command::new("curl");
        command
            .args(["-sS", "-X", "POST", "-w", "%{http_code}"])
            .args(["-H", "Content-Type: application/json", "-H", "Expect:"])
            .args(["--connect-timeout", CONNECT_TIMEOUT])
            .arg("--data-binary")
            .arg(format!("@{}", request.display()))
            .arg("-o")
            .arg(reply)
            .arg(&url);
        let output = self
            .processes
            .output_within(&mut command, REQUEST_TIMEOUT)
            .map_err(|e| match e {
                RunError::Io(e) => ReviewerError::Spawn(format!("curl: {e}")),
                RunError::Stopped => ReviewerError::Stopped,
                RunError::TimedOut => ReviewerError::Failed(format!(
                    "{url} gave no reply in {}s",
                    REQUEST_TIMEOUT.as_secs()
                )),
            })?;
        if !output.status.success() {
            return Err(ReviewerError::Failed(format!(
                "cannot reach {url}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let body = std::fs::read_to_string(reply).unwrap_or_default();
        let status = String::from_utf8_lossy(&output.stdout);
        if !status.trim().starts_with('2') {
            return Err(ReviewerError::Failed(format!(
                "{url} answered HTTP {}: {}",
                status.trim(),
                quote(&body)
            )));
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|_| ReviewerError::Unreadable(quote(&body)))?;
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| ReviewerError::Unreadable(quote(&body)))?;
        // A thinking model's reasoning comes first, and may quote finding-shaped lines.
        Ok(match content.rsplit_once("</think>") {
            Some((_, answer)) => answer.to_owned(),
            None => content.to_owned(),
        })
    }
}

fn failed(path: &Path, verb: &str, e: &std::io::Error) -> ReviewerError {
    ReviewerError::Failed(format!("cannot {verb} {}: {e}", path.display()))
}

fn quote(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(QUOTE) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

fn diff(worktree: &Path, base: &str) -> Result<String, ReviewerError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            CONTEXT_LINES,
            base,
            "--",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ReviewerError::Spawn(e.to_string()))?;
    if !output.status.success() {
        return Err(ReviewerError::Failed(format!(
            "cannot diff {} from {base}: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One file's hunks, each line led by its number in the new file
#[derive(Debug, PartialEq, Eq)]
struct FileDiff {
    path: String,
    hunks: Vec<String>,
}

/// Reads `git diff` output into files of numbered hunks
///
/// A file with no hunks, such as a binary one or a pure rename, is left out.
fn parse(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut old_path = String::new();
    let mut next_line = 0u32;
    // A removed `-- x` reads `--- x`, so file headers count only before a hunk.
    let mut in_header = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files.push(FileDiff {
                path: String::new(),
                hunks: Vec::new(),
            });
            in_header = true;
        } else if let (true, Some(path)) = (in_header, line.strip_prefix("--- ")) {
            old_path = path_of(path, "a/");
        } else if let (true, Some(path)) = (in_header, line.strip_prefix("+++ ")) {
            if let Some(file) = files.last_mut() {
                file.path = match path {
                    "/dev/null" => old_path.clone(),
                    path => path_of(path, "b/"),
                };
            }
        } else if line.starts_with("@@") {
            in_header = false;
            next_line = new_start(line);
            if let Some(file) = files.last_mut() {
                file.hunks.push(format!("{line}\n"));
            }
        } else if let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) {
            let (mark, rest) = line.split_at(line.len().min(1));
            match mark {
                "+" | " " => {
                    let _ = writeln!(hunk, "{next_line:>6} {mark}{rest}");
                    next_line += 1;
                }
                "-" => {
                    let _ = writeln!(hunk, "{:>6} -{rest}", "");
                }
                // `\ No newline at end of file`, and anything else git adds.
                _ => {}
            }
        }
    }
    files.retain(|f| !f.hunks.is_empty() && !f.path.is_empty());
    files
}

fn path_of(path: &str, prefix: &str) -> String {
    let path = path.trim_matches('"');
    path.strip_prefix(prefix).unwrap_or(path).to_owned()
}

// `@@ -12,7 +14,9 @@ fn name`: the new file's hunk starts at line 14.
fn new_start(header: &str) -> u32 {
    header
        .split_whitespace()
        .find_map(|part| part.strip_prefix('+'))
        .and_then(|range| range.split(',').next())
        .and_then(|start| start.parse().ok())
        .unwrap_or(1)
}

/// Packs files' hunks into chunks of at most `budget` bytes where it can
///
/// Each chunk names a file before its first hunk there. A hunk too big for
/// a chunk of its own is cut between lines, each piece under its file's
/// name again. A single line over the budget still goes, alone.
fn chunks(files: &[FileDiff], budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for file in files {
        let heading = format!("### {}\n", file.path);
        let mut named = false;
        for hunk in &file.hunks {
            for piece in pieces(hunk, budget.saturating_sub(heading.len())) {
                let need = piece.len() + if named { 0 } else { heading.len() };
                if !chunk.is_empty() && chunk.len() + need > budget {
                    chunks.push(std::mem::take(&mut chunk));
                    named = false;
                }
                if !named {
                    chunk.push_str(&heading);
                    named = true;
                }
                chunk.push_str(&piece);
            }
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

// A hunk cut between lines into pieces of at most `size` bytes.
fn pieces(hunk: &str, size: usize) -> Vec<String> {
    if hunk.len() <= size {
        return vec![hunk.to_owned()];
    }
    let mut pieces = Vec::new();
    let mut piece = String::new();
    for line in hunk.split_inclusive('\n') {
        if !piece.is_empty() && piece.len() + line.len() > size {
            pieces.push(std::mem::take(&mut piece));
        }
        piece.push_str(line);
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

#[cfg(test)]
mod tests;
