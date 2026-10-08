//! The issue writer: a request becomes issues an agent can build from
//!
//! `shep kelpie issue "<request>"` runs the `issue-writer` agent on its
//! own, in a detached checkout of `origin/main`, and files what it writes
//! as `ready-for-human` for the maintainer to read. Its session reads the
//! repo, and its guard runs only the commands that file, label and link
//! issues, and git's read-only ones. It ends with a list of what it filed,
//! which kelpie reads back from the forge and checks before printing.
//! `--interactive` starts `claude` in the maintainer's terminal instead, in
//! the project's checkout with the same prompt and tool limits, to plan the
//! request together and file it as `ready-for-agent`. That session is the
//! maintainer's: kelpie starts it and gets out of the way.
//!
//! Each listed implementer's `agent:` label is made on the repo the first
//! time the issue writer needs it, since the forge refuses a label the repo
//! lacks.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::adapters::{SystemClock, claude_settings};
use crate::agents::{Agents, ISSUE_WRITER, Role, Runs};
use crate::board::{AGENT_LABEL, READY, agent_label};
use crate::guard::IssueRules;
use crate::ports::Clock;
use crate::ports::{
    self, AgentCall, Ending, Fence, Forge, Guard, NewLabel, Reach, Session, SessionId, Tools,
};
use crate::profile::CREDENTIALS;
use crate::runner::{HUMAN, ProjectPaths};
use crate::settings::{AgentName, RoleAgents, RoleModel, Settings};
use crate::usage;
use crate::worktree;

mod prompt;

pub use prompt::instructions;

/// What the issue writer runs on, from its agent file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Writer {
    /// Its model and effort, on Claude Code
    pub model: RoleModel,
    /// Its file's body: its prompt
    pub prompt: String,
}

impl Writer {
    /// The `issue-writer` agent in `agents`
    ///
    /// # Errors
    ///
    /// A message when `agents` has no `issue-writer`, or its file is another role's.
    pub fn of(agents: &Agents) -> Result<Self, String> {
        let name = AgentName::kelpies(ISSUE_WRITER);
        let agent = agents.get(&name).ok_or_else(|| {
            format!("there is no `{ISSUE_WRITER}` agent: `shep kelpie add` writes kelpie's own")
        })?;
        let (Role::IssueWriter, Runs::Session { model, .. }, Some(prompt)) =
            (agent.role, &agent.runs, &agent.prompt)
        else {
            return Err(format!(
                "agent file `{ISSUE_WRITER}.md` is a {}'s: its role must be `issue-writer`",
                agent.role.as_str()
            ));
        };
        Ok(Self {
            model: model.clone(),
            prompt: prompt.clone(),
        })
    }
}

/// The project a request is for
#[derive(Debug, Clone, Copy)]
pub struct Project<'a> {
    /// Its settings: the checkout, the forge repo and the private names
    pub settings: &'a Settings,
    /// Where its files live under kelpie's home
    pub paths: &'a ProjectPaths,
    /// Its implementers, which an issue's `agent:` label names
    pub agents: &'a RoleAgents,
    /// The kelpie binary, which runs the guard
    pub kelpie: &'a Path,
}

/// How the issue writer runs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// On its own, filing for the maintainer to read
    Headless,
    /// In the maintainer's terminal, filing what they agree for a worker
    Interactive,
}

impl Mode {
    /// The status label what it files carries
    pub fn status(self) -> &'static str {
        match self {
            Self::Headless => HUMAN,
            Self::Interactive => READY,
        }
    }
}

/// Makes each label the issue writer files with that the repo lacks: the
/// status label and every listed implementer's `agent:` label
///
/// # Errors
///
/// A message naming the label the forge would not read or make.
pub fn make_labels(forge: &dyn Forge, project: &Project<'_>, mode: Mode) -> Result<(), String> {
    let repo = &project.settings.forge;
    let slug = repo.as_str();
    let have = forge
        .repo_labels(repo)
        .map_err(|e| format!("cannot read {slug}'s labels: {e}"))?;
    let status = crate::flock::add::LABELS
        .into_iter()
        .find(|l| l.name == mode.status());
    let agents: Vec<(String, String)> = (project.agents.implementers.iter())
        .map(|i| {
            (
                format!("{AGENT_LABEL}{}", i.name),
                format!("Built by kelpie's {} agent", i.name),
            )
        })
        .collect();
    let wanted = status
        .into_iter()
        .chain(agents.iter().map(|(name, about)| NewLabel {
            name,
            color: "c5def5",
            description: about,
        }));
    for label in wanted.filter(|l| !have.iter().any(|h| h.eq_ignore_ascii_case(l.name))) {
        forge
            .create_label(repo, &label)
            .map_err(|e| format!("cannot make the label `{}` on {slug}: {e}", label.name))?;
    }
    Ok(())
}

