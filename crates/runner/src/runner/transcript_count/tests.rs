use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ports::{CodeGraph, Role, Session, Tools, Usage};
use crate::settings::{AgentHarness, ContextSize, EndpointUrl, ModelHost, ModelServer};
use crate::test::ticket;
use crate::test::{RecordedSink, short_tempdir};

// A pi 1.1.0 session given the code graph, whose model called the graph's `list_projects` and got
// an answer, and whose lines hold no system message of its tools
const PI: &str = include_str!("../../../../../fixtures/pi-session-mcp.jsonl");
// A pi 1.1.0 worker session given the code graph, recorded after #949, whose system message lists
// the graph's tools in `toolsAdded`
const PI_CONNECTED: &str = include_str!("../../../../../fixtures/pi-session-mcp-connected.jsonl");
// pi's `system` messages for the code graph's tools: added when the server connects, and removed
// when it goes, which pi writes by re-registering each tool hidden
const ADDED: &str = concat!(
    r#"{"type":"message","message":{"role":"system","content":"","toolsAdded":[{"name":"mcp__codebase_memory_mcp__search_graph"},{"name":"mcp__codebase_memory_mcp__list_projects"}]}}"#,
    "\n",
);
const REMOVED: &str = concat!(
    r#"{"type":"message","message":{"role":"system","content":"","toolsRemoved":[{"name":"mcp__codebase_memory_mcp__search_graph"},{"name":"mcp__codebase_memory_mcp__list_projects"}]}}"#,
    "\n",
);
// One real failed tool result for each kind each harness had, with the call that made it
const PI_ERRORS: &str = include_str!("../../../../../fixtures/pi-session-tool-errors.jsonl");
const CLAUDE_ERRORS: &str =
    include_str!("../../../../../fixtures/claude-transcript-tool-errors.jsonl");
const SESSION: &str = "8b2d6e41-0c3f-4a97-b5e8-1d7f9a2c4e60";

// pi, as far as its session file goes: each call appends its lines to the file pi names
// `<time>_<session>.jsonl` in its sessions folder. Each call appends the next of `texts`, and the
// last one again once they run out.
#[derive(Debug)]
struct Pi {
    sessions: PathBuf,
    texts: Vec<&'static str>,
    next: AtomicUsize,
}

impl Agents for Pi {
    fn prepare(&self, _call: &AgentCall) -> Result<(), AgentError> {
        Ok(())
    }

    fn run(&self, call: &AgentCall, _ending: &Ending) -> Result<AgentReply, AgentError> {
        let file = (self.sessions).join(format!("2026-10-15T19-59-36-998Z_{SESSION}.jsonl"));
        let before = std::fs::read_to_string(&file).unwrap_or_default();
        let at = self
            .next
            .fetch_add(1, Ordering::Relaxed)
            .min(self.texts.len() - 1);
        std::fs::write(&file, before + self.texts[at]).unwrap();
        Ok(AgentReply {
            session_id: call.session.id().clone(),
            text: "done".into(),
            usage: Usage::default(),
            session_cost: None,
            context: None,
            compactions: 0,
            metered: None,
        })
    }

    fn transcript(&self, _call: &AgentCall) -> Option<Transcript> {
        Some(Transcript {
            folder: self.sessions.clone(),
            threads: None,
        })
    }
}

// Claude Code, as far as its session file goes: it names the file for the session.
#[derive(Debug)]
struct Claude {
    sessions: PathBuf,
    recorded: &'static str,
}

impl Agents for Claude {
    fn prepare(&self, _call: &AgentCall) -> Result<(), AgentError> {
        Ok(())
    }

    fn run(&self, call: &AgentCall, _ending: &Ending) -> Result<AgentReply, AgentError> {
        let file = (self.sessions).join(format!("{}.jsonl", call.session.id().0));
        let before = std::fs::read_to_string(&file).unwrap_or_default();
        std::fs::write(&file, before + self.recorded).unwrap();
        Ok(AgentReply {
            session_id: call.session.id().clone(),
            text: "done".into(),
            usage: Usage::default(),
            session_cost: None,
            context: None,
            compactions: 0,
            metered: None,
        })
    }

    fn transcript(&self, _call: &AgentCall) -> Option<Transcript> {
        Some(Transcript {
            folder: self.sessions.clone(),
            threads: None,
        })
    }
}

