//! `shep kelpie upgrade`: installs a new kelpie build and restarts kelpie onto it
//!
//! Kelpie's sheep all run through one link, `bin/kelpie` under kelpie's home,
//! which points at a versioned file in `builds/`. An upgrade makes the new
//! build's file, relinks, then restarts the dog and each runner in turn,
//! never a runner with a merge in flight. The link it replaced stays as
//! `bin/kelpie.previous` for `--rollback`.
//!
//! An upgrade never moves the shepherd. A build for another shep minor than
//! the shepherd runs would restart into a refusal, so it stops first and
//! says what to run.

mod fleet;
mod host;
pub mod install;

#[cfg(test)]
mod tests;

pub use host::main;

use std::path::{Path, PathBuf};
use std::time::Duration;

use install::{Install, Lock};

use crate::shepherd::release_line;

/// How often a runner with a merge in flight is asked again
const MERGE_POLL: Duration = Duration::from_secs(5);

/// How long a merge in flight is waited for, before the upgrade stops
const MERGE_WAIT: Duration = Duration::from_secs(30 * 60);

/// How often a restarted sheep is asked whether it answers
const UP_POLL: Duration = Duration::from_secs(1);

/// How long a restarted sheep has to answer: a runner needs about 7s to
/// stop and up to 30s to open its channel
const UP_WAIT: Duration = Duration::from_secs(90);

/// Where a new build comes from
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A release of kelpie, built from its tag
    Release(String),
    /// A branch, tag or commit of kelpie, built from source
    Ref(String),
    /// A build made elsewhere, such as the new build itself finishing an
    /// upgrade across a shep minor
    Binary(PathBuf),
}

/// What `upgrade` was asked to do
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Install a build from a source, and restart onto it
    Install(Source),
    /// Put the previous build back, and restart onto it
    Rollback,
}

/// What an upgrade asks of the machine it runs on
pub trait Machine {
    /// Builds `source` in `staging`, returning the binary
    ///
    /// # Errors
    ///
    /// A message when the build fails.
    fn build(&self, source: &Source, staging: &Path) -> Result<PathBuf, String>;

    /// The shep version `binary` is built for
    ///
    /// # Errors
    ///
    /// A message when `binary` does not run or does not say.
    fn shep_version(&self, binary: &Path) -> Result<String, String>;

    /// Signs `binary` for the platform, which only macOS asks for
    ///
    /// # Errors
    ///
    /// A message when signing fails.
    fn sign(&self, binary: &Path) -> Result<(), String>;

    /// Whether the process `pid` is running
    fn alive(&self, pid: u32) -> bool;

    /// Waits for `time`
    fn sleep(&self, time: Duration);
}

/// What a kelpie sheep is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The adopted dog that holds the leases
    Dog,
    /// A project's runner
    Runner,
}

/// One of kelpie's sheep, as the shepherd lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The sheep's name
    pub name: String,
    /// Whether it is the dog or a runner
    pub kind: Kind,
    /// The program the shepherd starts it with
    pub program: PathBuf,
    /// Whether it is running
    pub online: bool,
}

/// What an upgrade asks of kelpie's shepherd
pub trait Fleet {
    /// The shep version the shepherd runs
    ///
    /// # Errors
    ///
    /// A message when the shepherd cannot be reached.
    fn shepherd_version(&self) -> Result<String, String>;

    /// The dog and every runner, in the order they are restarted
    ///
    /// # Errors
    ///
    /// A message when the shepherd cannot list them.
    fn members(&self) -> Result<Vec<Member>, String>;

    /// Whether the runner `name` has a merge in flight
    ///
    /// # Errors
    ///
    /// A message when the runner's status cannot be read.
    fn merging(&self, name: &str) -> Result<bool, String>;

    /// Restarts `member`
    ///
    /// # Errors
    ///
    /// A message when the shepherd refuses.
    fn restart(&self, member: &Member) -> Result<(), String>;

    /// Whether `member` is up and answering
    ///
    /// # Errors
    ///
    /// A message when the shepherd cannot say.
    fn up(&self, member: &Member) -> Result<bool, String>;
}

/// Runs `job`, as the process `pid`, and says what it did
///
/// # Errors
///
/// A message when another upgrade runs, a sheep does not run through the
/// link, the build is for another shep minor than the shepherd, or a
/// restart does not come up. Nothing restarts before the first three.
pub fn run(
    job: &Job,
    install: &Install,
    pid: u32,
    machine: &dyn Machine,
    fleet: &dyn Fleet,
) -> Result<Vec<String>, String> {
    let _lock = Lock::take(install, pid, |holder| machine.alive(holder))?;
    let current = install.current()?;
    let members = fleet.members()?;
    through_the_link(&members, &install.link())?;
    let shepherd = fleet.shepherd_version()?;
    let mut lines = Vec::new();
    let build = match job {
        Job::Rollback => install.before()?,
        Job::Install(source) => {
            let build = stage(source, install, machine)?;
            lines.push(format!("kept the build as {}", build.display()));
            build
        }
    };
    let built = machine.shep_version(&build).map_err(|why| {
        format!(
            "{} did not say which shep it is built for ({why}), so it is too old to install",
            build.display()
        )
    })?;
    if release_line(&built) != release_line(&shepherd) {
        return Err(across_a_minor(&build, &built, &shepherd));
    }
    if build == current {
        lines.push(format!(
            "{} is already the installed build",
            build.display()
        ));
        return Ok(lines);
    }
    install.relink(&build)?;
    lines.push(format!(
        "linked {} to {}",
        install.link().display(),
        build.display()
    ));
    lines.extend(restart(&members, machine, fleet, &build)?);
    Ok(lines)
}