// The shell's history, which neither the harness nor gh reads, and the
// issue writer's sandbox does not either. gh's config and Claude Code's own
// files they do read, so only its file tools are kept from those.
const HISTORY: [&str; 3] = ["~/.zsh_history", "~/.bash_history", "~/.zsh_sessions/**"];

// What the issue writer's session may reach: `checkout` to read, GitHub to
// reach, nothing to write, and the guard that holds its commands, which
// keeps what the session filed and read in `ledger`.
fn reach(
    project: &Project<'_>,
    checkout: &Path,
    mode: Mode,
    ledger: PathBuf,
) -> Result<Reach, String> {
    let settings = project.settings;
    let paths = project.paths;
    let common = worktree::git(
        &settings.repo,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|e| e.to_string())?;
    let homes = [&paths.shep_home, &paths.kelpie_home]
        .into_iter()
        .enumerate()
        .filter(|&(i, home)| i == 0 || !home.starts_with(&paths.shep_home))
        .map(|(_, home)| format!("{}/**", home.display()));
    let rules = IssueRules {
        status: mode.status().to_owned(),
        agents: (project.agents.implementers.iter())
            .map(|i| i.name.as_str().to_owned())
            .collect(),
        ledger: Some(ledger),
    };
    let fence = Fence {
        write: Vec::new(),
        no_write: crate::fence::deny_write(checkout),
        no_read: (CREDENTIALS.iter().chain(&HISTORY))
            .map(|&p| p.to_owned())
            .chain(homes)
            .collect(),
        read: vec![checkout.to_owned(), project.kelpie.to_owned()],
        hosts: vec!["github.com".to_owned(), "api.github.com".to_owned()],
        no_commands: Vec::new(),
        env: Default::default(),
        sockets: Vec::new(),
        guard: Guard {
            kelpie: project.kelpie.to_owned(),
            worktree: checkout.to_owned(),
            build: checkout.to_owned(),
            git_common_dir: PathBuf::from(common),
            folders: vec![paths.kelpie_home.clone(), settings.repo.clone()],
            private_names: (settings.private_names.iter())
                .map(|n| n.as_str().to_owned())
                .collect(),
            issues: Some(rules),
        },
        hooks: Vec::new(),
    };
    Ok(Reach {
        read: Vec::new(),
        fence: Some(Box::new(fence)),
    })
}

/// Runs the issue writer on its own on `request`, and says what it filed
///
/// It works in a fresh detached checkout of `origin/main`, removed once its
/// reply is read. Each issue it lists is read back from the forge and
/// checked for its status label, one `agent:` label naming a listed
/// implementer, and acceptance criteria.
///
/// # Errors
///
/// A message naming what failed. A session that fails, or ends without
/// the list kelpie asked for, keeps its checkout and is named, so the
/// maintainer can resume it. An issue that fails its checks is named with
/// what it lacks, beside the rest of what was filed.
pub fn headless(
    project: &Project<'_>,
    writer: &Writer,
    request: &str,
    forge: &dyn Forge,
    agents: &dyn ports::Agents,
) -> Result<Vec<String>, String> {
    make_labels(forge, project, Mode::Headless)?;
    let session =
        crate::work_item::new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?;
    let folder = project.paths.issues();
    let checkout = folder.join(&session.0);
    let repo = &project.settings.repo;
    worktree::view(repo, &checkout)
        .map_err(|e| format!("cannot check out main for the issue writer: {e}"))?;
    let call = call(project, writer, request, &session, &checkout)?;
    let started = SystemClock.now();
    let ran = agents
        .prepare(&call)
        .and_then(|()| agents.run(&call, &Ending::default()));
    record(project, &call, &ran, started);
    let resume = |why: String| {
        format!(
            "{why}. Its checkout is kept: `cd {} && claude --resume {}` picks the session up",
            checkout.display(),
            session.0
        )
    };
    let reply = ran.map_err(|e| resume(format!("the issue writer's session failed: {e}")))?;
    let Some(filed) = read_reply(&reply.text) else {
        return Err(resume(format!(
            "the issue writer ended without the list of issues kelpie asked for, so \
             nothing it filed was checked. It ended: {}",
            tail(&reply.text)
        )));
    };
    // Best effort: what is left behind is only disk, and the issues are filed.
    let _ = worktree::remove_view(repo, &checkout);
    let _ = std::fs::remove_dir_all(call.settings.with_extension("tmp"));
    for file in [Some(&call.settings), call.instructions.as_ref()]
        .into_iter()
        .flatten()
    {
        let _ = std::fs::remove_file(file);
    }
    report(project, forge, &filed)
}

