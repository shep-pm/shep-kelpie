//! The board briefing: `board.md`, what the project manager's agent reads
//!
//! Code writes it from what the runner already holds, so the agent never
//! runs git or gh. It has one entry per open work item with its session's
//! activity, the rulings waiting, the ready queue in the board's order,
//! the agents the project lists, the events since the agent's last wake,
//! and an overlap table of the files open branches and ready issues share. Its shape follows what was
//! measured: a PM briefed this way decided as well as one that looked for
//! itself, at about half the cost.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::agents::{Role, Tier};
use crate::ports::{CallActivity, Cost, Timestamp, Wait};
use crate::state::BoardEvent;
use crate::ticket::TicketId;

/// How long a call in flight may show nothing before the board calls it idle
pub const IDLE_AFTER: u64 = 10 * 60;

/// How much of each ready issue's body the board quotes, in characters
const EXCERPT: usize = 600;

/// How much of a worker's closing message a work item keeps, in characters
const SUMMARY: usize = 300;

/// A named path matching more files than this on `main` names none of them
const AMBIGUOUS: usize = 3;

/// What `status` shows of a runner that does not pick
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Run {
    /// `finish` holds the board back until the open work items end
    Finishing,
    /// The open work items ended, and the runner waits for its sheep to stop
    Finished,
}

/// Everything one board shows
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Briefing<'a> {
    /// The project
    pub project: &'a str,
    /// When it was written
    pub now: Timestamp,
    /// How many work items may hold a slot at once
    pub active_items: u32,
    /// How many work items parked on rulings stop the board opening work:
    /// the point where it stops, which items already working can pass
    pub pending_rulings: u32,
    /// How many open work items hold a slot
    pub held: usize,
    /// How many open work items are parked on rulings `concurrency.pending_rulings` counts
    pub parked: usize,
    /// Whether the runner is finishing, or finished and stopping, so the
    /// board opens nothing new
    pub run: Option<Run>,
    /// `origin/main`'s commit, once git has read it
    pub main: Option<String>,
    /// The open work items, oldest first
    pub items: Vec<Item>,
    /// The rulings waiting on the maintainer, oldest first
    pub rulings: Vec<RulingLine>,
    /// The workflows whose latest completed run on `main` failed, in the order
    /// they went red
    pub red: Vec<RedLine>,
    /// The ready queue as last read, `None` before the first read
    pub ready: Option<Ready>,
    /// The agents the project lists: its implementers, the default first, then
    /// its reviewers in their order
    pub agents: Vec<AgentLine>,
    /// The board events the project manager has not read
    pub events: Vec<BoardEvent>,
    /// Whether the project manager has read the board before
    pub woken_before: bool,
    /// Whether unread events were dropped for being old
    pub dropped: bool,
    /// The files each open work item's branch conflicts with `main` in,
    /// which kelpie catches it up for. A branch not listed merges cleanly
    /// with `main`.
    pub behind: BTreeMap<TicketId, Vec<String>>,
    /// What `git merge-tree` finds merging two open work items' branches,
    /// by their tickets, the lower first. A pair not listed merges cleanly,
    /// and the files a branch in `behind` conflicts with `main` in are in no
    /// pair: a clash there with a branch that holds `main`'s change is `main`'s.
    pub conflicts: BTreeMap<(TicketId, TicketId), Merge>,
}

/// What merging two open branches finds, when it is not a clean merge
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// These files conflict
    Conflicts(Vec<String>),
    /// git could not say, as one too old for `merge-tree --write-tree` or a
    /// missing commit
    Unknown,
}

/// An open work item
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Its ticket
    pub ticket: TicketId,
    /// The issue's title
    pub title: String,
    /// Where it stands, in words
    pub phase: String,
    /// Its pull request, once there is one
    pub pull_request: Option<u64>,
    /// When it opened, when known
    pub opened: Option<Timestamp>,
    /// The implementer its worker runs on
    pub agent: String,
    /// Its branch
    pub branch: String,
    /// What its session is doing
    pub session: Session,
    /// Since when its worker's turn has waited for its model, and for what
    pub waiting: Option<(Timestamp, Wait)>,
    /// The worker's last turn's closing message
    pub summary: Option<String>,
    /// What its priced calls have cost
    pub spent: Cost,
    /// What they may cost before its worker's next turn waits on a ruling
    pub cap: Cost,
    /// The files it touches
    pub files: Files,
}

