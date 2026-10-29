//! The worker's profile: the sandbox and instructions kelpie starts it with
//!
//! The worker runs with no one to ask, so its sandbox is its whole fence.
//! Writes go to its worktree, its build folder and what a commit and a push
//! need, and never to agents' own files in the worktree. Reads go to those
//! folders, the git its commits run through, the toolchain its build runs and
//! what its call is granted; the rest of the home, other repos and their
//! `.env` files included, is denied, and credential paths are unreadable
//! wherever they sit. Hosts are GitHub's and the project's, and a command only
//! the project manager does is one it may not run, each adapter enforcing it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::dep_sources::{self, ProcessEnv};
use crate::fence;
use crate::hooks::Hooks;
use crate::lease::door;
use crate::package_managers::MANAGERS;
use crate::ports::{Fence, Guard, InstallationToken, Reach};
use crate::settings::{BuildDir, EnvName, GuardHook, NonBlank};

pub mod housekeeping;
mod login;
mod login_env;
mod safe_directory;

pub use login::{config_dir as login_dir, write as write_login};

/// The variable that hands a worker the maintainer's gh login, which a
/// harness passes to the process and writes to no file
pub const GH_TOKEN_VAR: &str = "GH_TOKEN";

/// Kelpie's instructions to every worker, appended to its system prompt
pub const INSTRUCTIONS: &str = include_str!("../worker-instructions.md");

// How a worker is told to get its pull request opened: by itself, as the
// maintainer's login, or by writing its text where kelpie opens it from.
const OPEN_YOURSELF: &str = "When the work is done, or before a question with commits made, push the branch with `git push origin HEAD`. Unless the branch already has a pull request, open a draft one with `gh pr create --draft --base {base}`, giving it your own body and a `--title` that is a conventional commit subject.{resolves} When a later turn changes the branch, change the open pull request's body with `gh pr edit --body`.";

// What a worker on a contributor's fork is told after the rest, which outranks every
// instruction above and every prompt after it that says to push
const COMMIT_ONLY: &str = "- This branch is a contributor's, on their fork, and your credential cannot push to it. Commit your work and do not push: kelpie pushes your commits to their branch after each turn, so every instruction to push means to commit, and `git push` is refused. Never force-push, and do not rewrite commits that are already there: kelpie pushes fast-forward only, and refuses a branch that does not extend the contributor's.\n";

const HAND_OVER: &str = "When the work is done, or before a question with commits made, push the branch with `git push origin HEAD`. Do not open or edit the pull request yourself: your credential cannot, and kelpie does it. Write its title and body in `{file}`. The first line is the title, a conventional commit subject such as `fix(parser): keep the last line`, above any template's text: a template's heading is never the title, and kelpie opens nothing from a file whose first line is not a conventional commit subject. The rest is the body{ends}. Write the file before you end your turn, and write it again to change the title or body; kelpie opens the draft when a turn ends with the branch pushed, and sets the title and body to what the file says.";

// What a worker's pull request body ends with when its ticket is a GitHub issue,
// which merging the pull request then closes
const RESOLVES: &str =
    " End the body with the line `Resolves #<issue>`, naming your work item's issue.";
const ENDS_WITH_RESOLVES: &str =
    ", which ends with the line `Resolves #<issue>`, naming your work item's issue";

/// Kelpie's own worker instructions, naming `kelpie`, the binary a worker
/// runs its tests under the machine's test lease with
///
/// With `pull_request_file`, the worker holds a credential that opens no
/// pull request, and is told to write its text there for kelpie to open.
pub fn instructions(kelpie: &Path, pull_request_file: Option<&Path>) -> String {
    instructions_for(
        kelpie,
        pull_request_file,
        true,
        crate::settings::DEFAULT_BASE,
        false,
    )
}