// Adds the run's line to the project's usage ledger. One that cannot be
// written is told and let go, since what was filed stands either way.
fn record(
    project: &Project<'_>,
    call: &AgentCall,
    ran: &Result<ports::AgentReply, ports::AgentError>,
    started: ports::Timestamp,
) {
    let Some(ended) = usage::ended(ran) else {
        return;
    };
    let draft = usage::Draft::of(call, ISSUE_WRITER, usage::CallKind::Issues, started);
    let spent = ran.as_ref().ok().map(usage::Spent::from);
    // A fresh session, so the call cost all of it.
    let line = draft.line(
        SystemClock.now(),
        ended,
        spent,
        ports::Cost(0),
        Default::default(),
    );
    let path = project.paths.folder.join(usage::FILE);
    if let Err(e) = usage::append_to(&path, &usage::Line::Call(line)) {
        eprintln!(
            "cannot add the issue writer's run to {}: {e}",
            path.display()
        );
    }
}

// The headless session's call: fresh, in `checkout`, prompted with the request.
fn call(
    project: &Project<'_>,
    writer: &Writer,
    request: &str,
    session: &SessionId,
    checkout: &Path,
) -> Result<AgentCall, String> {
    let folder = project.paths.issues();
    let prompt_file = folder.join(format!("{}.md", session.0));
    std::fs::write(&prompt_file, instructions(writer, project, Mode::Headless))
        .map_err(|e| format!("cannot write {}: {}", prompt_file.display(), e.kind()))?;
    let settings = folder.join(format!("{}.json", session.0));
    // The call's scratch folder, which Claude Code empties as the call starts
    // and its sandbox lets it write, so the ledger is the session's alone.
    let scratch = settings.with_extension("tmp");
    let mut reach = reach(
        project,
        checkout,
        Mode::Headless,
        scratch.join("ledger.json"),
    )?;
    reach.read.push(scratch);
    Ok(AgentCall {
        role: ports::Role::IssueWriter,
        harness: writer.model.harness.clone(),
        issue: 0,
        model: writer.model.model.as_str().to_owned(),
        effort: writer.model.effort,
        session: Session::New(session.clone()),
        cwd: checkout.to_owned(),
        settings,
        instructions: Some(prompt_file),
        prompt: format!("The maintainer's request:\n\n{}", request.trim()),
        plugin_dirs: Vec::new(),
        tools: Tools::Issues,
        reach,
        lease: None,
    })
}

/// What the headless issue writer's reply lists
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Filed {
    /// The issues it filed, a parent first
    pub filed: Vec<u64>,
    /// What it says to the maintainer
    #[serde(default)]
    pub note: String,
}

/// The list in the issue writer's reply, from its first `{` to its last `}`
pub fn read_reply(text: &str) -> Option<Filed> {
    let start = text.find('{')?;
    let end = text.rfind('}').filter(|&end| end > start)?;
    serde_json::from_str(&text[start..=end]).ok()
}

// The last of a reply, for a message.
fn tail(text: &str) -> String {
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    match chars.len() > 300 {
        true => format!(
            "...{}",
            chars[chars.len() - 300..].iter().collect::<String>()
        ),
        false => text.to_owned(),
    }
}

