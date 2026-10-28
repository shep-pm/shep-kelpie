//! A project's name, and where its files live under kelpie's home

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::STATE_FILE;
use crate::ticket::TicketId;
use crate::tools::Tools;

/// A project's name, which is also its sheep's name
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProjectName(String);

impl ProjectName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for ProjectName {
    type Error = ProjectNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if value.is_empty() || value.starts_with(['-', '.']) || !value.chars().all(allowed) {
            return Err(ProjectNameError(value.to_owned()));
        }
        // A project's folder sits beside kelpie's own in kelpie's home.
        if crate::home::is_own(value) {
            return Err(ProjectNameError(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

// A name read back from the dog's book file passes the same check.
impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::try_from(name.as_str()).map_err(serde::de::Error::custom)
    }
}

/// A name that is not one plain path component, carrying the name
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNameError(pub String);

impl fmt::Display for ProjectNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} cannot name a project: use letters, digits, - _ . and do not start with - or ., \
             nor use one of kelpie's own folders ({}), as in `shep kelpie add <another name>`",
            self.0,
            crate::home::OWN.join(" ")
        )
    }
}

impl core::error::Error for ProjectNameError {}

/// The branch kelpie makes for the work item for `ticket`
pub fn work_branch(ticket: TicketId) -> String {
    format!("kelpie/{ticket}")
}

/// Where the runner of `project` answers: `<kelpie home>/run/<project>.sock`
///
/// Under `run/` and not the project's folder, whose path is longer than a
/// Unix socket's may be.
pub fn socket_of(kelpie_home: &Path, project: &str) -> PathBuf {
    kelpie_home.join("run").join(format!("{project}.sock"))
}

/// Where a project's files live under kelpie's home
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    /// The authenticator secret and the codes used, which every project shares
    pub totp: PathBuf,
    /// The agent files, which every project shares
    pub agents: PathBuf,
    /// The project's own folder, which a relative path in its settings is
    /// taken from
    pub folder: PathBuf,
    /// The state file
    pub state: PathBuf,
    /// The board briefing, `board.md`, which the project manager's agent reads
    pub board: PathBuf,
    /// The rulings answered while the runner was stopped, one file each,
    /// which it acts on when it starts
    pub answers: PathBuf,
    /// The project manager's folder: the board as it stood at its last wake,
    /// and its notes
    pub pm: PathBuf,
    /// The representative's folder, where its calls run with nothing in it
    pub representative: PathBuf,
    /// Every ruling answered, one JSON object a line, which the project manager's batch retro reads
    pub rulings: PathBuf,
    /// What happened to the work items, one JSON object a line, which the
    /// project manager's batch retro reads
    pub journal: PathBuf,
    /// The folder for the worker's settings file and instructions
    pub worker: PathBuf,
    /// The folder for the plugins that hold each step's skill
    pub skills: PathBuf,
    /// Where the project manager's batch retros are kept unread, one file
    /// each, which every project shares
    pub retro_reports: PathBuf,
    /// Kelpie's own tools for showing a work item's UI, which every project shares
    pub tools: Tools,
    /// Kelpie's home, which holds every folder here
    pub kelpie_home: PathBuf,
    /// The shepherd's home, which a worker may not read outside its own folders
    pub shep_home: PathBuf,
    /// The dog's door, the one socket a worker may connect to. Under
    /// kelpie's home unless the runner sets it from its own environment.
    pub door: PathBuf,
    /// The runner's own socket, `<kelpie home>/run/<project>.sock`, where it
    /// answers the typed protocol (ADR 0017)
    pub socket: PathBuf,
    worktrees: PathBuf,
    builds: PathBuf,
    graphs: PathBuf,
    graph_runtimes: PathBuf,
    browsers: PathBuf,
    triage: PathBuf,
}

impl ProjectPaths {
    /// `<kelpie home>/<project>/`, holding the project's state and worker
    /// files, and its `worktrees`, `builds`, `code-graphs`, `cbm` and `browser`
    /// folders, beside kelpie's own `totp`, `agents` and `tools`
    pub fn under(kelpie_home: &Path, shep_home: &Path, project: &ProjectName) -> Self {
        let folder = kelpie_home.join(project.as_str());
        Self {
            totp: kelpie_home.join("totp"),
            agents: kelpie_home.join(crate::agents::FOLDER),
            folder: folder.clone(),
            state: folder.join(STATE_FILE),
            board: folder.join("board.md"),
            answers: folder.join("answers"),
            pm: folder.join("pm"),
            representative: folder.join("representative"),
            rulings: folder.join("rulings.jsonl"),
            journal: folder.join("journal.jsonl"),
            worker: folder.join("worker"),
            skills: folder.join("skills"),
            retro_reports: kelpie_home.join("retro-reports/unverified"),
            tools: Tools::under(kelpie_home),
            kelpie_home: kelpie_home.to_owned(),
            shep_home: shep_home.to_owned(),
            door: kelpie_home.join("dog/lease.sock"),
            socket: socket_of(kelpie_home, project.as_str()),
            worktrees: folder.join("worktrees"),
            builds: folder.join("builds"),
            graphs: folder.join("code-graphs"),
            graph_runtimes: folder.join("cbm"),
            browsers: folder.join("browser"),
            triage: folder.join("triage"),
        }
    }