/// [`instructions`] for a work item whose ticket is a GitHub issue when `resolves`, else a
/// local ticket, whose pull request has no `Resolves` line because kelpie closes the ticket
/// when it merges, and the project's `base` branch where the worker is told what it works against
///
/// With `kelpie_pushes`, the branch is a contributor's, on their fork: the worker is told to
/// commit and never to push, whatever else its instructions say of pushing.
pub fn instructions_for(
    kelpie: &Path,
    pull_request_file: Option<&Path>,
    resolves: bool,
    base: &str,
    kelpie_pushes: bool,
) -> String {
    let (resolves, ends) = match resolves {
        true => (RESOLVES, ENDS_WITH_RESOLVES),
        false => ("", ""),
    };
    let pull_request = match pull_request_file {
        Some(file) => HAND_OVER
            .replace("{file}", &file.display().to_string())
            .replace("{ends}", ends),
        None => OPEN_YOURSELF.replace("{resolves}", resolves),
    };
    let mut text = INSTRUCTIONS
        .replace("{pull_request}", &pull_request)
        .replace("{kelpie}", &kelpie.display().to_string())
        .replace("{base}", base);
    if kelpie_pushes {
        text.push_str(COMMIT_ONLY);
    }
    text
}

// Claude Code opens a worktree's whole common git dir to sandboxed writes.
// These are the parts a commit and a push do not need, and whose change
// would run code outside the sandbox. `refs/remotes` stays open for the
// push's tracking ref; kelpie fetches `origin/main` before cutting from it.
const GIT_DENY: [&str; 8] = [
    "config",
    "hooks",
    "info",
    "modules",
    "HEAD",
    "index",
    "packed-refs",
    "refs/tags",
];

// A clone's own git dir is the worker's whole. These are the parts that run
// code outside the sandbox when changed, and the alternates that lend it
// another repo's objects.
const CLONE_DENY: [&str; 5] = ["config", "hooks", "info", "modules", "objects/info"];

/// Read and written by no worker. `~/.config/gh` stays readable for a worker
/// that runs as the maintainer: `gh` will not start without its config, and
/// the worker opens its own pull request. A worker holding the App's token has
/// a config folder of its own.
pub const CREDENTIALS: [&str; 12] = [
    "~/.ssh/**",
    "~/.aws/**",
    "~/.gnupg/**",
    "~/.docker/**",
    "~/.netrc",
    "~/.git-credentials",
    "~/.npmrc",
    "~/.cargo/credentials",
    "~/.cargo/credentials.toml",
    // Kelpie's old home. The maintainer's disk still holds a project's state
    // and settings file, the webhook URL's old file and the authenticator
    // secret there, so these rules still protect real files. Kelpie's home
    // and the shepherd's are the profile's own.
    "~/.kelpie/projects/**",
    "~/.kelpie/settings.toml",
    "~/.kelpie/totp/**",
];

// What only the project manager does: merge, mark ready, and summon. `kelpie
// guard` refuses these in any shape; these match a command's prefix only.
const PM_ONLY: [&str; 5] = [
    "gh pr merge",
    "gh pr merge *",
    "gh pr ready",
    "gh pr ready *",
    "gh *review please*",
];

// `gh api` could merge or relabel around the rules above, and `gh auth token`
// prints the maintainer's token. No worker needs either.
const GH_DENY: [&str; 4] = ["gh api", "gh api *", "gh auth", "gh auth *"];

// Pushes that rewrite or delete branches on the remote whatever they name:
// `--mirror` and `--all` push every local branch, and a `+` refspec forces.
// A rule's `*` needs something to match, so a flag straight after `push`
// takes a rule of its own.
const PUSH_FLAGS: [&str; 9] = [
    "git push *--mirror*",
    "git push *--all*",
    "git push *--delete*",
    "git push -d*",
    "git push * -d*",
    "git push *--force*",
    "git push -f*",
    "git push * -f*",
    "git push * +*",
];

// Every push, for a worker whose commits kelpie pushes. The guard refuses it
// in any shape; these match a command's prefix only.
const KELPIE_PUSHES: [&str; 2] = ["git push", "git push *"];