// Each filed issue as the forge holds it, with what it lacks.
fn report(project: &Project<'_>, forge: &dyn Forge, filed: &Filed) -> Result<Vec<String>, String> {
    let repo = &project.settings.forge;
    let slug = repo.as_str();
    let status = Mode::Headless.status();
    let mut lines = match filed.filed.len() {
        0 => vec![format!("the issue writer filed nothing on {slug}")],
        n => vec![format!(
            "the issue writer filed {n} on {slug}, `{status}` for you to read, then label \
             `{READY}`:"
        )],
    };
    let mut wrong = false;
    for &number in &filed.filed {
        let issue = match forge.issue(repo, number) {
            Ok(issue) => issue,
            Err(e) => {
                wrong = true;
                lines.push(format!("#{number}: cannot read it back: {e}"));
                continue;
            }
        };
        // As the board will read it once the maintainer labels it ready.
        let names = project.agents.implementer_names();
        let mut lacks = Vec::new();
        let mut about = Vec::new();
        match agent_label(&issue.labels, &names) {
            Ok(Some(agent)) => about.push(format!("{AGENT_LABEL}{agent}")),
            Ok(None) => lacks.push(format!("no `{AGENT_LABEL}` label")),
            Err(e) => lacks.push(e.to_string()),
        }
        if !issue.labels.iter().any(|l| l.eq_ignore_ascii_case(status)) {
            lacks.push(format!("no `{status}` label"));
        }
        if !has_criteria(&issue.body) {
            lacks.push("no acceptance criteria".to_owned());
        }
        about.extend(issue.parent.map(|p| format!("sub-issue of #{p}")));
        let line = match about.is_empty() {
            true => format!("#{number} {}", issue.title),
            false => format!("#{number} {} ({})", issue.title, about.join(", ")),
        };
        match lacks.is_empty() {
            true => lines.push(line),
            false => {
                wrong = true;
                lines.push(format!("{line}: {}", lacks.join("; ")));
            }
        }
    }
    let note = filed.note.trim();
    if !note.is_empty() {
        lines.push(format!("note: {note}"));
    }
    match wrong {
        true => Err(lines.join("\n")),
        false => Ok(lines),
    }
}

/// Whether `body` has an acceptance criteria section: a heading that names
/// it, then a list item
pub fn has_criteria(body: &str) -> bool {
    let mut lines = body.lines().map(str::trim);
    let heading = lines.by_ref().any(|line| {
        line.starts_with('#') && line.to_ascii_lowercase().contains("acceptance criteria")
    });
    heading && lines.any(|line| line.starts_with("- ") || line.starts_with("* "))
}

/// The interactive issue writer's `claude`, and the files it is started with
#[derive(Debug)]
pub struct Interactive {
    /// The command, which inherits the terminal
    pub command: Command,
    /// Its settings file and its ledger, to remove once it ends
    pub files: Vec<PathBuf>,
}

/// `program`, Claude Code, started on `request` in the project's checkout,
/// with the issue writer's prompt appended and its tool limits
///
/// Its settings file, written here, carries the deny rules and the guard,
/// and allows no command outright, so Claude Code asks the maintainer
/// before each one the guard lets through. The maintainer's own settings
/// stay in force beside it.
///
/// # Errors
///
/// A message naming the file that could not be written, or the git command
/// that could not read the checkout.
pub fn interactive(
    project: &Project<'_>,
    writer: &Writer,
    request: &str,
    program: &OsStr,
) -> Result<Interactive, String> {
    let folder = project.paths.issues();
    let checkout = &project.settings.repo;
    let run = crate::work_item::new_session_id()
        .map_err(|e| format!("cannot draw a name for its files: {e}"))?;
    let file = folder.join(format!("interactive-{}.json", run.0));
    let ledger = folder.join(format!("interactive-{}.ledger.json", run.0));
    let reach = reach(project, checkout, Mode::Interactive, ledger.clone())?;
    let mut settings = claude_settings(Tools::Issues, &reach);
    // That session is not in kelpie's sandbox, so the maintainer's own stays
    // as it is, and the maintainer is there to say yes to each command.
    if let Some(settings) = settings.as_object_mut() {
        settings.remove("sandbox");
    }
    if let Some(permissions) = settings["permissions"].as_object_mut() {
        permissions.remove("allow");
    }
    let text = serde_json::to_string_pretty(&settings).expect("settings are JSON");
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&file, text))
        .map_err(|e| format!("cannot write {}: {}", file.display(), e.kind()))?;
    let mut command = crate::spawn::command(program);
    command
        .arg("--model")
        .arg(writer.model.model.as_str())
        .arg("--effort")
        .arg(writer.model.effort.as_str())
        .arg("--settings")
        .arg(&file)
        .arg("--append-system-prompt")
        .arg(instructions(writer, project, Mode::Interactive))
        .arg(request.trim())
        .current_dir(checkout);
    Ok(Interactive {
        command,
        files: vec![file, ledger],
    })
}

#[cfg(test)]
mod tests;
