//! The project manager's call: its folder, files and fence
//!
//! It runs in its own folder, which holds a copy of the board as it stood
//! when it woke, each open branch's diff and its notes. Of kelpie's and the
//! shepherd's homes it reads that folder and the project's worktrees alone,
//! and it reads the checkout, never a secret-shaped file in either, `gh`'s
//! config or the credentials no worker reads. The rest of the machine stays readable to its sandbox, as
//! for a reviewer, since Claude Code reads its own files wherever they are.
//! It reaches no host but the model's, has only its read and file tools,
//! and writes only by adding to its notes, which a hook enforces.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::board::priority;
use crate::hooks::Hooks;
use crate::ports::{
    AgentCall, Cost, Fence, Guard, PM_NOTES, Reach, Role, Session, Severity, Tools,
};
use crate::runner::Runner;
use crate::runner::briefing::phase_text;
use crate::settings::{PmAgent, RulingName};
use crate::skills::Step;
use crate::state::{Ruling, RulingKind, Stuck};
use crate::ticket::TicketId;
use crate::work_item::{Phase, ReviewStage, WorkItem};

/// `gh`'s config, its token with it, which a worker reads to open its pull
/// request and the project manager never needs
const GH_CONFIG: &str = "~/.config/gh/**";

/// The files under the checkout and the worktrees that it never reads,
/// since each is where a secret is kept: a remote's URL in `.git/config`
/// can carry a token, and the rest hold keys and tokens by convention
pub(in crate::runner) const SECRET_SHAPED: [&str; 7] = [
    ".git/config",
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/.npmrc",
    "**/.netrc",
];

impl Runner {
    // The call, with its folder, notes, settings and prompt on disk, and the
    // `board` where it `reads` it: a compaction or a retro reads neither.
    pub(super) fn pm_call(
        &mut self,
        agent: &PmAgent,
        session: Session,
        prompt: String,
        board: Option<String>,
    ) -> Result<AgentCall, String> {
        let call = self.pm_call_made(agent, session, prompt, board)?;
        self.prepared(call)
    }

    /// The batch retro's call: the project manager's agent in its folder, with
    /// no standing instructions, tools that read and search only, and the plugin
    /// folder its skill is in
    pub(in crate::runner) fn pm_retro_call(
        &mut self,
        agent: &PmAgent,
        session: Session,
        prompt: String,
    ) -> Result<AgentCall, String> {
        let mut call = self.pm_call_made(agent, session, prompt, None)?;
        call.tools = Tools::Retro;
        call.instructions = None;
        call.settings = self.paths.worker.join("pm-retro-settings.json");
        call.plugin_dirs = (self.skills.plugin_dir(Step::Retro).into_iter())
            .map(Path::to_owned)
            .collect();
        self.prepared(call)
    }

    fn pm_call_made(
        &mut self,
        agent: &PmAgent,
        session: Session,
        prompt: String,
        board: Option<String>,
    ) -> Result<AgentCall, String> {
        let folder = self.paths.pm.clone();
        let failed = |what: &std::path::Path, e: std::io::Error| {
            format!("cannot write {}: {}", what.display(), e.kind())
        };
        std::fs::create_dir_all(&folder).map_err(|e| failed(&folder, e))?;
        let notes = folder.join(PM_NOTES);
        if !notes.exists() {
            std::fs::write(&notes, "").map_err(|e| failed(&notes, e))?;
        }
        // A copy, so the board it reads holds still while it reads, with the
        // code to read. It is the same text the wake's prompt carried.
        if let Some(text) = &board {
            let path = folder.join("board.md");
            std::fs::write(&path, text).map_err(|e| failed(&path, e))?;
        }
        let instructions = self.paths.worker.join("pm-instructions.md");
        let text = agent.prompt.as_deref().unwrap_or_default();
        crate::runner::turn::write(&self.paths.worker, &instructions, text)?;
        Ok(AgentCall {
            settings: self.paths.worker.join("pm-settings.json"),
            instructions: Some(instructions),
            reach: self.pm_reach(),
            plugin_dirs: (self.skills.issue_plugin().into_iter())
                .map(Path::to_owned)
                .collect(),
            ..AgentCall::new(
                Role::Pm,
                Tools::Pm,
                agent.model.harness.clone(),
                agent.model.model.as_str().to_owned(),
                agent.model.effort,
                session,
                folder,
                prompt,
            )
        })
    }

