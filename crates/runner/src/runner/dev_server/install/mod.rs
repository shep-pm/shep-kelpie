//! The packages of a checkout, installed before its dev server starts
//!
//! A bump's head or a fresh worktree has no `node_modules`. A server that
//! needs a package would fetch it into a temp folder its sandbox refuses. The
//! install is part of the server's command, so it runs in the server's
//! sandbox. It runs only where `node_modules` is missing or an earlier install
//! did not finish. A worker's own install is never wiped. The package manager
//! is the one whose lockfile the project keeps. It runs from the lockfile as
//! it stands, with no install script. A bump's new version is code nobody has
//! read yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::fence;
use crate::package_managers::{self, MANAGERS, PackageManager};
use crate::ports::{DevRun, Fence, TestOutcome};
use crate::spawn::shell_quote;

/// How long the install and the server together get to answer on the port, and the install of a
/// worktree alone gets to end
pub(in crate::runner) const INSTALL_WITHIN: Duration = Duration::from_secs(300);

/// The file in the build folder that an install leaves while it runs
///
/// An install stopped part way leaves a `node_modules` that looks whole, so the next start
/// installs again while this is there.
const UNFINISHED: &str = "install-unfinished";

// What the shell runs: the install in the lockfile's folder unless it holds `node_modules`
// that no install left unfinished, which must succeed, and then the server. The folder is
// `$1`, the mark `$2`, and the server's program and arguments follow them.
const SCRIPT: &str = "(cd \"$1\" && { { [ -d node_modules ] && [ ! -e \"$2\" ]; } \
                      || { touch \"$2\" && INSTALL && rm -f \"$2\"; }; }) \
                      || exit 1; shift 2; exec \"$@\"";

/// What installs the packages of a worktree: one command for every folder that needs it
pub(in crate::runner) struct Plan {
    /// The command, as a shell reads it, to run in the worktree
    pub command: String,
    /// The variables that put each manager's cache in the build folder
    pub env: BTreeMap<String, String>,
    /// The folders it installs, relative to the worktree, `.` for the worktree itself
    pub folders: Vec<String>,
}

/// What the install of a worktree's packages came to
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Installed {
    /// The folders it installed, as the plan names them
    pub folders: Vec<String>,
    /// Why it failed, when it did
    pub failed: Option<String>,
}

impl Installed {
    /// What `outcome`, the run of a plan's command, says of the plan's folders
    pub(in crate::runner) fn of(folders: Vec<String>, outcome: TestOutcome) -> Self {
        let failed = match outcome {
            TestOutcome::Passed => None,
            TestOutcome::Failed { output } => Some(output),
            TestOutcome::TimedOut => Some(format!(
                "it ran past {} seconds and was stopped",
                INSTALL_WITHIN.as_secs()
            )),
            TestOutcome::NotRun { reason } => Some(reason),
        };
        Self { folders, failed }
    }
}

/// The install of the packages of `worktree`, or none when every folder that keeps a lockfile
/// holds `node_modules` that no install left unfinished
///
/// The worktree and the folders directly under it are read, each with the package manager whose
/// lockfile it keeps. The mark is written before the first install and removed after the last
/// one succeeds, so a failure leaves it for the dev server's install, which runs after this one
/// and skips a folder this one installed.
pub(in crate::runner) fn worktree(worktree: &Path, build: &Path) -> Option<Plan> {
    let mark = build.join(UNFINISHED);
    let wanted: Vec<_> = package_managers::folders(worktree)
        .into_iter()
        .filter_map(|folder| {
            let manager = MANAGERS
                .iter()
                .filter(|m| m.install.is_some())
                .find(|m| m.locks(&folder))?;
            let installed = folder.join("node_modules").is_dir() && !mark.exists();
            (!installed).then_some((folder, manager, manager.install?))
        })
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let mut command = format!("touch {}", shell_quote(&mark.display().to_string()));
    let mut env = BTreeMap::new();
    let mut folders = Vec::new();
    for (folder, manager, install) in wanted {
        let at = folder.display().to_string();
        command.push_str(&format!(" && cd {} && {install}", shell_quote(&at)));
        for (variable, cache) in manager.caches {
            env.entry((*variable).to_owned())
                .or_insert_with(|| build.join(cache).display().to_string());
        }
        let shown = folder.strip_prefix(worktree).unwrap_or(&folder);
        folders.push(match shown.as_os_str().is_empty() {
            true => ".".to_owned(),
            false => shown.display().to_string(),
        });
    }
    let mark = shell_quote(&mark.display().to_string());
    command.push_str(&format!(" && rm -f {mark}"));
    Some(Plan {
        command,
        env,
        folders,
    })
}

/// Lets the install of `worktree`'s packages write the harness folders a package ships
///
/// The install runs the package manager and no agent, so `fence` keeps the harnesses' root files
/// and drops the `**` globs a worker's own calls are held to: a package may ship a `.claude`
/// folder, and `lib0` does.
pub(in crate::runner) fn relax(fence: &mut Fence, worktree: &Path) {
    let workers = fence::deny_write(worktree);
    fence.no_write.retain(|path| !workers.contains(path));
    fence.no_write.extend(fence::install_deny_write(worktree));
}

/// The folder with a lockfile that `cwd` or the first folder above it, up to `worktree`, holds
fn lockfile_folder(worktree: &Path, cwd: &Path) -> Option<(PathBuf, &'static PackageManager)> {
    cwd.ancestors()
        .take_while(|folder| folder.starts_with(worktree))
        .find_map(|folder| {
            MANAGERS
                .iter()
                .filter(|m| m.install.is_some())
                .find(|m| m.locks(folder))
                .map(|manager| (folder.to_owned(), manager))
        })
}

/// Has `run` install the packages its checkout keeps a lockfile for before it starts the server
///
/// Does nothing where no lockfile is in the server's folder or above it in the checkout. The
/// server gets the time to install only where the packages are not there yet, and a cache
/// that `run` already names is kept.
pub(super) fn before(run: &mut DevRun, worktree: &Path, build: &Path) {
    let Some((folder, manager)) = lockfile_folder(worktree, &run.cwd) else {
        return;
    };
    let Some(install) = manager.install else {
        return;
    };
    let mark = build.join(UNFINISHED);
    let server = std::mem::replace(
        &mut run.program,
        crate::spawn::sh().to_string_lossy().into_owned(),
    );
    let mut args = vec![
        "-c".to_owned(),
        SCRIPT.replace("INSTALL", install),
        "kelpie-dev-server".to_owned(),
        folder.display().to_string(),
        mark.display().to_string(),
        server,
    ];
    args.append(&mut run.args);
    run.args = args;
    for (variable, folder) in manager.caches {
        (run.env.entry((*variable).to_owned()))
            .or_insert_with(|| build.join(folder).display().to_string());
    }
    (run.policy.hosts).extend(manager.hosts.iter().map(|&h| h.to_owned()));
    if !folder.join("node_modules").is_dir() || mark.exists() {
        run.ready_within = INSTALL_WITHIN;
    }
}

#[cfg(test)]
mod tests;