    /// The project's name, which names its folder
    pub fn name(&self) -> String {
        let name = self.folder.file_name();
        name.map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    /// Whether the runner's socket is short enough to bind
    ///
    /// Not part of [`sockets_fit`](Self::sockets_fit), which refuses a
    /// runner's start: a runner on a path too long goes on without its socket.
    ///
    /// # Errors
    ///
    /// A message naming the path that is too long.
    pub fn socket_fits(&self) -> Result<(), String> {
        crate::home::socket_fits(&self.socket)
    }

    /// Whether the door, and the longest socket a call opens (a 128-bit
    /// random name in the worker folder), are short enough to bind
    ///
    /// # Errors
    ///
    /// A message naming the path that is too long.
    pub fn sockets_fit(&self) -> Result<(), String> {
        crate::home::socket_fits(&self.door)?;
        crate::home::socket_fits(&self.worker.join(format!("{}.sock", "0".repeat(32))))
    }

    /// The folders kelpie owns for the project's work items: every worktree
    /// and every build folder
    pub fn owned(&self) -> [PathBuf; 2] {
        [self.worktrees.clone(), self.builds.clone()]
    }

    /// The project's `[app.dogs.kelpie.e2e]` table as the runner recorded it
    /// for the dog, which is not there while the project sets none
    pub fn e2e(&self) -> PathBuf {
        self.folder.join("e2e.json")
    }

    /// The project's local tickets, one markdown file each, which the project
    /// has only when its `tickets.source` is `local`
    pub fn tickets(&self) -> PathBuf {
        self.folder.join("tickets")
    }

    /// The folder holding every work item's worktree
    pub fn worktrees(&self) -> PathBuf {
        self.worktrees.clone()
    }

    /// The worktree for the work item for `ticket`
    pub fn worktree(&self, ticket: TicketId) -> PathBuf {
        self.worktrees.join(ticket.to_string())
    }

    /// The worktree a bump's head is checked out in for its e2e run, in the
    /// folder the dog finds the project by
    pub fn bump_worktree(&self, pull_request: u64) -> PathBuf {
        self.worktrees.join(format!("bump-{pull_request}"))
    }

    /// The build folder of a bump's browser proof: the dev server's log goes there
    pub fn bump_build(&self, pull_request: u64) -> PathBuf {
        self.builds.join(format!("bump-{pull_request}"))
    }

    /// The folder holding the code graph index of the view the triager reads
    /// for `ticket`, which goes when its call ends
    pub fn triage_graph(&self, ticket: TicketId) -> PathBuf {
        self.graphs.join(format!("triage-{ticket}"))
    }

    /// The folder the code graph daemon of the triager's view for `issue`
    /// binds its socket in, which goes when its call ends
    pub fn triage_graph_runtime(&self, ticket: TicketId) -> PathBuf {
        self.graph_runtimes.join(format!("triage-{ticket}"))
    }

    /// The folder holding the browser server's config and output for a bump's proof
    pub fn bump_browser(&self, pull_request: u64) -> PathBuf {
        self.browsers.join(format!("bump-{pull_request}"))
    }

    /// The build folder for the work item for `ticket`
    pub fn build(&self, ticket: TicketId) -> PathBuf {
        self.builds.join(ticket.to_string())
    }

    /// The folder holding the code graph index of the worktree that resolves
    /// `issue`, which goes when the worktree does
    pub fn code_graph(&self, ticket: TicketId) -> PathBuf {
        self.graphs.join(ticket.to_string())
    }

    /// The folder the code graph daemon of the worktree for `ticket`
    /// binds its socket in, which goes with the index. Short, as a socket's path must be.
    pub fn code_graph_runtime(&self, ticket: TicketId) -> PathBuf {
        self.graph_runtimes.join(ticket.to_string())
    }

    /// The folder holding the browser server's config and output for the work
    /// item for `ticket`, which the worker cannot write
    pub fn browser(&self, ticket: TicketId) -> PathBuf {
        self.browsers.join(ticket.to_string())
    }

    /// The view of `origin/main` the triager researches `ticket` in,
    /// which is removed when the call ends
    pub fn triage(&self, ticket: TicketId) -> PathBuf {
        self.triage.join(ticket.to_string())
    }

    /// The folder the images of the issue `ticket` are downloaded to for the
    /// triager to read, beside its view of `main` and outside the repo, which
    /// goes when the call ends
    pub fn triage_images(&self, ticket: TicketId) -> PathBuf {
        self.triage.join(format!("{ticket}-images"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_name_is_one_path_component() {
        for bad in [
            "", "a/b", "..", ".hidden", "-x", "-h", "--help", "sp ace", "dog", "tools",
        ] {
            assert!(ProjectName::try_from(bad).is_err(), "{bad:?}");
        }
        assert!(ProjectName::try_from("shep-kelpie_2.0").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_too_long_to_bind_is_named() {
        let koji = ProjectName::try_from("koji").unwrap();
        let fits = ProjectPaths::under(Path::new("/s/kelpie"), Path::new("/s"), &koji);
        assert_eq!(fits.sockets_fit(), Ok(()));
        let long = format!("/{}", "s".repeat(60));
        let home = Path::new(&long);
        let paths = ProjectPaths::under(&home.join("kelpie"), home, &koji);
        let error = paths.sockets_fit().unwrap_err();
        assert!(
            error.contains(&paths.worker.display().to_string()),
            "{error}"
        );
    }

    #[test]
    fn a_ticket_names_its_branch_and_folders() {
        let koji = ProjectName::try_from("koji").unwrap();
        let paths = ProjectPaths::under(Path::new("/s/kelpie"), Path::new("/s"), &koji);
        // id, then its branch, worktree and build
        let cases = [
            ("340", "kelpie/340", "worktrees/340", "builds/340"),
            ("k12", "kelpie/k12", "worktrees/k12", "builds/k12"),
        ];
        for (id, branch, worktree, build) in cases {
            let ticket: TicketId = id.parse().unwrap();
            assert_eq!(work_branch(ticket), branch, "{id}");
            let folder = Path::new("/s/kelpie/koji");
            assert_eq!(paths.worktree(ticket), folder.join(worktree), "{id}");
            assert_eq!(paths.build(ticket), folder.join(build), "{id}");
            assert_eq!(
                paths.browser(ticket),
                folder.join("browser").join(id),
                "{id}"
            );
            assert_eq!(
                paths.code_graph(ticket),
                folder.join("code-graphs").join(id),
                "{id}"
            );
            assert_eq!(
                paths.code_graph_runtime(ticket),
                folder.join("cbm").join(id),
                "{id}"
            );
            assert_eq!(paths.triage(ticket), folder.join("triage").join(id), "{id}");
            assert_eq!(
                paths.triage_images(ticket),
                folder.join("triage").join(format!("{id}-images")),
                "{id}"
            );
        }
    }

    #[test]
    fn the_runner_socket_sits_under_run_in_kelpie_s_home() {
        let koji = ProjectName::try_from("koji").unwrap();
        let paths = ProjectPaths::under(Path::new("/s/kelpie"), Path::new("/s"), &koji);
        assert_eq!(paths.socket, Path::new("/s/kelpie/run/koji.sock"));
        assert_eq!(socket_of(Path::new("/s/kelpie"), "koji"), paths.socket);
    }

    #[cfg(unix)]
    #[test]
    fn a_runner_socket_too_long_to_bind_is_reported_by_socket_fits() {
        let koji = ProjectName::try_from("koji").unwrap();
        let fits = ProjectPaths::under(Path::new("/s/kelpie"), Path::new("/s"), &koji);
        assert_eq!(fits.socket_fits(), Ok(()));
        let long = format!("/{}", "s".repeat(95));
        let paths = ProjectPaths::under(&Path::new(&long).join("kelpie"), Path::new("/s"), &koji);
        let error = paths.socket_fits().unwrap_err();
        assert!(error.contains("koji.sock"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_door_too_long_to_bind_is_named() {
        let koji = ProjectName::try_from("koji").unwrap();
        let long = format!("/{}", "s".repeat(90));
        let mut paths = ProjectPaths::under(Path::new("/k"), Path::new(&long), &koji);
        paths.door = Path::new(&long).join("kelpie/dog/lease.sock");
        let error = paths.sockets_fit().unwrap_err();
        assert!(error.contains("lease.sock"), "{error}");
    }
}
