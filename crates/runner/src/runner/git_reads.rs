//! The git reads a step makes with the runner's lock let go
//!
//! A fetch or a diff in the repo needs no write access to the worktree, and nor does the
//! worktree's making, whose answer a turn's preparation takes in place of its own. A step that
//! finds the read it needs missing returns it as a job (`turn/reads.rs`) and is made again with the
//! answer, which stays for its later asks. A read that fails reaches the step as the inline call
//! returned it. A rebase that holds the lock can contend for a ref lock with a fetch in flight,
//! and the fetch then fails with git's own error, which takes the step's error path. Read in
//! place where no pass runs to make the job.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::Runner;
use super::dev_server::install::{self, Installed};
use super::report::{Begin, StepReport};
use crate::fence;
use crate::ports::{TestRun, TestRunner};
use crate::ticket::TicketId;
use crate::work_item::WorkItem;
use crate::worktree::{self, Base, Remote, Start, Worktree, WorktreeError};

/// The head of a branch and the files it changes that a fetch found
type Changes = (String, Vec<String>);

/// What the pass has read from git for a step
#[derive(Debug, Default)]
pub(in crate::runner) struct Kept {
    head: Option<Result<String, WorktreeError>>,
    // By the head the base was read for, which a step that finds another reads again
    bases: BTreeMap<String, Result<Base, WorktreeError>>,
    // By the head the tips were read for, which a step that finds another reads again
    borrowed: BTreeMap<String, Result<Vec<String>, WorktreeError>>,
    changes: Option<Result<Changes, WorktreeError>>,
    prepared: Option<Result<Worktree, WorktreeError>>,
    installed: Option<Installed>,
}

impl Kept {
    /// Forgets what was read, as a pass begins or ends or a step ends
    pub(in crate::runner) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether nothing is kept
    #[cfg(test)]
    pub(in crate::runner) fn is_empty(&self) -> bool {
        self.head.is_none()
            && self.bases.is_empty()
            && self.borrowed.is_empty()
            && self.changes.is_none()
            && self.prepared.is_none()
            && self.installed.is_none()
    }

    /// Whether a job made the work item's worktree
    pub(in crate::runner) fn is_prepared(&self) -> bool {
        self.prepared.is_some()
    }

    /// What the install of the worktree's packages came to, once a job ran it
    pub(in crate::runner) fn installed(&self) -> Option<&Installed> {
        self.installed.as_ref()
    }

    /// Keeps what a job found for the step that asked
    pub(in crate::runner) fn keep(&mut self, found: Found) {
        match found {
            Found::Head(head) => self.head = Some(head),
            Found::Base { head, base } => {
                self.bases.insert(head, base);
            }
            Found::Borrowed { head, borrowed } => {
                self.borrowed.insert(head, borrowed);
            }
            Found::Changes(changes) => self.changes = Some(changes),
            Found::Prepared(prepared) => self.prepared = Some(prepared),
            Found::Installed(installed) => self.installed = Some(installed),
        }
    }
}

/// A git read, which the step asks for by [`Ask`] and the rotation runs with the lock let go
pub(in crate::runner) enum Git {
    Head(Head),
    Base(Standing),
    Borrowed(Borrowed),
    Changes(Changed),
    Prepare(Preparing),
    Install(Installing),
}

/// What a [`Git`] read found, errors as the inline call returns them
pub(in crate::runner) enum Found {
    Head(Result<String, WorktreeError>),
    Base {
        head: String,
        base: Result<Base, WorktreeError>,
    },
    Borrowed {
        head: String,
        borrowed: Result<Vec<String>, WorktreeError>,
    },
    Changes(Result<Changes, WorktreeError>),
    Prepared(Result<Worktree, WorktreeError>),
    Installed(Installed),
}

impl Git {
    /// Whether the read changes the work item's worktree, which a trigger on it then waits for
    pub(in crate::runner) fn changes_worktree(&self) -> bool {
        matches!(self, Self::Prepare(_) | Self::Install(_))
    }