// A worker's pi call in `folder`, given the code graph
fn pi_call(folder: &Path) -> AgentCall {
    let server = ModelServer {
        host: ModelHost::Url(EndpointUrl::try_from("http://127.0.0.1:8080".to_owned()).unwrap()),
        context: ContextSize::try_from(65_536).unwrap(),
    };
    let mut call = AgentCall::new(
        Role::Worker,
        Tools::Work,
        AgentHarness::Pi(server),
        "qwen",
        crate::settings::Effort::Medium,
        Session::Resume(SessionId(SESSION.into())),
        folder.join("worktree"),
        "go on",
    );
    call.issue = crate::ticket::TicketId::from_issue(7);
    call.reach.code_graph = Some(CodeGraph {
        binary: folder.join("codebase-memory-mcp"),
        index: folder.join("graph"),
        runtime: folder.join("cbm"),
    });
    call
}

// A worker's Claude Code call in `folder`
fn claude_call(folder: &Path) -> AgentCall {
    AgentCall::new(
        Role::Worker,
        Tools::Work,
        AgentHarness::ClaudeCode,
        "claude",
        crate::settings::Effort::Medium,
        Session::Resume(SessionId(SESSION.into())),
        folder.join("worktree"),
        "go on",
    )
}

fn counted_over(sessions: PathBuf, texts: Vec<&'static str>) -> (TranscriptCount, RecordedSink) {
    let sink = RecordedSink::default();
    let counted = TranscriptCount {
        agents: Arc::new(Pi {
            sessions,
            texts,
            next: AtomicUsize::new(0),
        }),
        notes: Notes::default(),
        metrics: Arc::new(Recorder::new(Arc::new(sink.clone()))),
    };
    (counted, sink)
}

#[test]
fn each_pi_call_counts_its_own_tool_calls_from_its_session_file() {
    let folder = short_tempdir();
    let sessions = folder.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let (counted, sink) = counted_over(sessions, vec![PI]);
    let call = pi_call(folder.path());
    let graph = [("server", "codebase-memory-mcp")];

    counted.run(&call, &Ending::default()).unwrap();
    let tool_calls = sink.value("kelpie_mcp_tool_calls_total", &graph);
    assert_eq!(
        tool_calls,
        Some(1.0),
        "mcp__codebase_memory_mcp__ is the graph's"
    );
    counted.run(&call, &Ending::default()).unwrap();
    let tool_calls = sink.value("kelpie_mcp_tool_calls_total", &graph);
    assert_eq!(
        tool_calls,
        Some(2.0),
        "the second call's own, not the first's again"
    );
}

#[test]
fn a_pi_call_counts_a_server_whose_tools_its_session_loaded_connected() {
    let folder = short_tempdir();
    let sessions = folder.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let (counted, sink) = counted_over(sessions, vec![PI_CONNECTED]);
    let call = pi_call(folder.path());
    let connected = [("outcome", "connected"), ("server", "codebase-memory-mcp")];
    let failed = [("outcome", "failed"), ("server", "codebase-memory-mcp")];

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &connected),
        Some(1.0)
    );
    assert_eq!(sink.value("kelpie_mcp_servers_total", &failed), None);
    assert!(counted.notes.standing_lines().is_empty());

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &connected),
        Some(2.0),
        "the session loaded the tools once, and each call was given the server"
    );
}

#[test]
fn a_pi_call_counts_a_server_its_session_answered_connected_and_one_it_said_nothing_of_failed() {
    let folder = short_tempdir();
    let sessions = folder.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let (counted, sink) = counted_over(sessions, vec![PI]);
    let mut call = pi_call(folder.path());
    call.reach.mcp_servers = [(
        "docs".to_owned(),
        serde_json::json!({ "command": "/k/docs" }),
    )]
    .into();
    let graph = [("outcome", "connected"), ("server", "codebase-memory-mcp")];
    let docs = [("outcome", "failed"), ("server", "repo")];

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(sink.value("kelpie_mcp_servers_total", &graph), Some(1.0));
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &docs),
        Some(1.0),
        "a server the project names is counted as repo"
    );
    let standing = counted.notes.standing_lines();
    assert_eq!(standing.len(), 1, "{standing:#?}");
    assert!(standing[0].contains("issue #7's worker"), "{}", standing[0]);
    assert!(
        standing[0].contains("ran without the MCP server `docs`"),
        "{}",
        standing[0]
    );
    assert!(
        standing[0].contains("its tools were not in the session as the call ended"),
        "pi records no reason: {}",
        standing[0]
    );

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &graph),
        Some(2.0),
        "the session answered the graph's tool"
    );
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &docs),
        Some(2.0),
        "and still says nothing of the other"
    );
    assert_eq!(counted.notes.standing_lines(), standing, "noted once");
}