// What a worker holding the App's token may not read: `gh`'s own config
// folder, which can hold the maintainer's token, and the keychain files. `gh`
// runs on its own config folder then, so it needs neither.
const MAINTAINERS_LOGIN: [&str; 2] = ["~/.config/gh/**", "~/Library/Keychains/**"];

// `security` reads the keychain `gh` and git's osxkeychain helper keep the
// maintainer's token in, and so does the helper run by hand or with
// `git credential`. A worker holding the App's token needs none of them.
// These are a guardrail, not the boundary: the sandbox closes the keychain (#429).
const KEYCHAIN_COMMANDS: [&str; 6] = [
    "security",
    "security *",
    "git credential",
    "git credential *",
    "git credential-*",
    "git -c credential*",
];

// The caches of `package_managers::MANAGERS` a worker's Go and bun write on
// every build and install, which the home's copies refuse
const BUILD_CACHES: [&str; 2] = ["GOCACHE", "BUN_INSTALL_CACHE_DIR"];

// The cache Go's linter writes on every run, whose exit code fails it when the
// home's copy refuses the write
const LINT_CACHE: (&str, &str) = ("GOLANGCI_LINT_CACHE", "golangci-lint");

// What `git push` and `gh` reach. A project adds its own, such as a registry.
const GITHUB: [&str; 2] = ["github.com", "api.github.com"];

// Where GitHub redirects a download of a run's logs, a job's logs or an
// artifact to. Listed by name, since `*.blob.core.windows.net` would reach any
// Azure storage account. The storage hosts are the range GitHub's docs list.
const GITHUB_DOWNLOADS: [&str; 21] = [
    "results-receiver.actions.githubusercontent.com",
    "productionresultssa0.blob.core.windows.net",
    "productionresultssa1.blob.core.windows.net",
    "productionresultssa2.blob.core.windows.net",
    "productionresultssa3.blob.core.windows.net",
    "productionresultssa4.blob.core.windows.net",
    "productionresultssa5.blob.core.windows.net",
    "productionresultssa6.blob.core.windows.net",
    "productionresultssa7.blob.core.windows.net",
    "productionresultssa8.blob.core.windows.net",
    "productionresultssa9.blob.core.windows.net",
    "productionresultssa10.blob.core.windows.net",
    "productionresultssa11.blob.core.windows.net",
    "productionresultssa12.blob.core.windows.net",
    "productionresultssa13.blob.core.windows.net",
    "productionresultssa14.blob.core.windows.net",
    "productionresultssa15.blob.core.windows.net",
    "productionresultssa16.blob.core.windows.net",
    "productionresultssa17.blob.core.windows.net",
    "productionresultssa18.blob.core.windows.net",
    "productionresultssa19.blob.core.windows.net",
];