/// What a work item's session is doing
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// A worker's turn is in flight
    Turn {
        /// When it started
        since: Timestamp,
        /// When its quiet counts from: the grant of the lease it waited for, or
        /// its start when it waited for none. `None` while it still waits for
        /// its model, when nothing it shows is late.
        active: Option<Timestamp>,
        /// What its harness says it last showed
        last: CallActivity,
    },
    /// A review call is in flight
    Review {
        /// When it started
        since: Timestamp,
        /// Since when it waits for its model, and for what
        waiting: Option<(Timestamp, Wait)>,
    },
    /// No call is in flight, and this is where the turn stands
    Still(String),
}

/// The files a work item touches
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    /// Its branch's diff from `main`
    Diff(Vec<String>),
    /// It has no branch yet, and its issue's body names these
    Named(Vec<String>),
    /// Neither is known
    Unknown,
}

impl Files {
    /// The files, none when unknown
    pub fn list(&self) -> &[String] {
        match self {
            Self::Diff(files) | Self::Named(files) => files,
            Self::Unknown => &[],
        }
    }
}

/// A ruling waiting on the maintainer
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulingLine {
    /// Its id
    pub id: TicketId,
    /// The work item it parks
    pub issue: Option<TicketId>,
    /// Its pull request
    pub pull_request: Option<u64>,
    /// The question's first line
    pub question: String,
}

/// A workflow red on `main`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedLine {
    /// The workflow's name
    pub workflow: String,
    /// When its first red run was created
    pub since: Timestamp,
    /// The commit its latest red run ran on
    pub head: String,
    /// Its latest red run's page on the forge
    pub url: String,
    /// What is being done about the latest red head
    pub doing: String,
}

/// The ready queue as the board last read it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// When it was read
    pub read: Timestamp,
    /// Its issues in the board's order, none of them open as a work item
    pub issues: Vec<ReadyLine>,
}

/// A ready issue
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyLine {
    /// Its ticket
    pub ticket: TicketId,
    /// Its title
    pub title: String,
    /// Its priority, such as `P1`
    pub priority: Option<&'static str>,
    /// Why the board passes over it, if it does
    pub waits: Option<String>,
    /// The paths its body names
    pub named: Vec<String>,
    /// The start of its body, for an issue the board may take
    pub excerpt: Option<String>,
}

/// An agent the project lists, as the agents section shows it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLine {
    /// Its name
    pub name: String,
    /// An implementer or a reviewer
    pub role: Role,
    /// Whether it is the implementer an issue with no `agent:` label runs on
    pub default: bool,
    /// Its tier, `standard` when its file names none
    pub tier: Tier,
    /// The kinds of work it takes, `None` for every kind
    pub kinds: Option<Vec<String>>,
    /// What its file says it is for
    pub description: Option<String>,
}

