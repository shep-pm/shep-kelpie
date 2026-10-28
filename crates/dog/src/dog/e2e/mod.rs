//! A worker's e2e run: placed by its worktree, queued per host, run on the host
//!
//! The ask names only a working folder. The dog works out the project from
//! where that folder is, and takes the host, command and results folder from
//! the project's `e2e.json`, never from anything the ask says, so a worker
//! cannot point a run anywhere its project's settings do not. A host holds one
//! run at a time, and a run is copy, command, copy back: see [`run`].

use std::borrow::Cow;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::Duration;

use tokio::io::BufReader;
use tokio::sync::{mpsc, oneshot};

use super::desk::Desk;
use super::door::{Place, heard, say};
use super::lock_desk;
use crate::home::{ProjectName, ProjectPaths};
use crate::ipc::{ServerReadHalf, ServerWriteHalf};
use crate::lease::counted::{CountedLease, Taken, Taker, Ticket};
use crate::lease::door::{Answer, Ask, E2E, NO_E2E_HOST, TIMED_OUT};
use crate::settings::E2e;

mod run;
#[cfg(test)]
pub(super) mod stand_in;

/// How long a run may take, from the grant, before the dog ends it
pub(super) const CEILING: Duration = Duration::from_secs(30 * 60);

/// How many commands hold a host's lease at once
const HOST_CAPACITY: NonZeroU32 = NonZeroU32::new(1).expect("1 is not zero");

/// How many lines of output wait for the worker to read them before the
/// command's output is held back
const OUTPUT_WAITING: usize = 256;

/// The two programs the dog starts for a run, and nothing else: ssh and rsync
#[derive(Debug, Clone)]
pub(super) struct Remote {
    pub(super) ssh: PathBuf,
    pub(super) rsync: PathBuf,
}

impl Default for Remote {
    /// The two on the `PATH`
    fn default() -> Self {
        Self {
            ssh: PathBuf::from("ssh"),
            rsync: PathBuf::from("rsync"),
        }
    }
}

/// Where a run goes and what it runs, as the project's `e2e.json` says
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    host: String,
    project: ProjectName,
    // The root of the worktree that asked, never a folder inside it.
    worktree: PathBuf,
    command: PathBuf,
    results: PathBuf,
}

impl Plan {
    /// The plan for the project whose worktree holds `worktree`
    ///
    /// # Errors
    ///
    /// What to tell the worker: the project has no host to run on, when
    /// `worktree` names none or the project has no `e2e.json` for its runner
    /// to have recorded, or why the recorded table cannot be used.
    pub(super) fn read(kelpie_home: &Path, worktree: Option<&Path>) -> Result<Self, String> {
        let no_host = || {
            format!(
                "{NO_E2E_HOST}: the project's settings name none in [app.dogs.kelpie.e2e], or \
                 the path asked from is not one of its worktrees"
            )
        };
        let (project, worktree) = worktree
            .and_then(|worktree| locate(kelpie_home, worktree))
            .ok_or_else(no_host)?;
        let file = ProjectPaths::under(kelpie_home, kelpie_home, &project).e2e();
        let recorded = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(no_host()),
            Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
        };
        let e2e: E2e = serde_json::from_slice(&recorded)
            .map_err(|e| format!("cannot use {}: {e}", file.display()))?;
        let plan = Self {
            host: e2e.host.as_str().to_owned(),
            project,
            worktree,
            command: e2e.command.as_path().to_owned(),
            results: e2e.results.as_path().to_owned(),
        };
        // Both reach the host in rsync's arguments, which an old rsync hands
        // to the host's shell, so only plain names go.
        for (what, path) in [("command", &plan.command), ("results", &plan.results)] {
            let plain = path.to_str().is_some_and(|p| {
                p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "./_-".contains(c))
            });
            if !plain {
                return Err(format!(
                    "the project's e2e {what} {} has a character the copy to the host cannot \
                     carry: use letters, digits and ./_-",
                    path.display()
                ));
            }
        }
        Ok(plan)
    }

    // Where the worktree is copied to on the host, from the account's home.
    fn remote_dir(&self) -> String {
        format!("kelpie-e2e/{}", self.project)
    }

    // Where the results land before they go into the worktree: a folder in
    // the project's own, which the worker's fence cannot write.
    fn staging(&self) -> Result<PathBuf, String> {
        let project_folder = self.worktree.parent().and_then(Path::parent);
        let name = self.worktree.file_name();
        match (project_folder, name) {
            (Some(folder), Some(name)) => {
                Ok(folder.join(format!("e2e-back-{}", name.to_string_lossy())))
            }
            _ => Err(format!("{} is not a worktree", self.worktree.display())),
        }
    }
}