/// Where one worker may write, and what it runs under
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerProfile<'a> {
    /// The work item's worktree
    pub worktree: &'a Path,
    /// The worker's build folder, its `CARGO_TARGET_DIR`
    pub build: &'a Path,
    /// The project repo's common git dir, which holds objects and refs. On a
    /// clone it is the project checkout's, which the worker reads and never writes.
    pub git_common_dir: &'a Path,
    /// The worktree's own git dir, inside the common one. On a clone it is the
    /// clone's own, outside the checkout's, and the worker writes it.
    pub git_dir: &'a Path,
    /// The work item's branch
    pub branch: &'a str,
    /// The project's base branch, which the worker may not write or push to
    pub base: &'a str,
    /// Whether kelpie pushes the worker's commits itself, because its branch
    /// is a contributor's, on their fork: the worker's token cannot reach it,
    /// and the login that can is never the worker's. Its own push is refused.
    pub kelpie_pushes: bool,
    /// The kelpie binary, which runs kelpie's own checks
    pub kelpie: &'a Path,
    /// The project's skills folder, which holds the plugins every call of the
    /// project is given, and which a skill names a file from
    pub skills: &'a Path,
    /// Kelpie's home, whose path the guard keeps off the forge
    pub kelpie_home: &'a Path,
    /// The project's checkout, whose path the guard keeps off the forge
    pub repo: &'a Path,
    /// The project's own guard hooks, which run after kelpie's
    pub guard_hooks: &'a [GuardHook],
    /// The domains the project's settings add to GitHub's
    pub allowed_domains: &'a [NonBlank],
    /// The registries of the package managers the worktree keeps files for, which
    /// `worker.package_registries` adds to the domains; none while it is off
    pub registries: &'a [&'static str],
    /// Variables the project's settings point into the build folder
    pub build_env: &'a BTreeMap<EnvName, BuildDir>,
    /// The machine's home folder, which the worker reads nothing of but the
    /// folders it works in, the tools it runs and what its call is granted
    pub home: &'a Path,
    /// The shepherd's home, whose `dogs.toml` holds the webhook's URL. The
    /// worker reads none of it but its own folders in kelpie's.
    pub shep_home: &'a Path,
    /// The dog's door, the one Unix socket the worker may connect to
    pub door: &'a Path,
    /// The token of kelpie's App for the project's repo, on a repo the App
    /// covers: the worker's only credential for GitHub, and the
    /// maintainer's login is then out of its reach. Without one the worker
    /// runs as the maintainer's login.
    pub token: Option<&'a InstallationToken>,
    /// The maintainer's own gh login as a token, for a worker with no App
    /// token on Windows. Its sandbox user cannot read the maintainer's gh
    /// config or Credential Manager, so the login reaches it as `GH_TOKEN`
    /// in place of the folder a Unix worker reads. None elsewhere, and
    /// ignored where `token` is set.
    pub login_token: Option<&'a InstallationToken>,
}