#[test]
fn a_pi_call_counts_a_server_whose_tools_its_session_removed_after_loading_them_failed() {
    let folder = short_tempdir();
    let sessions = folder.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let (counted, sink) = counted_over(sessions, vec![ADDED, REMOVED]);
    let call = pi_call(folder.path());
    let connected = [("outcome", "connected"), ("server", "codebase-memory-mcp")];
    let failed = [("outcome", "failed"), ("server", "codebase-memory-mcp")];

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &connected),
        Some(1.0)
    );
    assert!(counted.notes.standing_lines().is_empty());

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &failed),
        Some(1.0),
        "the tools went, so the earlier addition does not stand"
    );
    assert_eq!(
        sink.value("kelpie_mcp_servers_total", &connected),
        Some(1.0)
    );
    let standing = counted.notes.standing_lines();
    assert_eq!(standing.len(), 1, "{standing:#?}");
    assert!(
        standing[0].contains("its tools were not in the session as the call ended"),
        "{}",
        standing[0]
    );
}

#[test]
fn each_pi_call_counts_its_own_requests_from_its_session_file() {
    let folder = short_tempdir();
    let sessions = folder.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let (counted, sink) = counted_over(sessions, vec![PI]);
    let call = pi_call(folder.path());
    let small = [("harness", "pi"), ("le", "150000"), ("role", "worker")];

    counted.run(&call, &Ending::default()).unwrap();
    let requests = sink.value("kelpie_request_context_tokens", &small);
    assert_eq!(requests, Some(2.0), "the recorded session's two requests");
    counted.run(&call, &Ending::default()).unwrap();
    let requests = sink.value("kelpie_request_context_tokens", &small);
    assert_eq!(
        requests,
        Some(4.0),
        "the second call's two, not the first's again"
    );
    let start = [("after", "start"), ("harness", "pi"), ("role", "worker")];
    assert_eq!(
        sink.value("kelpie_cache_write_tokens_total", &start),
        Some(0.0),
        "the recorded session wrote nothing to a cache"
    );
}

// The wrapper around `agents`, with its samples and notes kept
fn counted_with(agents: impl Agents + 'static) -> (TranscriptCount, RecordedSink, Notes) {
    let sink = RecordedSink::default();
    let notes = Notes::default();
    let counted = TranscriptCount {
        agents: Arc::new(agents),
        notes: notes.clone(),
        metrics: Arc::new(Recorder::new(Arc::new(sink.clone()))),
    };
    (counted, sink, notes)
}

fn sessions_of(folder: &Path) -> PathBuf {
    let sessions = folder.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    sessions
}

#[test]
fn each_claude_call_counts_its_own_failed_tool_calls_by_kind() {
    let folder = short_tempdir();
    let (counted, sink, _) = counted_with(Claude {
        sessions: sessions_of(folder.path()),
        recorded: CLAUDE_ERRORS,
    });
    let call = claude_call(folder.path());
    let guard = [("kind", "guard-block"), ("role", "worker")];
    let other = [("kind", "other"), ("role", "worker")];

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.total("kelpie_tool_errors_total"),
        12.0,
        "one for each recorded failure"
    );
    assert_eq!(
        sink.value("kelpie_tool_errors_total", &guard),
        Some(1.0),
        "kelpie's own `confine` hook"
    );
    assert_eq!(
        sink.value("kelpie_tool_errors_total", &other),
        Some(1.0),
        "a command that ran and failed"
    );

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.total("kelpie_tool_errors_total"),
        24.0,
        "the second call's own, not the first's again"
    );
    assert_eq!(
        sink.value("kelpie_tool_errors_total", &guard),
        Some(2.0),
        "the second call's own refusal"
    );
}

#[test]
fn each_pi_call_counts_its_own_failed_tool_calls_by_kind() {
    let folder = short_tempdir();
    let (counted, sink, _) = counted_with(Pi {
        sessions: sessions_of(folder.path()),
        texts: vec![PI_ERRORS],
        next: AtomicUsize::new(0),
    });
    let call = pi_call(folder.path());
    let guard = [("kind", "guard-block"), ("role", "worker")];
    let edit_miss = [("kind", "edit-miss"), ("role", "worker")];

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.total("kelpie_tool_errors_total"),
        8.0,
        "one for each recorded failure"
    );
    assert_eq!(
        sink.value("kelpie_tool_errors_total", &guard),
        Some(1.0),
        "the guard extension's own refusal"
    );
    assert_eq!(
        sink.value("kelpie_tool_errors_total", &edit_miss),
        Some(1.0),
        "an edit whose old text is not in the file"
    );
}

