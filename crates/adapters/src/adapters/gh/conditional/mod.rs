//! REST reads that send the last answer's `ETag` and keep the answer a 304 confirms
//!
//! GitHub does not count a 304 against the hourly limit. A read keeps the
//! tag and body of its last answer, keyed by the login it read as and the
//! path, and sends the tag as `If-None-Match` the next time. A restart starts
//! with nothing kept. GraphQL has no conditional requests, so only the REST
//! reads go through here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::de::IgnoredAny;

use super::{failed, gh_command, unreadable};
use crate::ports::{ForgeError, Revalidated};

/// The `Accept` every conditional read sends, since a tag holds only for the
/// `Accept` it was given
const ACCEPT: &str = "Accept: application/vnd.github+json";

/// The items on a page of a list the reads ask for
const PER_PAGE: usize = 100;

/// The pages a list is read to, past which it is cut off
const MAX_PAGES: usize = 50;

/// The answers kept at once, past which all are dropped
const KEPT: usize = 512;

/// What one request answered
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fetched {
    /// A new answer, with the tag GitHub gave it where it gave one
    Fresh { etag: Option<String>, body: Vec<u8> },
    /// The answer the request's tag names is still the answer
    NotModified,
}

/// How one REST request is made
pub(super) trait Fetch {
    /// `path` as the login in the config folder `config` (the maintainer's
    /// own when none), with `etag` as `If-None-Match` when given
    fn get(
        &self,
        config: Option<&Path>,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Fetched, ForgeError>;
}

// The login a read was made as (none for the maintainer's own), and its path
type Key = (Option<PathBuf>, String);

#[derive(Debug, Clone)]
struct Kept {
    etag: String,
    body: Vec<u8>,
}

/// The answers kept for conditional reads, shared by every read of a runner
#[derive(Debug, Clone)]
pub struct Etags {
    kept: Arc<Mutex<BTreeMap<Key, Kept>>>,
    revalidated: Revalidated,
    // Set once a request failed naming a 304 that the same one without a
    // tag did not, so `gh` is not passing a 304 on and no tag is sent again
    unsent: Arc<AtomicBool>,
}

impl Default for Etags {
    fn default() -> Self {
        Self::new(Revalidated::default())
    }
}

impl Etags {
    /// Nothing kept, adding each 304 to `revalidated`
    pub fn new(revalidated: Revalidated) -> Self {
        Self {
            kept: Arc::default(),
            revalidated,
            unsent: Arc::default(),
        }
    }

    /// Forgets what was kept for reads as the login in `config`, whose token
    /// changed
    pub fn forget(&self, config: &Path) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.retain(|(scope, _), _| scope.as_deref() != Some(config));
    }

    // `path`'s answer: the kept one when GitHub says it is unchanged
    pub(super) fn get(
        &self,
        fetch: &dyn Fetch,
        config: Option<&Path>,
        path: &str,
    ) -> Result<Vec<u8>, ForgeError> {
        let key = (config.map(Path::to_owned), path.to_owned());
        let kept = (self.kept.lock().unwrap_or_else(PoisonError::into_inner))
            .get(&key)
            .cloned()
            .filter(|_| !self.unsent.load(Ordering::Relaxed));
        let sent = kept.as_ref().map(|k| k.etag.as_str());
        let fetched = match fetch.get(config, path, sent) {
            Err(ForgeError::Failed(said)) if sent.is_some() && names_304(&said) => {
                let plain = fetch.get(config, path, None)?;
                self.unsent.store(true, Ordering::Relaxed);
                plain
            }
            answer => answer?,
        };
        match (fetched, kept) {
            (Fetched::NotModified, Some(kept)) => {
                self.revalidated.note();
                Ok(kept.body)
            }
            (Fetched::NotModified, None) => Err(ForgeError::Unreadable(
                "GitHub answered 304 to a read that sent no tag".to_owned(),
            )),
            (Fetched::Fresh { etag, body }, _) => {
                let mut all = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
                match etag {
                    Some(etag) => {
                        if all.len() >= KEPT {
                            all.clear();
                        }
                        all.insert(
                            key,
                            Kept {
                                etag,
                                body: body.clone(),
                            },
                        );
                    }
                    None => {
                        all.remove(&key);
                    }
                }
                Ok(body)
            }
        }
    }

