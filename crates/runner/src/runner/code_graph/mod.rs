//! A code graph of a work item's worktree for the calls that read the code
//!
//! A machine with the server installed has each reviewer and the triager
//! indexed before the call starts. A worker never gets it. The index is kept
//! under kelpie's home per work item, and goes with the work item's worktree. A
//! triage view's index goes when its call ends.
//!
//! The call is prepared with the runner's lock held, and indexing takes
//! seconds to minutes, so it runs on the call's own thread, where a stop
//! ends it.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::Runner;
use super::claim::Notes;
use crate::metrics::{Feature, Recorder};
use crate::ports::{AgentCall, CodeGraph, Ending, Role, Tools};
use crate::ticket::TicketId;

// An index that takes longer is not worth holding a turn for.
const INDEX_LIMIT: Duration = Duration::from_secs(300);

// The standing warning: told once however many calls meet it.
const INDEX_FAILED: &str = "code-graph-index-failed";

// How much of an indexer's output a note repeats.
const TAIL: usize = 300;

impl Runner {
    /// Names the code graph server and its index in `call`'s reach, if the
    /// server is installed and the call reads the code
    ///
    /// The index is built by [`indexed`], once the call's thread has started.
    /// A machine without the server runs its calls without, and says nothing.
    pub(super) fn with_code_graph(&self, call: &mut AgentCall) {
        let tools = &self.paths.tools;
        if !reads_code(call) || !tools.has_code_graph() {
            return;
        }
        // Only a call that works an issue reads code.
        let Some(ticket) = call.issue else {
            return;
        };
        // A work item of the same ticket would share the index of a triage view.
        let (index, runtime) = match call.role {
            Role::Triager => (
                self.paths.triage_graph(ticket),
                self.paths.triage_graph_runtime(ticket),
            ),
            _ => (
                self.paths.code_graph(ticket),
                self.paths.code_graph_runtime(ticket),
            ),
        };
        call.reach.code_graph = Some(CodeGraph {
            binary: tools.code_graph(),
            index,
            runtime,
        });
    }

    /// Removes the index of the worktree for `ticket`, and its daemon's folder
    ///
    /// # Errors
    ///
    /// The folder that could not be removed, and why.
    pub(super) fn remove_code_graph(&self, ticket: TicketId) -> Result<(), String> {
        remove(&[
            self.paths.code_graph(ticket),
            self.paths.code_graph_runtime(ticket),
        ])
    }

    // A server pi, Codex or goose cannot load is left out, and a note says which and why. goose
    // loads one with env keys it skips, without them, and a note says which.
    pub(super) fn without_servers_the_harness_cannot_load(&self, call: &mut AgentCall) {
        // parity: harness.mcp-servers
        let (harness, facts) = (call.harness.clone(), &self.ports.harness_mcp);
        let who = who(call);
        call.reach.mcp_servers.retain(|name, entry| {
            let refused = facts.refusal(&harness, name, entry);
            if let Some(why) = &refused {
                self.notes
                    .push(format!("{who} runs without the MCP server `{name}`: {why}"));
            }
            refused.is_none()
        });
        // parity: harness.mcp-servers
        for (name, entry) in &call.reach.mcp_servers {
            if let Some(skipped) = facts.skipped_env(&harness, name, entry) {
                self.notes.push(format!(
                    "{who} runs the MCP server `{name}` without {skipped}"
                ));
            }
        }
    }
}

// The reviewers and the triager read the code. The worker's turns never do:
// no recorded worker session has answered one of the server's tools, and its 17
// definitions ride in every request of every turn (`docs/facts/mcp-servers.md`).
fn reads_code(call: &AgentCall) -> bool {
    call.harness.loads_mcp()
        && matches!(
            (call.role, call.tools),
            (Role::Reviewer | Role::Triager, Tools::Review)
        )
}

pub(super) fn who(call: &AgentCall) -> String {
    match call.issue {
        Some(ticket) => format!("{}'s {}", ticket.named().to_lowercase(), name(call.role)),
        None => format!("issue #0's {}", name(call.role)),
    }
}

// The note's owner for a worker's turn a previous image began, which carries its issue and
// nothing else of its call
pub(super) fn who_carried(ticket: TicketId) -> String {
    format!("{}'s {}", ticket.named().to_lowercase(), name(Role::Worker))
}