/// The board as `board.md` holds it
pub fn render(board: &Briefing<'_>) -> String {
    let mut out = String::new();
    let now = board.now;
    let _ = writeln!(out, "# Board: {}, {} UTC\n", board.project, date_time(now));
    let main = board
        .main
        .as_deref()
        .map_or(String::new(), |m| format!(" Main is at {}.", short(m)));
    let _ = writeln!(
        out,
        "{} work items open: {} of {} slots taken, and {} of {} parked on rulings.{main}\n",
        board.items.len(),
        board.held,
        board.active_items,
        board.parked,
        board.pending_rulings
    );
    match board.run {
        Some(Run::Finishing) => out.push_str(
            "The runner is finishing: the board opens nothing new, and the runner stops once \
             the open work items end.\n\n",
        ),
        Some(Run::Finished) => out.push_str(
            "The runner finished its open work items and is stopping: the board opens \
             nothing new.\n\n",
        ),
        None => {}
    }
    open_work(&mut out, board);
    rulings(&mut out, &board.rulings);
    red_on_main(&mut out, board);
    ready_queue(&mut out, board.ready.as_ref(), now);
    agents(&mut out, &board.agents);
    events(&mut out, board);
    overlap(&mut out, board);
    files(&mut out, &board.items);
    excerpts(&mut out, board.ready.as_ref());
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

fn open_work(out: &mut String, board: &Briefing<'_>) {
    out.push_str("## Open work\n\n");
    if board.items.is_empty() {
        out.push_str("(none)\n");
    }
    for item in &board.items {
        let pr = item
            .pull_request
            .map_or("no pull request yet".to_owned(), |n| format!("PR #{n}"));
        let opened = item.opened.map_or(String::new(), |t| {
            format!(", opened {} ago", age(board.now, t))
        });
        let touches = match &item.files {
            Files::Diff(files) => format!("touches {}", count(files.len(), "file")),
            Files::Named(files) => format!(
                "no commits yet, its body names {}",
                count(files.len(), "file")
            ),
            Files::Unknown => "no branch yet".to_owned(),
        };
        let _ = writeln!(
            out,
            "- {} \"{}\": {}; {pr}{opened}, on {}; {touches}",
            ticket_ref(item.ticket),
            item.title,
            item.phase,
            item.agent
        );
        let _ = writeln!(out, "  - Session: {}", session(&item.session, board.now));
        let _ = writeln!(
            out,
            "  - Spend: ${:.2} of ${:.2}",
            item.spent.usd(),
            item.cap.usd()
        );
        if let Some((since, why)) = item.waiting {
            let _ = writeln!(
                out,
                "  - Waiting for its model since {} ({}): for {}",
                clock(since),
                age(board.now, since),
                why.as_str()
            );
        }
        if let Some(summary) = &item.summary {
            let _ = writeln!(out, "  - Last turn: \"{summary}\"");
        }
        if let Some(files) = board.behind.get(&item.ticket) {
            let _ = writeln!(
                out,
                "  - Behind main: conflicts with it in {}; kelpie catches it up at the gate, so \
                 this is no conflict between work items",
                files.join(", ")
            );
        }
    }
    out.push('\n');
}

fn session(session: &Session, now: Timestamp) -> String {
    match session {
        Session::Turn {
            since,
            active,
            last,
        } => {
            let running = format!("worker turn running for {}", age(now, *since));
            let Some(active) = active else {
                return format!("{running}, waiting for its model");
            };
            let Some(quiet) = quiet_for(*active, *last, now) else {
                return format!("{running}, its activity unreadable");
            };
            let gap = age(now, Timestamp(now.0.saturating_sub(quiet)));
            match last {
                _ if quiet >= IDLE_AFTER => {
                    format!("{running}, idle: no tool call or output for {gap}")
                }
                CallActivity::At(at) if at >= since => {
                    format!("{running}, last tool call or output {gap} ago")
                }
                _ => format!("{running}, no tool call or output yet"),
            }
        }
        Session::Review { since, waiting } => {
            let running = format!("review call running for {}", age(now, *since));
            match waiting {
                Some((_, why)) => format!("{running}, waiting for {}", why.as_str()),
                None => running,
            }
        }
        Session::Still(what) => what.clone(),
    }
}

/// How long a turn whose quiet runs from `since` has shown nothing, at
/// `now`, or `None` when its harness keeps no record of what it shows
///
/// A record from before the turn's quiet began, as a resumed session's
/// transcript is, counts from `since`.
pub fn quiet_for(since: Timestamp, last: CallActivity, now: Timestamp) -> Option<u64> {
    let seen = match last {
        CallActivity::Untracked => return None,
        CallActivity::Nothing => since,
        CallActivity::At(at) => at.max(since),
    };
    Some(now.0.saturating_sub(seen.0))
}

fn rulings(out: &mut String, rulings: &[RulingLine]) {
    if rulings.is_empty() {
        return;
    }
    out.push_str("## Rulings waiting on the maintainer\n\n");
    for ruling in rulings {
        let issue = ruling
            .issue
            .map_or(String::new(), |t| format!(" on {}", ticket_ref(t)));
        let pr = ruling
            .pull_request
            .map_or(String::new(), |n| format!(" (PR #{n})"));
        let _ = writeln!(out, "- {}{issue}{pr}: {}", ruling.id, ruling.question);
    }
    out.push('\n');
}

fn red_on_main(out: &mut String, board: &Briefing<'_>) {
    if board.red.is_empty() {
        return;
    }
    out.push_str("## Red on main\n\n");
    for red in &board.red {
        let _ = writeln!(
            out,
            "- {}: red for {}, latest run {} at {}; {}",
            red.workflow,
            age(board.now, red.since),
            red.url,
            short(&red.head),
            red.doing
        );
    }
    out.push('\n');
}

fn ready_queue(out: &mut String, ready: Option<&Ready>, now: Timestamp) {
    let Some(ready) = ready else {
        out.push_str(
            "## Ready queue\n\nNot read since the runner started. The board reads it while \
             a slot is free.\n\n",
        );
        return;
    };
    let _ = writeln!(
        out,
        "## Ready queue, read {} ago (the board's order, first taken first; a new issue joins after the last of its priority)\n",
        age(now, ready.read)
    );
    if ready.issues.is_empty() {
        out.push_str("(empty)\n");
    }
    for (n, issue) in ready.issues.iter().enumerate() {
        let waits = issue
            .waits
            .as_ref()
            .map_or(String::new(), |w| format!("; waits: {w}"));
        let named = match issue.named.as_slice() {
            [] => "names no file".to_owned(),
            named => format!("names {}", named.join(", ")),
        };
        let _ = writeln!(
            out,
            "{}. {} [{}] \"{}\"{waits}; {named}",
            n + 1,
            ticket_ref(issue.ticket),
            issue.priority.unwrap_or("no priority"),
            issue.title
        );
    }
    out.push('\n');
}

fn agents(out: &mut String, agents: &[AgentLine]) {
    out.push_str("## Agents\n\n");
    if agents.is_empty() {
        out.push_str("(none)\n\n");
        return;
    }
    out.push_str(
        "Implementers, the default first, then reviewers in their order. An `agent:` label \
         picks a work item's implementer: tier and kinds are shown, nothing picks by them.\n\n",
    );
    for agent in agents {
        let default = if agent.default { ", default" } else { "" };
        let kinds = match &agent.kinds {
            Some(kinds) => format!("kinds {}", kinds.join(", ")),
            None => "every kind".to_owned(),
        };
        let _ = writeln!(
            out,
            "- {} ({}{default}): {} tier, {kinds}; {}",
            agent.name,
            agent.role.as_str(),
            agent.tier.as_str(),
            agent.description.as_deref().unwrap_or("no description")
        );
    }
    out.push('\n');
}

fn events(out: &mut String, board: &Briefing<'_>) {
    if board.woken_before {
        out.push_str("## Since your last wake\n\n");
    } else {
        out.push_str("## Recent events\n\n");
    }
    if board.dropped {
        out.push_str("(older unread events were dropped)\n");
    }
    if board.events.is_empty() {
        out.push_str("(none)\n");
    }
    for event in &board.events {
        let _ = writeln!(out, "- [{}] {} {}", event.id, clock(event.at), event.what);
    }
    out.push('\n');
}

fn overlap(out: &mut String, board: &Briefing<'_>) {
    out.push_str(
        "## Overlap\n\nFiles both sides touch, and the files `git merge-tree` finds in conflict \
         between two open branches. A branch's files are its diff from main; a ready issue's are \
         the paths its body names. Pairs not listed share nothing.\n\n",
    );
    let mut rows = Vec::new();
    for (i, a) in board.items.iter().enumerate() {
        for b in &board.items[i + 1..] {
            let key = (a.ticket.min(b.ticket), a.ticket.max(b.ticket));
            let merge = match board.conflicts.get(&key) {
                Some(Merge::Conflicts(files)) => files.join(", "),
                Some(Merge::Unknown) => "conflict check unavailable".to_owned(),
                None => String::new(),
            };
            let both = shared(a.files.list(), b.files.list());
            if !both.is_empty() || !merge.is_empty() {
                rows.push((label(a), label(b), merge, both.join(", ")));
            }
        }
    }
    let ready = board
        .ready
        .as_ref()
        .map_or(&[][..], |r| r.issues.as_slice());
    for issue in ready {
        for item in &board.items {
            let both = shared(&issue.named, item.files.list());
            if !both.is_empty() {
                let a = format!("{} (ready)", ticket_ref(issue.ticket));
                rows.push((a, label(item), String::new(), both.join(", ")));
            }
        }
    }
    if rows.is_empty() {
        out.push_str("(nothing shared)\n\n");
        return;
    }
    out.push_str("| A | B | In conflict | Both touch |\n| --- | --- | --- | --- |\n");
    for (a, b, conflicts, both) in rows {
        let [a, b, conflicts, both] = [a, b, conflicts, both].map(|c| c.replace('|', "\\|"));
        let _ = writeln!(out, "| {a} | {b} | {conflicts} | {both} |");
    }
    out.push('\n');
}

fn label(item: &Item) -> String {
    let ticket = ticket_ref(item.ticket);
    match item.pull_request {
        Some(pr) => format!("{ticket} (PR #{pr})"),
        None => ticket,
    }
}

/// How a ticket is named on the board: `#9` for a GitHub issue, `k12` for a
/// local ticket, which has no issue to point at.
pub fn ticket_ref(ticket: TicketId) -> String {
    match ticket {
        TicketId::Github(number) => format!("#{number}"),
        TicketId::Local(_) => ticket.to_string(),
    }
}

/// The files of `a` that `b` also names, in `a`'s order
pub fn shared<'a>(a: &'a [String], b: &[String]) -> Vec<&'a str> {
    let b: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    a.iter()
        .map(String::as_str)
        .filter(|f| b.contains(f))
        .collect()
}