    /// The board a wake carries in its prompt and `board.md` in its folder
    /// holds: the board as the runner last wrote it, with the section naming
    /// each open work item's worktree and its diff
    ///
    /// # Errors
    ///
    /// A message naming the file that could not be read.
    pub(super) fn pm_board(&mut self) -> Result<String, String> {
        let folder = self.paths.pm.clone();
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("cannot make {}: {}", folder.display(), e.kind()))?;
        let mut text = match std::fs::read_to_string(&self.paths.board) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(format!(
                    "cannot read the board for the project manager: {}",
                    e.kind()
                ));
            }
        };
        text.push_str(&format!("\n{}", self.pm_diffs(&folder)));
        text.push_str(&self.pm_over_budget());
        Ok(text)
    }

    /// The tickets of the work items parked on an over-budget ruling, whose
    /// priority the wake reads through the ticket source with the lock let go
    pub(super) fn over_budget_tickets(&self) -> Vec<TicketId> {
        (self.state.rulings.iter())
            .filter(|r| is_over_budget(r))
            .filter_map(|r| r.issue)
            .collect()
    }

    // The board's section on each over-budget ruling: whose call it is, and
    // what the maintainer would weigh to raise the cap. Empty with none.
    fn pm_over_budget(&self) -> String {
        let rulings: Vec<&Ruling> = (self.state.rulings.iter())
            .filter(|r| is_over_budget(r))
            .collect();
        if rulings.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\n## Over-budget rulings\n\nEach work item over its spending cap, and what bears on \
             raising it.\n",
        );
        for ruling in rulings {
            out.push('\n');
            out.push_str(&self.over_budget_block(ruling));
        }
        out
    }

    // One over-budget ruling's block.
    fn over_budget_block(&self, ruling: &Ruling) -> String {
        let RulingKind::Stuck(Stuck::OverBudget {
            phase,
            spent,
            cap,
            by_role,
        }) = &ruling.kind
        else {
            return String::new();
        };
        let named = ruling
            .issue
            .map_or_else(|| "no work item".to_owned(), |t| t.shown());
        let mut out = format!("### Ruling {} on {named}\n\n", ruling.id);
        let call = match self.settings.rulings.pm_decides(RulingName::OverBudget) {
            true => "yours: your option is applied, and the cap doubles",
            false => "the maintainer's: your option is a recommendation, and nothing is applied",
        };
        let item = ruling.issue.and_then(|t| self.state.item(t));
        let now = self.ports.clock.now();
        let _ = writeln!(out, "- Whose call: {call}");
        let _ = writeln!(out, "- How close: {}", phase_text(phase, now));
        let _ = writeln!(out, "- Open findings: {}", open_findings(phase, item));
        let _ = writeln!(
            out,
            "- CI: {}",
            item.map_or_else(|| ci(None), |i| ci(Some(i)))
        );
        let touches = ruling
            .issue
            .map_or(String::new(), |t| self.touches(t, item));
        let priority = ruling
            .issue
            .map_or_else(|| "priority not read".to_owned(), |t| self.priority_line(t));
        let _ = writeln!(out, "- How critical: {priority}; {touches}");
        let raises = item.map_or(0, |i| i.budget_raises);
        let _ = writeln!(
            out,
            "- Spend: {} against a cap of {} (the worker {}, the reviewers {}), the cap raised \
             {raises} time{} before",
            dollars(*spent),
            dollars(*cap),
            dollars(by_role.worker),
            dollars(by_role.reviewer),
            if raises == 1 { "" } else { "s" }
        );
        out
    }

    // The priority `ticket`'s labels give it, as the wake's reads or a read now found.
    fn priority_line(&self, ticket: TicketId) -> String {
        let kept = self.pm.logs.as_ref().and_then(|logs| logs.found(ticket));
        let issue =
            (kept.cloned()).unwrap_or_else(|| self.ports.tickets.issue(&self.remote, ticket));
        match issue {
            Ok(issue) => match priority(&issue.labels) {
                Some(p) => format!("priority {p}"),
                None => "no priority label".to_owned(),
            },
            Err(_) => "priority not read".to_owned(),
        }
    }

    // What the item touches and how heavy that is.
    fn touches(&self, ticket: TicketId, item: Option<&WorkItem>) -> String {
        let files = self.item_files(ticket);
        let list = files.list();
        let weight = item
            .and_then(|i| i.weight.as_ref())
            .map_or("not weighed".to_owned(), ToString::to_string);
        if list.is_empty() {
            return format!("touches no file the board knows, {weight}");
        }
        const SHOWN: usize = 12;
        let mut names: Vec<&str> = list.iter().take(SHOWN).map(String::as_str).collect();
        if list.len() > SHOWN {
            names.push("...");
        }
        let names = self.shown(names.join(", "));
        format!("touches {} file(s), {weight}: {names}", list.len())
    }

    // Its folder to read and write, the worktrees and checkout to read;
    // nothing else of the homes, nor `gh`'s token; no host but the model's.
    pub(super) fn pm_reach(&self) -> Reach {
        let folder = self.paths.pm.clone();
        let code = [self.paths.worktrees(), self.settings.git.checkout.clone()];
        self.fenced_reach(&folder, &code, vec![folder.clone()], Vec::new())
    }

    // A session that reads its `folder` and the `code` trees, writes `write`
    // and reaches `hosts` besides the model's; of kelpie's and the shepherd's
    // homes it reads its folder alone, and in the code trees no secret-shaped file.
    pub(in crate::runner) fn fenced_reach(
        &self,
        folder: &std::path::Path,
        code: &[std::path::PathBuf],
        write: Vec<std::path::PathBuf>,
        hosts: Vec<String>,
    ) -> Reach {
        let folder = folder.to_owned();
        let (home, shep, kelpie) = (&self.home, &self.paths.shep_home, &self.paths.kelpie_home);
        let homes =
            crate::profile::home_denials([home.as_path(), shep.as_path(), kelpie.as_path()]);
        // Both layers let a deny inside a folder it reads win: the sandbox
        // emits it after the folder's allow, and Claude Code's denies win.
        let secrets = code.iter().flat_map(|tree| {
            (SECRET_SHAPED.iter()).map(move |file| format!("{}/{file}", tree.display()))
        });
        let no_read = (crate::profile::CREDENTIALS.iter())
            .chain(&[GH_CONFIG])
            .map(|p| (*p).to_owned())
            .chain(homes)
            .chain(secrets)
            .collect();
        Reach {
            // Outside its folder, so its file tools may read them under `-p`.
            read: code.to_vec(),
            fence: Some(Box::new(Fence {
                write,
                no_write: Vec::new(),
                no_read,
                // The guard's own binary, which these calls' hooks run inside
                // the fence, and exec needs file-read of the file it runs.
                read: [folder.clone(), self.kelpie.clone()]
                    .into_iter()
                    .chain(code.iter().cloned())
                    .collect(),
                hosts,
                no_commands: Vec::new(),
                env: BTreeMap::new(),
                sockets: Vec::new(),
                guard: Guard {
                    kelpie: self.kelpie.clone(),
                    worktree: folder.clone(),
                    build: folder.clone(),
                    git_common_dir: folder,
                    base: self.settings.git.base().to_owned(),
                    kelpie_pushes: false,
                    folders: vec![kelpie.clone(), self.settings.git.checkout.clone()],
                    hooks: Hooks::default(),
                },
                hooks: Vec::new(),
            })),
            block_keychain: false,
            code_graph: None,
            browser: None,
            mcp_servers: BTreeMap::new(),
        }
    }
}