impl WorkerProfile<'_> {
    /// The worker's sandbox
    pub fn reach(&self) -> Reach {
        let (write, no_write) = match self.is_clone() {
            true => self.clone_writes(),
            false => self.worktree_writes(),
        };
        // The whole home is denied, and what the call needs is allowed back into
        // it. Kelpie's home is the shepherd's too unless `KELPIE_HOME` puts it
        // elsewhere, and either may sit outside the machine's home.
        let homes = home_denials([self.home, self.shep_home, self.kelpie_home]);
        // A tool home is read back whole, so each cargo home's own credential
        // files are named with it: `CREDENTIALS` spells the default home alone,
        // and `CARGO_HOME` may point anywhere the maintainer keeps it.
        let tools = dep_sources::toolchain_homes(self.home, &ProcessEnv);
        let no_read = CREDENTIALS
            .iter()
            .chain(self.token.iter().flat_map(|_| &MAINTAINERS_LOGIN))
            .map(|&p| p.to_owned())
            .chain(homes)
            .chain(dep_sources::cargo_credentials(&tools))
            .collect();
        // What the call reads back inside the denied home: the folders it works
        // in, the git and toolchain its build runs, the plugins its skills name,
        // and the config its `git` and `gh` start with. A credential named beside
        // them stays in `no_read`, which the sandbox applies after every allow.
        let read = [
            self.worktree,
            self.build,
            self.kelpie,
            self.git_common_dir,
            self.skills,
        ]
        .into_iter()
        .chain(self.is_clone().then_some(self.git_dir))
        .map(Path::to_owned)
        .chain(tools)
        .chain(self.login_carve_outs())
        .collect();
        let no_commands = PM_ONLY
            .iter()
            .chain(&GH_DENY)
            .chain(self.token.iter().flat_map(|_| &KEYCHAIN_COMMANDS))
            .map(|&c| c.to_owned())
            .chain(push_to_base(self.base))
            .chain(PUSH_FLAGS.iter().map(|&c| c.to_owned()))
            .chain(
                (self.kelpie_pushes)
                    .then_some(KELPIE_PUSHES)
                    .into_iter()
                    .flatten()
                    .map(str::to_owned),
            )
            .collect();
        // A registry the project lists itself is not named twice.
        let registries = (self.registries.iter().copied())
            .filter(|host| self.allowed_domains.iter().all(|d| d.as_str() != *host));
        let hosts = GITHUB
            .into_iter()
            .chain(GITHUB_DOWNLOADS)
            .chain(self.allowed_domains.iter().map(NonBlank::as_str))
            .chain(registries)
            .map(str::to_owned)
            .collect();
        let fence = Fence {
            write,
            no_write,
            no_read,
            read,
            hosts,
            no_commands,
            env: self.env(),
            sockets: vec![self.door.to_owned()],
            guard: Guard {
                kelpie: self.kelpie.to_owned(),
                worktree: self.worktree.to_owned(),
                build: self.build.to_owned(),
                git_common_dir: self.git_common_dir.to_owned(),
                base: self.base.to_owned(),
                kelpie_pushes: self.kelpie_pushes,
                folders: vec![self.kelpie_home.to_owned(), self.repo.to_owned()],
                hooks: Hooks::default(),
            },
            hooks: self.guard_hooks.to_vec(),
        };
        Reach {
            read: Vec::new(),
            fence: Some(Box::new(fence)),
            block_keychain: false,
            code_graph: None,
            browser: None,
            mcp_servers: BTreeMap::new(),
        }
    }

    // A git dir outside the common one is a clone's own repo: the common git
    // dir is then the project checkout's, and the worker never writes it.
    fn is_clone(&self) -> bool {
        !self.git_dir.starts_with(self.git_common_dir)
    }

    // A linked worktree: what a commit and a push need of the common git dir.
    fn worktree_writes(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let git = |p: &str| self.git_common_dir.join(p);
        let branch_ref = git("refs/heads").join(self.branch);
        let tracking_ref = git("refs/remotes/origin").join(self.branch);
        let write = vec![
            self.worktree.to_owned(),
            self.build.to_owned(),
            git("objects"),
            self.git_dir.to_owned(),
            git("logs/refs/heads").join(self.branch),
            with_suffix(&branch_ref, ".lock"),
            branch_ref,
            git("logs/refs/remotes/origin").join(self.branch),
            with_suffix(&tracking_ref, ".lock"),
            tracking_ref,
            // A commit locks `packed-refs` before it finds nothing to delete from
            // it. The lock is all it needs then, and the file stays denied
            // below, so a write that needs the file still fails. One a killed
            // git leaves is removed by `worktree::prepare`.
            git("packed-refs.lock"),
        ];
        // The clone's own base branch, which a push of every branch would carry.
        let no_write: Vec<PathBuf> = GIT_DENY
            .iter()
            .map(|p| git(p))
            .chain([git("refs/heads").join(self.base)])
            .chain(fence::deny_write(self.worktree))
            .collect();
        (write, no_write)
    }

    // A clone: its git dir whole, less what git runs code from or borrows
    // objects through. It names no ref, since a ref that does not exist yet
    // cannot be granted, and the push guard keeps its base branch off the remote.
    fn clone_writes(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let own = |p: &str| self.git_dir.join(p);
        let write = vec![
            self.worktree.to_owned(),
            self.build.to_owned(),
            self.git_dir.to_owned(),
        ];
        let no_write = CLONE_DENY
            .iter()
            .map(|p| own(p))
            .chain(fence::deny_write(self.worktree))
            .collect();
        (write, no_write)
    }

    // What the worker's own commands read of the home folder for their config:
    // git's, and `gh`'s when the worker runs as the maintainer's login in a
    // folder. A worker holding the App's token has a `gh` folder of its own in
    // its build folder, and one handed the login as `GH_TOKEN` needs none.
    fn login_carve_outs(&self) -> impl Iterator<Item = PathBuf> {
        let from_folder = self.token.is_none() && self.login_token.is_none();
        let gh = from_folder.then(|| self.home.join(".config/gh"));
        git_config(self.home)
            .into_iter()
            .chain(gh.filter(|path| path.exists()))
    }

    // The folders git trusts on Windows: the common git dir and the worktree,
    // and a clone's own git dir beside them.
    fn trusted_dirs(&self) -> Vec<&Path> {
        let clone = self.is_clone().then_some(self.git_dir);
        [self.git_common_dir, self.worktree]
            .into_iter()
            .chain(clone)
            .collect()
    }

    fn env(&self) -> BTreeMap<String, String> {
        self.env_for(cfg!(windows))
    }

    // On Windows the clone is not the worker's own, so its git trusts the clone's folders.
    fn env_for(&self, windows: bool) -> BTreeMap<String, String> {
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let cargo = ("CARGO_TARGET_DIR".to_owned(), path(self.build));
        // A project's `build_env` comes after, so it may move one.
        let caches = (MANAGERS.iter().flat_map(|m| m.caches))
            .filter(|(name, _)| BUILD_CACHES.contains(name))
            .chain([&LINT_CACHE])
            .map(|(name, dir)| ((*name).to_owned(), path(&self.build.join(dir))));
        let door = (door::SOCKET_VAR.to_owned(), path(self.door));
        let project = self.build_env.iter().map(|(name, dir)| {
            (
                name.as_str().to_owned(),
                path(&self.build.join(dir.as_path())),
            )
        });
        let github = self.token.into_iter().flat_map(|_| login::env(self.build));
        // Its login is `GH_TOKEN`, which `gh auth git-credential` answers from,
        // so it needs the helper and no prompts but no `gh` folder.
        let maintainer = (self.login_token.filter(|_| self.token.is_none()))
            .into_iter()
            .flat_map(|token| {
                let var = (GH_TOKEN_VAR.to_owned(), token.expose().to_owned());
                std::iter::once(var).chain(login::no_prompt())
            });
        // One variable, so a covered worker's value carries both lists.
        let trusted = windows.then(|| safe_directory::parameters(self.trusted_dirs()));
        let git_config = [housekeeping::PARAMETERS]
            .into_iter()
            .chain(trusted.as_deref())
            .chain((self.token.or(self.login_token)).map(|_| login::GIT_CONFIG))
            .collect::<Vec<_>>()
            .join(" ");
        [
            cargo,
            door,
            ("GIT_CONFIG_PARAMETERS".to_owned(), git_config),
        ]
        .into_iter()
        .chain(caches)
        .chain(project)
        .chain(github)
        .chain(maintainer)
        .collect()
    }
}