    /// Reads, with the calls the step would have made inline
    pub(in crate::runner) fn run(self) -> Found {
        match self {
            Self::Head(ask) => Found::Head(ask.run()),
            Self::Base(ask) => {
                let head = ask.head.clone();
                Found::Base {
                    base: ask.run(),
                    head,
                }
            }
            Self::Borrowed(ask) => {
                let head = ask.head.clone();
                Found::Borrowed {
                    borrowed: ask.run(),
                    head,
                }
            }
            Self::Changes(ask) => Found::Changes(ask.run()),
            Self::Prepare(ask) => Found::Prepared(ask.run()),
            Self::Install(ask) => Found::Installed(ask.run()),
        }
    }
}

/// A read of git, made from owned values
pub(in crate::runner) trait Ask: Sized {
    type Answer;

    /// Reads, in place
    fn run(self) -> Self::Answer;

    /// The read as a job's
    fn job(self) -> Git;
}

/// The head of a branch on its remote, just fetched
pub(in crate::runner) struct Head {
    checkout: PathBuf,
    branch: String,
    remote: Remote,
}

impl Ask for Head {
    type Answer = Result<String, WorktreeError>;

    fn run(self) -> Self::Answer {
        worktree::origin_head(&self.checkout, &self.branch, &self.remote)
    }

    fn job(self) -> Git {
        Git::Head(self)
    }
}

/// Where the forge's `head` of a branch stands against its base, just fetched
pub(in crate::runner) struct Standing {
    checkout: PathBuf,
    branch: String,
    remote: Remote,
    head: String,
    base: String,
}

impl Ask for Standing {
    type Answer = Result<Base, WorktreeError>;

    fn run(self) -> Self::Answer {
        let Self {
            checkout,
            branch,
            remote,
            head,
            base,
        } = self;
        worktree::base_of(&checkout, &branch, &head, &remote, &base)
    }

    fn job(self) -> Git {
        Git::Base(self)
    }
}

/// The commits among `tips` that `head` carries and the base lacks, just fetched
pub(in crate::runner) struct Borrowed {
    checkout: PathBuf,
    head: String,
    base: String,
    tips: Vec<String>,
}

impl Ask for Borrowed {
    type Answer = Result<Vec<String>, WorktreeError>;

    fn run(self) -> Self::Answer {
        worktree::borrowed(&self.checkout, &self.head, &self.base, &self.tips)
    }

    fn job(self) -> Git {
        Git::Borrowed(self)
    }
}

/// The head of a branch, just fetched, and the agents' own files it changes
pub(in crate::runner) struct Changed {
    checkout: PathBuf,
    branch: String,
    remote: Remote,
    accepted: Option<String>,
    base: String,
}

impl Ask for Changed {
    type Answer = Result<Changes, WorktreeError>;

    fn run(self) -> Self::Answer {
        let accepted = self.accepted.as_deref();
        fence::changed(
            &self.checkout,
            &self.branch,
            &self.remote,
            accepted,
            &self.base,
        )
    }

    fn job(self) -> Git {
        Git::Changes(self)
    }
}

/// A work item's worktree and build folder, made ready from `start` after a fetch
pub(in crate::runner) struct Preparing {
    checkout: PathBuf,
    worktree: PathBuf,
    branch: String,
    start: Start,
    base: String,
    remote: Remote,
    build: PathBuf,
}

impl Ask for Preparing {
    type Answer = Result<Worktree, WorktreeError>;

    fn run(self) -> Self::Answer {
        worktree::prepare(
            &self.checkout,
            &self.worktree,
            &self.branch,
            self.start,
            &self.base,
            &self.remote,
            &self.build,
        )
    }

    fn job(self) -> Git {
        Git::Prepare(self)
    }
}

/// A work item's packages installed in the worker's sandbox, once its worktree is made
///
/// The worktree is made outside any sandbox, so the install follows it as a call of its own.
pub(in crate::runner) struct Installing {
    tests: Arc<dyn TestRunner>,
    run: TestRun,
    folders: Vec<String>,
}

impl Ask for Installing {
    type Answer = Installed;

    fn run(self) -> Self::Answer {
        Installed::of(self.folders, self.tests.run(&self.run))
    }

    fn job(self) -> Git {
        Git::Install(self)
    }
}

/// Where a worktree made for `item` starts: a rework or adopted branch is the one `origin` holds
pub(in crate::runner) fn start_of(item: &WorkItem) -> Start {
    if item.rework || item.adopted {
        Start::Pushed
    } else {
        Start::Main
    }
}