impl Desk {
    /// Asks for the e2e lease of `host` on behalf of `taker`
    pub(super) fn take_e2e(&mut self, host: &str, taker: Taker) -> Taken {
        let now = self.book.now();
        let lease = self.e2e.entry(host.to_owned()).or_insert_with(|| {
            CountedLease::named(Cow::Owned(format!("{E2E}:{host}")), HOST_CAPACITY)
        });
        let taken = lease.ask(taker, now);
        self.look(true);
        taken
    }

    /// Gives `host`'s e2e lease back, or leaves its queue, for the connection
    /// `ticket`, and returns the waiters granted after it
    pub(super) fn leave_e2e(&mut self, host: &str, ticket: Ticket) -> Vec<Ticket> {
        let now = self.book.now();
        let granted =
            (self.e2e.get_mut(host)).map_or_else(Vec::new, |lease| lease.leave(ticket, now));
        self.look(true);
        granted
    }
}

/// Answers one `e2e` ask from `place`, which has read it
///
/// It queues for its host's lease, then runs, streaming the command's output
/// and ending with its exit code. A worker that hangs up, or a run past
/// [`CEILING`], ends the run: the programs it started are killed and the
/// lease goes back with the connection's [`Place`].
pub(super) async fn visit(
    place: &mut Place,
    ask: Ask,
    reader: &mut BufReader<ServerReadHalf>,
    write: &mut ServerWriteHalf,
) {
    let plan = match Plan::read(&place.kelpie_home, ask.worktree.as_deref()) {
        Ok(plan) => plan,
        Err(why) => return say(write, &Answer::Error(why)).await,
    };
    let (wake, woken) = oneshot::channel();
    let taker = Taker {
        ticket: place.ticket,
        pid: ask.pid,
        what: ask.what,
    };
    let pid = taker.pid;
    // The waiter is listed before it asks, so a grant cannot miss it.
    place.list_waiter(wake);
    place.e2e_host = Some(plan.host.clone());
    let taken = lock_desk(&place.desk).desk.take_e2e(&plan.host, taker);
    if let Taken::Queued { ahead } = taken {
        say(write, &Answer::Queued(ahead)).await;
        tokio::select! {
            _ = woken => {}
            // Anything but a grant first, even a line, ends the wait.
            _ = heard(reader) => return,
        }
    }
    let line = format!(
        "{E2E}: granted to pid {pid} for {} on {}",
        plan.project, plan.host
    );
    if let Some(line) = super::door::granted_after_wait(taken, line) {
        println!("{line}");
    }
    say(write, &Answer::Granted).await;

    let (lines, mut output) = mpsc::channel(OUTPUT_WAITING);
    let mut ran = Box::pin(tokio::time::timeout(
        CEILING,
        run::run(&place.remote, &plan, lines),
    ));
    // The worker sends nothing after its ask, so the connection's end is
    // the worker going away.
    let mut hung = pin!(async { while heard(reader).await {} });
    let ended = loop {
        tokio::select! {
            Some(line) = output.recv() => say(write, &Answer::Output(line)).await,
            ended = &mut ran => break ended,
            () = &mut hung => {
                println!("{E2E}: pid {pid} hung up on its run on {}", plan.host);
                return;
            }
        }
    };
    // The run's programs are killed with it, and its end closes the lines.
    drop(ran);
    while let Some(line) = output.recv().await {
        say(write, &Answer::Output(line)).await;
    }
    let last = match ended {
        Ok(Ok(code)) => Answer::Done(code),
        Ok(Err(why)) => Answer::Error(why),
        Err(_) => {
            let ended = format!("e2e: ended after {} minutes", CEILING.as_secs() / 60);
            say(write, &Answer::Output(ended)).await;
            Answer::Done(TIMED_OUT)
        }
    };
    println!("{E2E}: pid {pid}'s run on {} ended: {last:?}", plan.host);
    say(write, &last).await;
}