// Pushes that name the branch kelpie cuts from, as `origin main`,
// `HEAD:main` or `refs/heads/main`. A repo without branch protection
// would otherwise take them. The guard reads every spelling; these are its backup.
fn push_to_base(base: &str) -> impl Iterator<Item = String> {
    [" {b}", " {b} *", ":{b}*", "/{b}*"]
        .into_iter()
        .map(move |p| format!("git push *{}", p.replace("{b}", base)))
}

/// The files of git's own config under `home` that are on disk
///
/// The global config, and the config, ignore and attributes files of git's folder
/// under `~/.config`. Cargo lists a package's files through gix, which fails on an
/// ignore file it cannot read. That folder's `credentials` file is not one of them.
pub fn git_config(home: &Path) -> Vec<PathBuf> {
    [
        ".gitconfig",
        ".config/git/config",
        ".config/git/ignore",
        ".config/git/attributes",
    ]
    .into_iter()
    .map(|file| home.join(file))
    .filter(|path| path.is_file())
    .collect()
}

/// Each of `homes` as a deny over the folder whole, less one another already
/// covers: denying `/k` needs no rule for `/k/shep`
pub fn home_denials<'a>(homes: impl IntoIterator<Item = &'a Path>) -> Vec<String> {
    let mut kept: Vec<&Path> = Vec::new();
    for home in homes {
        if kept.iter().any(|held| home.starts_with(held)) {
            continue;
        }
        kept.retain(|held| !held.starts_with(home));
        kept.push(home);
    }
    kept.into_iter()
        .map(|home| format!("{}/**", home.display()))
        .collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}