    // Every page of the list at `path`, which carries `per_page=100` already
    pub(super) fn pages(
        &self,
        fetch: &dyn Fetch,
        config: Option<&Path>,
        path: &str,
    ) -> Result<Vec<Vec<u8>>, ForgeError> {
        let mut pages = Vec::new();
        for page in 1..=MAX_PAGES {
            let body = self.get(fetch, config, &format!("{path}&page={page}"))?;
            let items: Vec<IgnoredAny> =
                serde_json::from_slice(&body).map_err(|_| unreadable(&body))?;
            let full = items.len() >= PER_PAGE;
            pages.push(body);
            if !full {
                return Ok(pages);
            }
        }
        Err(too_many(path))
    }

    // Every page of a list GitHub answers as an object holding `field`'s array
    pub(super) fn pages_in(
        &self,
        fetch: &dyn Fetch,
        config: Option<&Path>,
        path: &str,
        field: &str,
    ) -> Result<Vec<Vec<u8>>, ForgeError> {
        let mut pages = Vec::new();
        for page in 1..=MAX_PAGES {
            let body = self.get(fetch, config, &format!("{path}&page={page}"))?;
            let seen = count_in(&body, field).ok_or_else(|| unreadable(&body))?;
            let full = seen >= PER_PAGE;
            pages.push(body);
            if !full {
                return Ok(pages);
            }
        }
        Err(too_many(path))
    }
}

// How many items `field`'s array holds, and None when the body is not JSON with one
fn count_in(body: &[u8], field: &str) -> Option<usize> {
    let said: serde_json::Value = serde_json::from_slice(body).ok()?;
    said.get(field)?.as_array().map(Vec::len)
}

fn too_many(path: &str) -> ForgeError {
    ForgeError::Unreadable(format!(
        "{path} has more than {MAX_PAGES} pages of {PER_PAGE}"
    ))
}

/// The requests, made with `gh api`
#[derive(Debug, Clone, Copy)]
pub(super) struct GhApi;

impl Fetch for GhApi {
    fn get(
        &self,
        config: Option<&Path>,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Fetched, ForgeError> {
        let mut command = gh_command(config);
        command.args(["api", "--include", "-H", ACCEPT]);
        if let Some(etag) = etag {
            command.args(["-H", &format!("If-None-Match: {etag}")]);
        }
        let output = command
            .arg(path)
            .output()
            .map_err(|e| ForgeError::Spawn(e.to_string()))?;
        answer(&output)
    }
}

// What `gh api --include` printed: the status and headers, a blank line and
// the body. Whether `gh` exits 0 on a 304 is not measured, so a 304 is read
// from the output or from the debug lines on stderr whatever the exit code.
fn answer(output: &Output) -> Result<Fetched, ForgeError> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (status, etag, body) = split(&output.stdout);
    if status == Some(304) || says_not_modified(&stderr) {
        return Ok(Fetched::NotModified);
    }
    if !output.status.success() {
        return Err(failed::failure(&stderr));
    }
    match status {
        Some(200..=299) => Ok(Fetched::Fresh {
            etag,
            body: body.to_vec(),
        }),
        _ => Err(unreadable(&output.stdout)),
    }
}

// Whether a failure says a 304, which `gh` may print as an error
fn names_304(said: &str) -> bool {
    said.contains("304") || said.to_ascii_lowercase().contains("not modified")
}

// The debug lines `GH_DEBUG=api` prints for the response
fn says_not_modified(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim_start_matches("< ");
        let mut words = line.split_whitespace();
        (words.next().is_some_and(|w| w.starts_with("HTTP/")) && words.next() == Some("304"))
            || line.starts_with("gh: HTTP 304")
    })
}

// The status, the `ETag` and the body of an `--include` answer
fn split(stdout: &[u8]) -> (Option<u16>, Option<String>, &[u8]) {
    let blank = |i: usize| stdout[i..].starts_with(b"\r\n\r\n") || stdout[i..].starts_with(b"\n\n");
    let Some(end) = (0..stdout.len()).find(|&i| blank(i)) else {
        return (None, None, stdout);
    };
    let gap = if stdout[end..].starts_with(b"\r\n\r\n") {
        4
    } else {
        2
    };
    let head = String::from_utf8_lossy(&stdout[..end]);
    let mut lines = head.lines();
    let status = (lines.next())
        .filter(|line| line.starts_with("HTTP/"))
        .and_then(|line| line.split_whitespace().nth(1)?.parse().ok());
    let etag = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("etag")
            .then(|| value.trim().to_owned())
    });
    (status, etag, &stdout[end + gap..])
}

#[cfg(test)]
mod tests;