fn files(out: &mut String, items: &[Item]) {
    let with_files: Vec<&Item> = items
        .iter()
        .filter(|i| !i.files.list().is_empty())
        .collect();
    if with_files.is_empty() {
        return;
    }
    out.push_str("## Files\n\n");
    for item in with_files {
        let how = match item.files {
            Files::Named(_) => ", named",
            _ => "",
        };
        let _ = writeln!(
            out,
            "- {} ({}{how}): {}",
            ticket_ref(item.ticket),
            item.branch,
            item.files.list().join(", ")
        );
    }
    out.push('\n');
}

fn excerpts(out: &mut String, ready: Option<&Ready>) {
    let quoted: Vec<&ReadyLine> = ready
        .map_or(&[][..], |r| r.issues.as_slice())
        .iter()
        .filter(|i| i.excerpt.is_some())
        .collect();
    if quoted.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "## Ready issues, the first {EXCERPT} characters of each body\n"
    );
    for issue in quoted {
        let text = issue.excerpt.as_deref().unwrap_or_default();
        let _ = writeln!(
            out,
            "### {} {}\n\n{text}\n",
            ticket_ref(issue.ticket),
            issue.title
        );
    }
}

/// The start of an issue's body, blank runs closed up, cut at a word past
/// 600 characters, each line quoted so none of it reads as the board's own
pub fn excerpt(body: &str) -> String {
    let mut text = String::new();
    let mut blank = 0;
    for line in body.trim().lines() {
        let line = line.trim_end();
        blank = if line.is_empty() { blank + 1 } else { 0 };
        if blank < 2 {
            text.push_str(line);
            text.push('\n');
        }
    }
    let cut = cut(text.trim_end(), EXCERPT);
    let quoted = cut.lines().map(|line| match line {
        "" => ">".to_owned(),
        line => format!("> {line}"),
    });
    quoted.collect::<Vec<_>>().join("\n")
}