/// Whether a work item's branch or worktree holds work its base lacks, which fetches first
pub(in crate::runner) struct Tree {
    checkout: PathBuf,
    worktree: PathBuf,
    branch: String,
    base: String,
}

impl Tree {
    /// Asks git
    pub(in crate::runner) fn holds_work(&self) -> Result<bool, WorktreeError> {
        worktree::holds_work(&self.checkout, &self.worktree, &self.branch, &self.base)
    }
}

impl Runner {
    /// The head of the work item's branch on its remote, read in place
    ///
    /// Asks git rather than the forge: the forge's head lags a push by a moment.
    pub(in crate::runner) fn origin_head(&self) -> Result<String, String> {
        let item = self.current().ok_or("the work item is gone")?;
        worktree::origin_head(&self.settings.git.checkout, &item.branch, &item.remote)
            .map_err(|e| e.to_string())
    }

    /// The head of the work item's branch on its remote, as git reports it, errors as
    /// `origin_head` returns them, or the step's `Begin::Reads`
    ///
    /// Asks git rather than the forge: the forge's head lags a push by a moment.
    pub(in crate::runner) fn head_on_origin(&self) -> Result<Result<String, String>, Box<Begin>> {
        let ask = self.current().map(|item| Head {
            checkout: self.settings.git.checkout.clone(),
            branch: item.branch.clone(),
            remote: item.remote.clone(),
        });
        let read = self.read_git(self.reads.git.head.clone(), ask)?;
        Ok(read.map_err(|e| e.to_string()))
    }

    /// The head of the work item's branch on its remote, or the step's end: `Begin::Reads`, or
    /// the gate failing as git could not say
    pub(in crate::runner) fn pushed_head(&self) -> Result<String, Box<Begin>> {
        self.head_on_origin()?
            .map_err(|reason| Box::new(self.gate_failed(reason)))
    }

    /// Where `head`, the forge's head of the work item's branch, stands against its base, or the
    /// step's end: `Begin::Reads`, or the gate failing as git could not say
    ///
    /// A base read for another head is not this one's: it is read again.
    pub(in crate::runner) fn base_standing(&self, head: &str) -> Result<Base, Box<Begin>> {
        let ask = self.current().map(|item| Standing {
            checkout: self.settings.git.checkout.clone(),
            branch: item.branch.clone(),
            remote: item.remote.clone(),
            head: head.to_owned(),
            base: self.item_base(item).to_owned(),
        });
        let read = self.read_git(self.reads.git.bases.get(head).cloned(), ask)?;
        read.map_err(|e| Box::new(self.gate_failed(format!("cannot fetch main: {e}"))))
    }

    /// The commits among `tips` that `head`, the forge's head of the work item's branch, carries
    /// and its base lacks, or the step's end: `Begin::Reads`, or the gate failing as git could
    /// not say
    ///
    /// Tips read for another head are not this one's: they are read again.
    pub(in crate::runner) fn tips_carried(
        &self,
        head: &str,
        tips: Vec<String>,
    ) -> Result<Vec<String>, Box<Begin>> {
        let ask = self.current().map(|item| Borrowed {
            checkout: self.settings.git.checkout.clone(),
            head: head.to_owned(),
            base: self.item_base(item).to_owned(),
            tips,
        });
        let read = self.read_git(self.reads.git.borrowed.get(head).cloned(), ask)?;
        read.map_err(|e| {
            let why = format!("cannot tell which pull requests this head carries: {e}");
            Box::new(self.gate_failed(why))
        })
    }

    /// The branch's head on its remote and the agents' own files it changes, or the step's
    /// `Begin::Reads`
    pub(in crate::runner) fn agent_files_read(
        &self,
    ) -> Result<Result<Changes, WorktreeError>, Box<Begin>> {
        let ask = self.current().map(|item| Changed {
            checkout: self.settings.git.checkout.clone(),
            branch: item.branch.clone(),
            remote: item.remote.clone(),
            accepted: item.claude_files_accepted.clone(),
            base: self.item_base(item).to_owned(),
        });
        self.read_git(self.reads.git.changes.clone(), ask)
    }