/// Indexes `call`'s worktree for the server its reach names, on the call's
/// own thread
///
/// A call whose index cannot be built, or is ended while it builds, loses the
/// server and runs without it, and a note says why.
pub(super) fn indexed(call: &mut AgentCall, notes: &Notes, metrics: &Recorder, ending: &Ending) {
    let Some(graph) = &call.reach.code_graph else {
        return;
    };
    match index(graph, &call.cwd, ending) {
        Ok(()) => {
            metrics.feature_ran(Feature::CodeGraph, "ok");
            notes.ends(INDEX_FAILED);
        }
        Err(e) => {
            let line = format!("{} runs without a code graph: {e}", who(call));
            // A call ended while it indexes says nothing of the graph itself.
            if ending.asked() {
                metrics.feature_ran(Feature::CodeGraph, "ended");
                notes.push(line);
            } else {
                metrics.feature_ran(Feature::CodeGraph, "failed");
                notes.standing(INDEX_FAILED, line);
            }
            call.reach.code_graph = None;
        }
    }
}

fn name(role: Role) -> &'static str {
    match role {
        Role::Worker => "worker",
        Role::Reviewer => "reviewer",
        Role::Pm => "project manager",
        Role::Representative => "representative",
        Role::Triager => "triager",
    }
}

/// Removes the folders of a graph, which may be gone already
///
/// # Errors
///
/// The folder that could not be removed, and why.
pub(super) fn remove(folders: &[std::path::PathBuf]) -> Result<(), String> {
    for folder in folders {
        match std::fs::remove_dir_all(folder) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(format!("cannot remove {}: {e}", folder.display()));
            }
            _ => {}
        }
    }
    Ok(())
}

// Indexes `worktree` into the graph's folder, which the server reads back.
// Run again for each call, since the server only watches what it is running over.
// A call asked to end kills the indexer.
fn index(graph: &CodeGraph, worktree: &Path, ending: &Ending) -> Result<(), String> {
    for folder in [&graph.index, &graph.runtime] {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    }
    // The daemon serves its graph over HTTP on a local port unless its index folder says not to.
    let ui_off = ["config", "set", "ui_enabled", "false"].map(OsStr::new);
    run(graph, worktree, "turning its web UI off", &ui_off, ending)?;
    let index = [
        OsStr::new("cli"),
        "index_repository".as_ref(),
        "--repo-path".as_ref(),
    ];
    run(
        graph,
        worktree,
        "indexing",
        &[&index[..], &[worktree.as_os_str()]].concat(),
        ending,
    )
    .map_err(|e| match too_long(&graph.runtime) {
        Some(why) => format!("{e}; {why}"),
        None => e,
    })
}

// Why the daemon's socket, `cbm-daemon-<uid>/cbm-<16 hex>.sock` under `runtime`, cannot bind
// there, when its path is too long: the daemon says only that its endpoint could not be made.
// The folder is this user's, so its owner is the uid. Windows has neither the folder nor the
// socket path limit, so it says nothing.
#[cfg(unix)]
fn too_long(runtime: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(runtime).ok()?.uid();
    let socket = runtime.join(format!("cbm-daemon-{uid}/cbm-{}.sock", "0".repeat(16)));
    crate::home::socket_fits(&socket).err()
}

#[cfg(windows)]
fn too_long(_runtime: &Path) -> Option<String> {
    None
}

// Runs the server's `args` over the graph's folders, saying `what` it was doing when it fails.
fn run(
    graph: &CodeGraph,
    worktree: &Path,
    what: &str,
    args: &[&OsStr],
    ending: &Ending,
) -> Result<(), String> {
    let log = graph.index.join("kelpie-index.log");
    let output =
        std::fs::File::create(&log).map_err(|e| format!("cannot write {}: {e}", log.display()))?;
    let more = output
        .try_clone()
        .map_err(|e| format!("cannot write {}: {e}", log.display()))?;
    let mut child = crate::spawn::command(&graph.binary)
        .args(args)
        .env("CBM_CACHE_DIR", &graph.index)
        .env("CBM_RUNTIME_DIR", &graph.runtime)
        .env("CBM_ALLOWED_ROOT", worktree)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(more)
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", graph.binary.display()))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if !ending.asked() && started.elapsed() < INDEX_LIMIT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match ending.asked() {
                    true => format!("the call was ended while {what}"),
                    false => format!("{what} took over {}s", INDEX_LIMIT.as_secs()),
                });
            }
            Err(e) => return Err(format!("cannot wait for the indexer: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    let said = said.trim();
    let from = said.len().saturating_sub(TAIL);
    let tail = said.get(from..).unwrap_or(said);
    Err(format!("{what} failed with {status}: {tail}"))
}

#[cfg(test)]
mod tests;