#[test]
fn a_hook_that_is_not_kelpies_is_named_in_a_note_and_in_no_label() {
    let folder = short_tempdir();
    let (counted, sink, notes) = counted_with(Claude {
        sessions: sessions_of(folder.path()),
        recorded: CLAUDE_ERRORS,
    });
    let mut call = claude_call(folder.path());
    call.issue = crate::ticket::TicketId::from_issue(794);

    counted.run(&call, &Ending::default()).unwrap();

    assert_eq!(
        notes.take(),
        ["issue #794's worker had one tool call refused by the hook \
             `/opt/homebrew/bin/node ~/.claude/hooks/kelpie-gate.js`"],
        "the hook is told once for the call, and kelpie's own `confine` refusal is told nowhere"
    );
    let labels: std::collections::BTreeSet<Vec<&str>> = sink
        .sent()
        .iter()
        .filter(|sample| sample.name == "kelpie_tool_errors_total")
        .map(|sample| sample.labels.iter().map(|(key, _)| *key).collect())
        .collect();
    assert_eq!(
        labels,
        std::collections::BTreeSet::from([vec!["kind", "role"]]),
        "no tool, path or command is in a label"
    );
}

#[test]
fn a_role_whose_requests_are_not_counted_counts_no_tool_errors() {
    let folder = short_tempdir();
    let (counted, sink, _) = counted_with(Claude {
        sessions: sessions_of(folder.path()),
        recorded: CLAUDE_ERRORS,
    });
    let mut call = claude_call(folder.path());
    call.role = Role::Triager;

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(sink.total("kelpie_tool_errors_total"), 0.0);
}

#[test]
fn a_carried_turn_counts_its_failed_tool_calls_and_names_a_hook_that_is_not_kelpies() {
    let folder = short_tempdir();
    let sessions = sessions_of(folder.path());
    let agents = Claude {
        sessions: sessions.clone(),
        recorded: CLAUDE_ERRORS,
    };
    let call = claude_call(folder.path());
    let Some(mark) = Mark::of(&agents, &call) else {
        panic!("a worker's Claude Code call has a mark");
    };
    std::fs::write(sessions.join(format!("{SESSION}.jsonl")), CLAUDE_ERRORS).unwrap();

    let sink = RecordedSink::default();
    let notes = Notes::default();
    mark.count(
        Harness::ClaudeCode,
        ticket(794),
        &Recorder::new(Arc::new(sink.clone())),
        &notes,
    );

    assert_eq!(
        sink.total("kelpie_tool_errors_total"),
        12.0,
        "one for each recorded failure"
    );
    assert_eq!(
        sink.value(
            "kelpie_tool_errors_total",
            &[("kind", "hook-block"), ("role", "worker")]
        ),
        Some(1.0)
    );
    assert_eq!(
        notes.take(),
        ["issue #794's worker had one tool call refused by the hook \
             `/opt/homebrew/bin/node ~/.claude/hooks/kelpie-gate.js`"],
        "the carried turn names the hook as the turn this image began would"
    );
}

#[test]
fn a_stand_in_call_is_counted_as_no_harness_counts() {
    let counted: Vec<Harness> = Harness::ALL
        .into_iter()
        .chain([Harness::StandIn])
        .filter(|h| Format::of_harness(*h).is_some())
        .collect();

    assert_eq!(counted, [Harness::ClaudeCode, Harness::Pi, Harness::Codex]);
}

// A Codex 0.159.3 call that compacted, as its rollout recorded it: five requests
const CODEX_ROLLOUT: &str = include_str!("../../../../../fixtures/codex-rollout-compacted.jsonl");
const CODEX_THREAD: &str = "01a0f7fd-4ed5-7773-80ea-165edc292d8d";

// Codex, as far as its rollout goes: it names the file for its own thread, in a folder for the day,
// and kelpie records which thread the session is once the first call ends.
#[derive(Debug)]
struct Codex {
    sessions: PathBuf,
    threads: PathBuf,
}

impl Agents for Codex {
    fn prepare(&self, _call: &AgentCall) -> Result<(), AgentError> {
        Ok(())
    }