// The build `source` makes, kept in the builds folder and signed.
fn stage(source: &Source, install: &Install, machine: &dyn Machine) -> Result<PathBuf, String> {
    let (binary, label) = match source {
        Source::Binary(path) => {
            runnable(path)?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("binary");
            (path.clone(), name.to_owned())
        }
        Source::Release(name) | Source::Ref(name) => {
            (machine.build(source, &install.staging())?, name.clone())
        }
    };
    let label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let (kept, fresh) = install.keep(&binary, &label)?;
    // A build already kept may be running, and signing rewrites the file.
    if fresh {
        machine.sign(&kept)?;
    }
    Ok(kept)
}

// A file that can be run, which the upgrade will not make of one that is not.
fn runnable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "{} is not executable: run `chmod +x {}` and upgrade again",
            path.display(),
            path.display()
        ));
    }
    Ok(())
}

// Refuses a sheep started by any program but the link, which a relink
// would never reach.
fn through_the_link(members: &[Member], link: &Path) -> Result<(), String> {
    match members.iter().find(|m| m.program != link) {
        None => Ok(()),
        Some(member) => Err(format!(
            "{} runs {}, not {}, so a relink would never reach it: point it at the link, or \
             remove it, before upgrading",
            member.name,
            member.program.display(),
            link.display()
        )),
    }
}

fn across_a_minor(build: &Path, built: &str, shepherd: &str) -> String {
    let path = build.display();
    format!(
        "{path} is built for shep {built} and the shepherd runs shep {shepherd}, so restarting \
         would end in a refusal. Nothing was relinked or restarted. To move across the minor: \
         upgrade shep and restart its shepherd, then run `{path} upgrade --binary {path}`. \
         Kelpie's sheep may sit errored until then."
    )
}

// The dog, then each runner, one at a time: a runner waits out its merge
// first, and the next one waits for this one to answer.
fn restart(
    members: &[Member],
    machine: &dyn Machine,
    fleet: &dyn Fleet,
    build: &Path,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut done: Vec<&str> = Vec::new();
    for (at, member) in members.iter().enumerate() {
        if !member.online {
            lines.push(format!(
                "{} is not running, so it takes the new build when it starts",
                member.name
            ));
            continue;
        }
        let stopped = |why: String, restarted: &[&str], left: &[Member]| {
            let left: Vec<&str> = left.iter().map(|m| m.name.as_str()).collect();
            format!(
                "{why}. Restarted: {}. Still on the old build: {}. The link points at {}, and \
                 `shep kelpie upgrade --rollback` puts the previous build back.",
                list(restarted),
                list(&left),
                build.display()
            )
        };
        if member.kind == Kind::Runner {
            wait_out_merge(member, machine, fleet)
                .map_err(|why| stopped(why, &done, &members[at..]))?;
        }
        fleet
            .restart(member)
            .map_err(|why| stopped(why, &done, &members[at..]))?;
        done.push(&member.name);
        wait_up(member, machine, fleet).map_err(|why| stopped(why, &done, &members[at + 1..]))?;
        lines.push(format!("restarted {}", member.name));
    }
    Ok(lines)
}

fn list(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

fn wait_out_merge(member: &Member, machine: &dyn Machine, fleet: &dyn Fleet) -> Result<(), String> {
    let mut waited = Duration::ZERO;
    while fleet.merging(&member.name)? {
        if waited >= MERGE_WAIT {
            return Err(format!(
                "{} still has a merge in flight after {} minutes",
                member.name,
                MERGE_WAIT.as_secs() / 60
            ));
        }
        machine.sleep(MERGE_POLL);
        waited += MERGE_POLL;
    }
    Ok(())
}

fn wait_up(member: &Member, machine: &dyn Machine, fleet: &dyn Fleet) -> Result<(), String> {
    let mut waited = Duration::ZERO;
    while !fleet.up(member)? {
        if waited >= UP_WAIT {
            return Err(format!(
                "{} did not answer in {} seconds after its restart: `shep bleats {}` says why",
                member.name,
                UP_WAIT.as_secs(),
                member.name
            ));
        }
        machine.sleep(UP_POLL);
        waited += UP_POLL;
    }
    Ok(())
}

/// Reads `upgrade`'s arguments
///
/// # Errors
///
/// The usage when they are not exactly one of `--release <tag>`,
/// `--ref <ref>`, `--binary <path>` or `--rollback`.
pub fn parse(args: &[String]) -> Result<Job, String> {
    match args {
        [flag] if flag == "--rollback" => Ok(Job::Rollback),
        [flag, value] if !value.is_empty() && !value.starts_with('-') => match flag.as_str() {
            "--release" => Ok(Job::Install(Source::Release(value.clone()))),
            "--ref" => Ok(Job::Install(Source::Ref(value.clone()))),
            "--binary" => Ok(Job::Install(Source::Binary(PathBuf::from(value)))),
            _ => Err(USAGE.to_owned()),
        },
        _ => Err(USAGE.to_owned()),
    }
}

/// What `upgrade` takes, as its help says
pub const USAGE: &str = "\
usage: shep kelpie upgrade --release <tag>   builds a release, then restarts kelpie onto it
       shep kelpie upgrade --ref <ref>       builds a branch, tag or commit
       shep kelpie upgrade --binary <path>   installs a build made elsewhere
       shep kelpie upgrade --rollback        puts the previous build back";