// Whether `ruling` is a work item's over-budget ruling.
fn is_over_budget(ruling: &Ruling) -> bool {
    matches!(ruling.kind, RulingKind::Stuck(Stuck::OverBudget { .. }))
}

// An amount as dollars and cents.
fn dollars(cost: Cost) -> String {
    format!("${:.2}", cost.usd())
}

// The findings still open when the work item was parked, by severity, then
// the ones it holds and those its worker disputed.
fn open_findings(phase: &Phase, item: Option<&WorkItem>) -> String {
    let in_round = match phase {
        Phase::Review(review) => match &review.stage {
            ReviewStage::Found { findings, .. } => findings.as_slice(),
            ReviewStage::Fixing { sent, .. } => sent.as_slice(),
            ReviewStage::SecondLook { first } => first.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    let count = |severity: Severity| in_round.iter().filter(|f| f.severity == severity).count();
    let (held, disputed) = item.map_or((0, 0), |i| (i.held.len(), i.disputes.len()));
    format!(
        "{} high, {} medium, {} low in the round; {held} held; {disputed} disputed",
        count(Severity::High),
        count(Severity::Medium),
        count(Severity::Low)
    )
}

// What CI says of the work item's head.
fn ci(item: Option<&WorkItem>) -> String {
    let short = |head: &str| head.get(..7).unwrap_or(head).to_owned();
    let mut says = Vec::new();
    if let Some(item) = item {
        if let Some(head) = &item.red_head {
            says.push(format!("red at {}, sent to the worker", short(head)));
        }
        if let Some(head) = &item.reran {
            says.push(format!("failed jobs rerun at {}", short(head)));
        }
        if let Some(head) = &item.unchecked {
            says.push(format!("{} passed as skipped, with no checks", short(head)));
        }
    }
    match says.is_empty() {
        true => "not run on this head".to_owned(),
        false => says.join("; "),
    }
}