    /// What was kept, else the job that reads it, else the read made in place where no pass runs
    /// to make the job and step again. An `ask` of none is a work item that is gone.
    fn read_git<A: Ask>(
        &self,
        kept: Option<A::Answer>,
        ask: Option<A>,
    ) -> Result<A::Answer, Box<Begin>> {
        if let Some(kept) = kept {
            return Ok(kept);
        }
        let Some(ask) = ask else {
            return Err(Box::new(Begin::Idle));
        };
        if !self.snapshot.in_pass() {
            return Ok(ask.run());
        }
        Err(Box::new(match self.git_job(ask.job()) {
            Some(job) => Begin::Reads(job),
            None => Begin::Idle,
        }))
    }

    /// The job that makes the focused work item's worktree, or none when the step's earlier ask
    /// kept it, no pass runs to make it, or the work item is gone
    pub(in crate::runner) fn preparing(&self) -> Option<Begin> {
        let item = self
            .current()
            .filter(|_| self.reads.git.prepared.is_none())
            // A turn that fails before it asks for the worktree makes none.
            .filter(|item| {
                !matches!(self.reads.started(item.issue).token(), Some(Err(_)))
                    && self.worker_agent(item).is_ok()
            })?;
        let job = self.git_job(self.prepare_ask(item, start_of(item)).job())?;
        Some(Begin::Reads(job))
    }

    /// The job that installs the packages of the focused work item's worktree in the worker's
    /// sandbox, or none when the worktree needs none, the step's earlier ask kept the answer, an
    /// earlier install ran, no pass runs to make it, or the worktree is not made
    ///
    /// The install follows the worktree's making, which the step's `preparing` job kept.
    pub(in crate::runner) fn installing(&self) -> Option<Begin> {
        let item = self.current().filter(|item| {
            self.reads.git.installed.is_none() && !self.install_ran.contains(&item.issue)
        })?;
        if !matches!(self.reads.git.prepared, Some(Ok(_))) {
            return None;
        }
        let plan = install::worktree(&item.worktree, &item.build)?;
        // The token the turn's start read; a turn that failed to get one fails in its call.
        let Some(Ok(token)) = self.reads.started(item.issue).token() else {
            return None;
        };
        let mut reach = self
            .worker_reach(item, start_of(item), token.as_ref())
            .ok()?;
        // The caches go in the build folder, unless the worker's own `build_env` named them.
        let fence = reach.fence.as_mut()?;
        for (variable, folder) in plan.env {
            fence.env.entry(variable).or_insert(folder);
        }
        let run = TestRun {
            command: plan.command,
            cwd: item.worktree.clone(),
            reach,
            settings: (self.paths.worker).join(format!("install-{}.sandbox.json", item.issue)),
            door: None,
            ceiling: install::INSTALL_WITHIN,
        };
        let job = self.git_job(Git::Install(Installing {
            tests: Arc::clone(&self.ports.test_runner),
            run,
            folders: plan.folders,
        }))?;
        Some(Begin::Reads(job))
    }

    /// Logs the install the step's job ran, and keeps it from being run again
    pub(in crate::runner) fn packages_installed(&mut self, issue: TicketId) {
        let Some(installed) = self.reads.git.installed().cloned() else {
            return;
        };
        self.install_ran.insert(issue);
        self.started.push(StepReport::PackagesInstalled {
            issue,
            folders: installed.folders,
            failed: installed.failed,
        });
    }

    /// The worktree of `item` made ready: what the step's job found, else made in place
    pub(in crate::runner) fn worktree_made(
        &self,
        item: &WorkItem,
        start: Start,
    ) -> Result<Worktree, WorktreeError> {
        let kept = self.reads.git.prepared.clone();
        kept.unwrap_or_else(|| self.prepare_ask(item, start).run())
    }

    fn prepare_ask(&self, item: &WorkItem, start: Start) -> Preparing {
        Preparing {
            checkout: self.settings.git.checkout.clone(),
            worktree: item.worktree.clone(),
            branch: item.branch.clone(),
            start,
            base: self.item_base(item).to_owned(),
            remote: item.remote.clone(),
            build: item.build.clone(),
        }
    }

    /// The check of whether `ticket`'s work item holds work, or none when it is gone
    pub(in crate::runner) fn tree_to_check(&self, ticket: TicketId) -> Option<Tree> {
        let item = self.state.item(ticket)?;
        Some(Tree {
            checkout: self.settings.git.checkout.clone(),
            worktree: item.worktree.clone(),
            branch: item.branch.clone(),
            base: self.item_base(item).to_owned(),
        })
    }
}