/// A worker's closing message on one line, cut at a word past
/// 300 characters, or `None` when it said nothing
pub fn summary(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then(|| cut(&line, SUMMARY))
}

/// `text` cut at the last whole word within `chars` characters and closed
/// with ` …`, or `text` as it is when it fits
///
/// The result is at most `chars + 2` characters. A single word longer than
/// `chars` is cut where it stands.
pub fn cut(text: &str, chars: usize) -> String {
    let Some((end, _)) = text.char_indices().nth(chars) else {
        return text.to_owned();
    };
    let head = &text[..end];
    let whole = text[end..].starts_with(char::is_whitespace);
    let head = match head.rsplit_once(char::is_whitespace) {
        Some((words, _)) if !whole => words,
        _ => head,
    };
    format!("{} …", head.trim_end())
}

/// The paths an issue's body names in backticks, as files on `main`
///
/// A name matches a file on `main` by its whole path or its last parts, as
/// `board.rs` matches `src/board.rs`. One matching more than three
/// files names none, and one matching none counts only if it has a `/`.
pub fn named_paths(body: &str, tree: &[String]) -> Vec<String> {
    let mut found = BTreeSet::new();
    for name in path_names(body) {
        let hits = matching(name, tree);
        match hits.len() {
            0 if name.contains('/') => {
                found.insert(name.to_owned());
            }
            1..=AMBIGUOUS => found.extend(hits.into_iter().cloned()),
            _ => {}
        }
    }
    found.into_iter().collect()
}