/// The project whose `worktrees` folder holds `worktree`, and the root of the
/// worktree it is in, once links are resolved, or none for any other path
///
/// A path that names the `worktrees` folder itself, one that does not exist,
/// and one that only reaches a worktree through `..` or a link out of it
/// name no project.
fn locate(kelpie_home: &Path, worktree: &Path) -> Option<(ProjectName, PathBuf)> {
    let worktree = crate::canonical(worktree).ok()?;
    std::fs::read_dir(kelpie_home)
        .ok()?
        .flatten()
        .find_map(|entry| {
            let name = ProjectName::try_from(entry.file_name().to_str()?).ok()?;
            // Only the project's folder under kelpie's home is read here.
            let worktrees = ProjectPaths::under(kelpie_home, kelpie_home, &name).worktrees();
            let worktrees = crate::canonical(&worktrees).ok()?;
            let inside = worktree.strip_prefix(&worktrees).ok()?;
            let root = worktrees.join(inside.components().next()?);
            Some((name, root))
        })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::*;

    // A kelpie home holding koji's worktrees 7 and 8 and golbat's 3, and a
    // folder outside it.
    fn home() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let home = base.join("kelpie");
        for worktree in [
            "koji/worktrees/7/src",
            "koji/worktrees/8",
            "golbat/worktrees/3",
        ] {
            std::fs::create_dir_all(home.join(worktree)).unwrap();
        }
        std::fs::create_dir_all(home.join("agents")).unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        (dir, home, outside)
    }

    fn named(home: &Path, worktree: &str) -> Option<String> {
        locate(home, &home.join(worktree)).map(|(name, _)| name.to_string())
    }

    #[test]
    fn a_worktree_names_its_project() {
        let (_dir, home, _outside) = home();
        assert_eq!(named(&home, "koji/worktrees/7").as_deref(), Some("koji"));
        assert_eq!(named(&home, "koji/worktrees/8").as_deref(), Some("koji"));
        assert_eq!(
            named(&home, "golbat/worktrees/3").as_deref(),
            Some("golbat")
        );
        // A worker may run it from a folder inside its worktree, and the
        // worktree is still the one that holds it.
        assert_eq!(
            named(&home, "koji/worktrees/7/src").as_deref(),
            Some("koji")
        );
        let (_, root) = locate(&home, &home.join("koji/worktrees/7/src")).unwrap();
        assert_eq!(root, home.join("koji/worktrees/7"));
    }

    #[test]
    #[cfg(unix)]
    fn a_link_to_a_worktree_is_resolved_first() {
        let (_dir, home, _outside) = home();
        let link = home.join("golbat/here");
        symlink(home.join("koji/worktrees/8"), &link).unwrap();
        assert_eq!(locate(&home, &link).unwrap().0.as_str(), "koji");
    }

    #[test]
    fn a_path_outside_every_worktrees_folder_names_none() {
        let (_dir, home, outside) = home();
        let worktree = home.join("koji/worktrees/7");
        let rises = worktree.join("../../../agents");
        let sideways = worktree.join("../../../golbat/worktrees/../state.json");
        let none = [
            rises,
            sideways,
            home.join("koji/worktrees"),
            home.join("koji"),
            home.join("agents"),
            home.join("koji/worktrees/9"),
            home.clone(),
            outside,
            PathBuf::from("/"),
            PathBuf::new(),
        ];
        for path in none {
            assert_eq!(locate(&home, &path), None, "{}", path.display());
        }
    }
    #[test]
    #[cfg(unix)]
    fn a_link_out_of_every_worktrees_folder_names_none() {
        let (_dir, home, outside) = home();
        let worktree = home.join("koji/worktrees/7");
        symlink(&outside, worktree.join("out")).unwrap();
        symlink(&outside, home.join("koji/worktrees/escape")).unwrap();
        for path in [worktree.join("out"), home.join("koji/worktrees/escape")] {
            assert_eq!(locate(&home, &path), None, "{}", path.display());
        }
    }
}
