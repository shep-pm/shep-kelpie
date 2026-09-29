//! Where Ollama's loaded model sits, from `GET /api/ps`
//!
//! Before a round against an Ollama host, the reviewer reads which models
//! are loaded and how much of each is on the GPU. A model partly on the CPU
//! fails the round, since it would run review-sized prompts at a twentieth
//! of its speed. A model that is not loaded yet is not checked: it loads
//! when the round's first request asks for it, and `/api/ps` cannot say
//! where it will go. A server with no `/api/ps`, which is not Ollama, is
//! skipped.

use std::process::{Command, Stdio};

use serde_json::Value;

use super::LocalReviewer;
use super::endpoint::{CHECK_TIMEOUT, CONNECT_TIMEOUT, quote};
use crate::ports::{ModelSeat, ReviewerError};
use crate::settings::LocalRound;

impl LocalReviewer {
    /// Fails with [`ReviewerError::Spilled`] when a model `local` would use
    /// sits partly on the CPU, and records where it sits for `status`
    pub(super) fn check_seat(&self, local: &LocalRound) -> Result<(), ReviewerError> {
        let Some((host, asked)) = local.ollama() else {
            return Ok(());
        };
        let Some(seats) = read_seats(&host)? else {
            *self.seat.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return Ok(());
        };
        let mut used: Vec<ModelSeat> = seats
            .into_iter()
            .filter(|seat| asked.is_none_or(|asked| names(&seat.name, asked)))
            .collect();
        // The worst placement first, so a spilled one is what `status` shows.
        used.sort_by_key(ModelSeat::gpu_percent);
        let reason = used.iter().find_map(ModelSeat::spill);
        *self.seat.lock().unwrap_or_else(|e| e.into_inner()) = used.into_iter().next();
        reason.map_or(Ok(()), |reason| Err(ReviewerError::Spilled(reason)))
    }

    /// Where the model sat when a round last looked
    pub(super) fn last_seat(&self) -> Option<ModelSeat> {
        self.seat.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

// Ollama names a model with its tag, and takes the bare name for `:latest`.
fn names(loaded: &str, asked: &str) -> bool {
    bare(loaded) == bare(asked)
}

fn bare(name: &str) -> &str {
    name.strip_suffix(":latest").unwrap_or(name)
}

// None where the host has no `/api/ps` to read.
fn read_seats(host: &str) -> Result<Option<Vec<ModelSeat>>, ReviewerError> {
    let url = format!("{host}/api/ps");
    let output = Command::new("curl")
        .args(["-sS", "-w", "\n%{http_code}"])
        .args([
            "--connect-timeout",
            CONNECT_TIMEOUT,
            "--max-time",
            CHECK_TIMEOUT,
        ])
        .arg(&url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ReviewerError::Spawn(format!("curl: {e}")))?;
    if !output.status.success() {
        return Err(ReviewerError::Failed(format!(
            "cannot reach {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
    match status.trim() {
        s if s.starts_with('2') => {}
        "404" | "405" | "501" => return Ok(None),
        s => {
            return Err(ReviewerError::Failed(format!(
                "{url} answered HTTP {s}: {}",
                quote(body)
            )));
        }
    }
    // A server that answers any path with a page is not Ollama either.
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    Ok(value["models"]
        .as_array()
        .map(|models| models.iter().filter_map(seat).collect()))
}

fn seat(model: &Value) -> Option<ModelSeat> {
    Some(ModelSeat {
        name: model["name"]
            .as_str()
            .or(model["model"].as_str())?
            .to_owned(),
        size: model["size"].as_u64()?,
        size_vram: model["size_vram"].as_u64().unwrap_or(0),
        context_length: model["context_length"].as_u64(),
        expires_at: model["expires_at"].as_str().map(str::to_owned),
    })
}