/// The paths an issue's body names in backticks that match no file or folder on `main`
///
/// A name matches as in [`named_paths`], or is a folder holding files. One with
/// no `/` counts only if its extension is one files on `main` have, so a field
/// as `item.base` and a version as `1.85` are not paths. A path the issue means
/// to add is among them, so a note on one says it may be new.
pub fn missing_paths(body: &str, tree: &[String]) -> Vec<String> {
    let missing = path_names(body).filter(|name| {
        let known = name.contains('/') || extension_on(name, tree);
        known && matching(name, tree).is_empty() && !folder_on(name, tree)
    });
    missing
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether an issue's body names a path in backticks, for or against any tree
pub fn names_a_path(body: &str) -> bool {
    path_names(body).next().is_some()
}

/// The file on `main` that ends the way the missing `name` does, if one does
///
/// The files sharing its file name are weighed by how many of its last parts
/// they share, then by the shortest path.
pub fn closest<'a>(name: &str, tree: &'a [String]) -> Option<&'a str> {
    let parts: Vec<&str> = name.rsplit('/').collect();
    let shared = |file: &str| {
        let ends = file.rsplit('/').zip(&parts);
        ends.take_while(|(a, b)| a == *b).count()
    };
    tree.iter()
        .map(|file| (shared(file), file))
        .filter(|(shared, _)| *shared > 0)
        .min_by_key(|(shared, file)| (std::cmp::Reverse(*shared), file.len(), file.as_str()))
        .map(|(_, file)| file.as_str())
}

// The path-like backticked spans of a body.
fn path_names(body: &str) -> impl Iterator<Item = &str> {
    body.split('`').skip(1).step_by(2).filter_map(path_like)
}

// The files on `main` that `name` is the whole path or last parts of.
fn matching<'a>(name: &str, tree: &'a [String]) -> Vec<&'a String> {
    let suffix = format!("/{name}");
    (tree.iter())
        .filter(|f| **f == name || f.ends_with(&suffix))
        .collect()
}

// Whether `name` is a folder with files in it on `main`, at its root or below another.
fn folder_on(name: &str, tree: &[String]) -> bool {
    let folder = name.trim_end_matches('/');
    let (head, middle) = (format!("{folder}/"), format!("/{folder}/"));
    tree.iter()
        .any(|f| f.starts_with(&head) || f.contains(&middle))
}

// Whether a file on `main` ends with `name`'s extension.
fn extension_on(name: &str, tree: &[String]) -> bool {
    let extension = |path: &str| path.rsplit_once('.').map(|(_, ext)| ext.to_owned());
    let wanted = extension(name);
    tree.iter().any(|f| extension(f) == wanted)
}

// A span that reads as a path: no spaces, path characters only, and a
// folder or an extension. A `:line` after it is dropped.
fn path_like(span: &str) -> Option<&str> {
    let span = span.split_once(':').map_or(span, |(path, line)| {
        if line.chars().all(|c| c.is_ascii_digit()) {
            path
        } else {
            ""
        }
    });
    let span = span.strip_prefix("./").unwrap_or(span);
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-');
    if span.is_empty() || span.starts_with(['-', '/', '.']) || !span.chars().all(allowed) {
        return None;
    }
    let last = span.rsplit('/').next().unwrap_or(span);
    let extension = last
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && (1..=5).contains(&ext.len()));
    (span.contains('/') || extension).then_some(span)
}

/// How long ago `then` was at `now`, as `45m` or `3h05m`
pub fn age(now: Timestamp, then: Timestamp) -> String {
    elapsed(now.0.saturating_sub(then.0))
}

/// `seconds` as `45m` or `3h05m`
pub fn elapsed(seconds: u64) -> String {
    let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
    if hours == 0 {
        format!("{minutes}m")
    } else {
        format!("{hours}h{minutes:02}m")
    }
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn short(commit: &str) -> &str {
    commit.get(..10).unwrap_or(commit)
}

// The time of day in UTC, as `14:05`
fn clock(t: Timestamp) -> String {
    let seconds = t.0 % 86_400;
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

/// The date and time in UTC, as `2026-10-05 14:05`
pub fn date_time(t: Timestamp) -> String {
    let at = i64::try_from(t.0)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok());
    match at {
        Some(at) => at
            .to_string()
            .get(..16)
            .unwrap_or_default()
            .replace('T', " "),
        None => t.0.to_string(),
    }
}

#[cfg(test)]
mod tests;
