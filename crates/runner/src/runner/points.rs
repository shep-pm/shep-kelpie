//! The project's hooks at the points of kelpie's own work item loop
//!
//! `turn_end` is a gate, run here when a worker's turn ends. `ruling`, `ci`
//! and `done` are notices: the runner hands them to a thread of their own,
//! which logs a non-zero exit or a hang with the hook's name, so the runner
//! goes on without waiting. `turn_start` and `reset` run on
//! the thread of the call they come before, in [`super::turn_hooks`]. All run
//! in the runner, outside any sandbox, so they read what the runner can.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use serde_json::{Value, json};

use super::Runner;
use super::claim::Notes;
use super::words::Wants;
use crate::hooks::{Hooks, Role};
use crate::ports::Timestamp;
use crate::settings::HookPoint;
use crate::state::{Finished, ProjectState, Ruling};
use crate::ticket::TicketId;
use crate::work_item::{Phase, WorkItem};

/// What the notice points have told already, in memory only
///
/// A restart tells a head's CI once more, and no ruling or work item again:
/// the rulings it finds are taken as told already.
#[derive(Debug, Default)]
pub(super) struct Told {
    // The rulings told to the `ruling` hooks, by id and when each was raised:
    // a work item's number names every ruling it raises in turn
    rulings: BTreeSet<(TicketId, Option<Timestamp>)>,
    // The head each work item's CI last settled on, told to the `ci` hooks
    ci: BTreeMap<TicketId, String>,
    // The thread the notice hooks run on
    notices: Notices,
}

impl Told {
    /// What a runner that opens on `state` has told: every ruling it holds
    pub(super) fn of(state: &ProjectState) -> Self {
        Self {
            rulings: state.rulings.iter().map(|r| (r.id, r.raised)).collect(),
            ci: BTreeMap::new(),
            notices: Notices::default(),
        }
    }
}

// One notice to run, or in a test a mark to say the ones before it have.
enum Job {
    Tell {
        hooks: Hooks,
        notes: Notes,
        point: HookPoint,
        role: Role,
        details: Value,
    },
    #[cfg(any(test, feature = "test-support"))]
    Settled(mpsc::Sender<()>),
}