    fn run(&self, call: &AgentCall, _ending: &Ending) -> Result<AgentReply, AgentError> {
        let day = self.sessions.join("2026/10/21");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join(format!("rollout-2026-10-21T15-40-45-{CODEX_THREAD}.jsonl"));
        let before = std::fs::read_to_string(&file).unwrap_or_default();
        // Each call's requests have responses of their own.
        let rollout = CODEX_ROLLOUT.replace("resp_", &format!("resp{}_", before.len()));
        std::fs::write(&file, before + &rollout).unwrap();
        std::fs::create_dir_all(&self.threads).unwrap();
        let record = self.threads.join(format!("{}.json", call.session.id().0));
        std::fs::write(record, format!(r#"{{"id":"{CODEX_THREAD}"}}"#)).unwrap();
        Ok(AgentReply {
            session_id: call.session.id().clone(),
            text: "done".into(),
            usage: Usage::default(),
            session_cost: None,
            context: None,
            compactions: 0,
            metered: None,
        })
    }

    fn transcript(&self, _call: &AgentCall) -> Option<Transcript> {
        Some(Transcript {
            folder: self.sessions.clone(),
            threads: Some(self.threads.clone()),
        })
    }
}

// A worker's Codex call in `folder`
fn codex_call(folder: &Path) -> AgentCall {
    AgentCall::new(
        Role::Worker,
        Tools::Work,
        AgentHarness::Codex,
        "gpt",
        crate::settings::Effort::Medium,
        Session::Resume(SessionId(SESSION.into())),
        folder.join("worktree"),
        "go on",
    )
}

#[test]
fn each_codex_call_counts_its_own_requests_from_its_rollout() {
    let folder = short_tempdir();
    let (counted, sink, _) = counted_with(Codex {
        sessions: sessions_of(folder.path()),
        threads: folder.path().join("threads"),
    });
    let call = codex_call(folder.path());
    let small = [("harness", "codex"), ("le", "150000"), ("role", "worker")];

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_request_context_tokens", &small),
        Some(5.0),
        "the recorded rollout's five requests, the compaction's included"
    );
    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(
        sink.value("kelpie_request_context_tokens", &small),
        Some(10.0),
        "the second call's five, not the first's again"
    );
}

#[test]
fn a_codex_call_of_any_role_but_the_worker_counts_no_request() {
    let folder = short_tempdir();
    let (counted, sink, _) = counted_with(Codex {
        sessions: sessions_of(folder.path()),
        threads: folder.path().join("threads"),
    });
    for role in [Role::Reviewer, Role::Pm] {
        let mut call = codex_call(folder.path());
        call.role = role;
        counted.run(&call, &Ending::default()).unwrap();
    }
    assert_eq!(sink.total("kelpie_request_context_tokens"), 0.0);
    assert_eq!(sink.total("kelpie_cache_write_tokens_total"), 0.0);
}

#[test]
fn a_codex_call_counts_no_mcp_server_and_no_tool_error() {
    let folder = short_tempdir();
    let (counted, sink, notes) = counted_with(Codex {
        sessions: sessions_of(folder.path()),
        threads: folder.path().join("threads"),
    });
    let mut call = codex_call(folder.path());
    call.reach.code_graph = Some(CodeGraph {
        binary: folder.path().join("codebase-memory-mcp"),
        index: folder.path().join("graph"),
        runtime: folder.path().join("cbm"),
    });

    counted.run(&call, &Ending::default()).unwrap();
    assert_eq!(sink.total("kelpie_mcp_tool_calls_total"), 0.0);
    assert_eq!(sink.total("kelpie_mcp_servers_total"), 0.0);
    assert_eq!(sink.total("kelpie_tool_errors_total"), 0.0);
    assert_eq!(notes.take(), Vec::<String>::new());
}

#[test]
fn a_carried_codex_turn_counts_its_requests_from_the_rollout_its_thread_names() {
    let folder = short_tempdir();
    let agents = Codex {
        sessions: sessions_of(folder.path()),
        threads: folder.path().join("threads"),
    };
    let call = codex_call(folder.path());
    let Some(mark) = Mark::of(&agents, &call) else {
        panic!("a worker's Codex call has a mark");
    };
    agents.run(&call, &Ending::default()).unwrap();

    let sink = RecordedSink::default();
    let notes = Notes::default();
    mark.count(
        Harness::Codex,
        ticket(1138),
        &Recorder::new(Arc::new(sink.clone())),
        &notes,
    );

    let small = [("harness", "codex"), ("le", "150000"), ("role", "worker")];
    assert_eq!(
        sink.value("kelpie_request_context_tokens", &small),
        Some(5.0)
    );
    assert_eq!(notes.take(), Vec::<String>::new());
}