impl Job {
    fn run(self) {
        match self {
            Self::Tell {
                hooks,
                notes,
                point,
                role,
                details,
            } => {
                if let Some(line) = hooks.notify(point, role, None, details) {
                    notes.push(line);
                }
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::Settled(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// The notice hooks' one thread, started with the first notice
///
/// A notice runs here and not under the runner's lock, so a hook that hangs
/// its 30 seconds holds up the notices behind it and nothing else. They run
/// in the order they were told.
#[derive(Debug, Default)]
struct Notices {
    queue: OnceLock<mpsc::Sender<Job>>,
    // The notices handed over and not yet run to their end
    open: Arc<AtomicUsize>,
}

impl Notices {
    // Hands `job` over, or gives it back when no thread can take it.
    fn send(&self, job: Job) -> Result<(), Box<Job>> {
        let queue = self.queue.get_or_init(|| {
            let (sender, jobs) = mpsc::channel::<Job>();
            let open = Arc::clone(&self.open);
            let _ = std::thread::Builder::new()
                .name("kelpie-notices".to_owned())
                .spawn(move || {
                    for job in jobs {
                        job.run();
                        open.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            sender
        });
        self.open.fetch_add(1, Ordering::SeqCst);
        queue.send(job).map_err(|e| {
            self.open.fetch_sub(1, Ordering::SeqCst);
            Box::new(e.0)
        })
    }

    fn running(&self) -> bool {
        self.open.load(Ordering::SeqCst) > 0
    }

    #[cfg(any(test, feature = "test-support"))]
    fn settle(&self) {
        let (done, settled) = mpsc::channel();
        if self.send(Job::Settled(done)).is_ok() {
            let _ = settled.recv();
            // The thread counts the mark itself out just after it signals.
            while self.running() {
                std::thread::yield_now();
            }
        }
    }
}

/// The name of `phase` as `status` spells it
pub(super) fn phase_name(phase: &Phase) -> String {
    let phase = serde_json::to_value(phase).unwrap_or(Value::Null);
    phase["state"].as_str().unwrap_or_default().to_owned()
}

impl Runner {
    /// Runs the project's `turn_end` hooks on a worker's turn that ended
    /// with `last_message`, and says why one refused it, if one did
    pub(super) fn turn_end(&self, item: &WorkItem, last_message: &str) -> Option<String> {
        if !self.hooks.get().has(HookPoint::TurnEnd) {
            return None;
        }
        let details = json!({
            "issue": item.issue,
            "phase": phase_name(&item.phase),
            "last_message": last_message,
            "head": item.known.head,
        });
        let checked = self.hooks.get().check(
            HookPoint::TurnEnd,
            Role::Worker,
            Some(&item.worktree),
            details,
        );
        checked.err().map(|refusal| refusal.to_string())
    }

    /// Hands the project's notice hooks at `point` `details` to run, off
    /// the runner's lock, and logs the ones that did not exit 0
    pub fn tell_hooks(&self, point: HookPoint, role: Role, details: Value) {
        if !self.hooks.get().has(point) {
            return;
        }
        let job = Job::Tell {
            hooks: self.hooks.get().as_ref().clone(),
            notes: self.notes.clone(),
            point,
            role,
            details,
        };
        if let Err(job) = self.told_points.notices.send(job) {
            // No thread could start, so the notice runs here, as it would have.
            job.run();
        }
    }

    /// Whether a notice hook is running or waits its turn, on a thread a
    /// handover could not carry
    pub fn notices_running(&self) -> bool {
        self.told_points.notices.running()
    }

    /// Waits until every notice told so far has run
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn settle_notices(&self) {
        self.told_points.notices.settle();
    }

    /// Tells the `ruling` hooks each ruling raised since they last heard
    pub(super) fn tell_rulings(&mut self) {
        if !self.hooks.get().has(HookPoint::Ruling) {
            return;
        }
        let told = &mut self.told_points.rulings;
        let raised: Vec<Ruling> = (self.state.rulings.iter())
            .filter(|r| told.insert((r.id, r.raised)))
            .cloned()
            .collect();
        for ruling in raised {
            let kind = serde_json::to_value(&ruling.kind).unwrap_or(Value::Null);
            let choices: &[&str] = match Wants::of(&ruling.kind) {
                Wants::Answer => &[],
                Wants::Merge => &["yes", "no", "rework"],
                Wants::Trust | Wants::YesOrNo => &["yes", "no"],
            };
            let details = json!({
                "id": ruling.id,
                "issue": ruling.issue,
                "pull_request": ruling.pull_request,
                "kind": kind["kind"],
                "question": ruling.question,
                "choices": choices,
            });
            self.tell_hooks(HookPoint::Ruling, Role::Kelpie, details);
        }
    }

    /// Tells the `ci` hooks that CI settled on `head` of `issue`'s pull
    /// request `number` as `conclusion`, once for each head
    pub(super) fn ci_settled(
        &mut self,
        issue: TicketId,
        number: u64,
        head: &str,
        conclusion: &str,
        failed: &[String],
    ) {
        if !self.hooks.get().has(HookPoint::Ci) {
            return;
        }
        if self
            .told_points
            .ci
            .insert(issue, head.to_owned())
            .as_deref()
            == Some(head)
        {
            return;
        }
        let details = json!({
            "issue": issue,
            "pull_request": number,
            "head": head,
            "conclusion": conclusion,
            "failed": failed,
        });
        self.tell_hooks(HookPoint::Ci, Role::Kelpie, details);
    }

    /// Tells the `done` hooks that a work item ended, with its history entry
    pub(super) fn tell_done(&self, record: &Finished) {
        let details = json!({
            "issue": record.issue,
            "pull_request": record.pull_request,
            "merged": record.merged,
            "closed": record.closed,
            "history": record,
        });
        self.tell_hooks(HookPoint::Done, Role::Kelpie, details);
    }
}
