//! The files a fenced call's hooks run, which its sandbox lets it read
//!
//! A fence denies the home folder whole, and the guard runs the project's hooks
//! inside the sandbox. Each hook is granted its program, the interpreter a
//! script names, and every file its command line names, each through its links,
//! with the folder each sits in: a script loads its neighbours and its
//! `package.json` from there. Never the home or a folder above it. A path under
//! a credential or login deny is not granted: that deny still wins.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::dep_sources::Env;
use crate::fence;
use crate::ports::{Fence, Policy};

use super::policy::{fence_policy, on_path, shebang};

/// What a fenced session may do: its fence, and the files its hooks run and name
pub(super) fn fenced_policy(fence: &Fence, env: &dyn Env) -> Policy {
    let mut policy = fence_policy(fence);
    policy.read.extend(hook_files(fence, env));
    policy
}

// Each file the hooks run or name and its folder, but one a deny that still wins covers
fn hook_files(fence: &Fence, env: &dyn Env) -> Vec<PathBuf> {
    let home = env.get("HOME").map(PathBuf::from);
    let home = home.as_deref();
    let commands =
        (fence.guard.hooks.commands()).chain(fence.hooks.iter().map(|hook| hook.command.as_str()));
    let named: BTreeSet<PathBuf> = commands
        .flat_map(|command| named_files(command, home, env))
        .flat_map(|file| with_folder(file, home))
        .collect();
    let denies = kept_denies(fence, home);
    named
        .into_iter()
        .filter(|file| !denies.iter().any(|rule| covers(rule, file)))
        .collect()
}

// The fence's credential denies, and every harness's login. A home deny is an
// absolute folder whole (`profile::home_denials`), the one hooks carve; a
// credential is under `~/` or one file.
fn kept_denies(fence: &Fence, home: Option<&Path>) -> Vec<Rule> {
    let logins = fence::HARNESSES.iter().flat_map(|own| own.credentials);
    (fence.no_read.iter().map(String::as_str))
        .chain(logins.copied())
        .filter(|rule| rule.starts_with("~/") || !rule.ends_with("/**"))
        .filter_map(|rule| Rule::of(rule, home))
        .collect()
}

// A `no_read` entry: a folder whole when it ends in `/**`, or one path
struct Rule {
    path: PathBuf,
    whole: bool,
}

impl Rule {
    fn of(rule: &str, home: Option<&Path>) -> Option<Self> {
        let (spelled, whole) = match rule.strip_suffix("/**") {
            Some(folder) => (folder, true),
            None => (rule, false),
        };
        Some(Self {
            path: expand(spelled, home)?,
            whole,
        })
    }
}

// Whether `rule` denies `path`, as spelled or through its links
fn covers(rule: &Rule, path: &Path) -> bool {
    let real = crate::canonical(&rule.path).ok();
    [Some(&rule.path), real.as_ref()]
        .into_iter()
        .flatten()
        .any(|denied| match rule.whole {
            true => path.starts_with(denied),
            false => path == denied,
        })
}

// The files `command` runs and names: its program, a script's interpreter and
// each later word that is a file, each as spelled and where its links lead
fn named_files(command: &str, home: Option<&Path>, env: &dyn Env) -> Vec<PathBuf> {
    let mut words = (command.split_whitespace())
        .map(|word| word.trim_matches(['\'', '"']))
        .skip_while(|word| word.contains('=') && !word.starts_with(['/', '~', '$']));
    let Some(first) = words.next() else {
        return Vec::new();
    };
    let program = match expand(first, home) {
        None if !first.contains('/') => on_path(Path::new(first), env),
        found => found,
    };
    let interpreter = program.iter().flat_map(|file| interpreter(file, env));
    let named: Vec<PathBuf> = (program.iter().cloned())
        .chain(interpreter)
        .chain(words.filter_map(|word| expand(word, home)))
        .filter(|path| path.is_file())
        .collect();
    named
        .into_iter()
        .flat_map(|file| {
            let real = crate::canonical(&file).ok();
            std::iter::once(file).chain(real)
        })
        .collect()
}

// `file` and the folder it sits in, unless that folder is the home or holds it
fn with_folder(file: PathBuf, home: Option<&Path>) -> impl Iterator<Item = PathBuf> {
    let folder = (file.parent())
        .filter(|folder| home.is_some_and(|home| !home.starts_with(folder)))
        .map(Path::to_owned);
    std::iter::once(file).chain(folder)
}

// What a script's shebang runs, as a file
fn interpreter(file: &Path, env: &dyn Env) -> Option<PathBuf> {
    let line = shebang(file)?;
    let words: Vec<&str> = line["#!".len()..].split_whitespace().collect();
    match words.as_slice() {
        ["/usr/bin/env", name, ..] => on_path(Path::new(name), env),
        [path, ..] => Some(PathBuf::from(path)),
        _ => None,
    }
}

// `word` as a path, when it is one: absolute, or under the home folder.
// With no home, a `~/` word names nothing.
fn expand(word: &str, home: Option<&Path>) -> Option<PathBuf> {
    match word
        .strip_prefix("~/")
        .or_else(|| word.strip_prefix("$HOME/"))
    {
        Some(rest) => home.map(|home| home.join(rest)),
        None => word.starts_with('/').then(|| PathBuf::from(word)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use super::fenced_policy;
    use crate::dep_sources::Env;
    use crate::hooks::Hooks;
    use crate::ports::{Fence, Policy};
    use crate::profile::WorkerProfile;
    use crate::settings::{GuardHook, HookEvent, HookPoint, NonBlank, ProjectHook};

    // The environment a test names, variable by variable
    struct Fixed(Vec<(&'static str, String)>);

    impl Env for Fixed {
        fn get(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(own, _)| *own == name)
                .map(|(_, value)| value.clone())
        }
    }

    // A worker's fence on the home `root`, its `command` hooks these
    fn worker_fence(root: &Path, commands: &[&str]) -> Fence {
        let wt = root.join("wt");
        let git = wt.join(".git");
        let profile = WorkerProfile {
            worktree: &wt,
            build: &root.join("build"),
            home: root,
            git_common_dir: &git,
            git_dir: &git,
            branch: "kelpie/34",
            base: "main",
            kelpie_pushes: false,
            kelpie: Path::new("/k/kelpie"),
            skills: &root.join("kelpie/skills"),
            kelpie_home: &root.join("kelpie"),
            repo: &root.join("repo"),
            guard_hooks: &[],
            allowed_domains: &[],
            registries: &[],
            build_env: &BTreeMap::new(),
            shep_home: &root.join("shep"),
            door: Path::new("/k/dog/lease.sock"),
            token: None,
            login_token: None,
        };
        let mut fence = *profile.reach().fence.unwrap();
        let list: Vec<ProjectHook> = (commands.iter())
            .map(|&command| ProjectHook {
                point: HookPoint::Command,
                command: NonBlank::try_from(command.to_owned()).unwrap(),
            })
            .collect();
        fence.guard.hooks = Hooks::new("koji", "acme/koji", &wt, &list);
        fence
    }

    // `HOME` the fake home, and `PATH` these folders
    fn home_env(root: &Path, path: &[&Path]) -> Fixed {
        let path = std::env::join_paths(path).unwrap();
        Fixed(vec![
            ("HOME", root.display().to_string()),
            ("PATH", path.into_string().unwrap()),
        ])
    }

    fn file(path: &Path, contents: &str) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        path.to_owned()
    }

    // What the policy grants under `folder`, each where its links lead
    fn reads_under(policy: &Policy, folder: &Path) -> Vec<PathBuf> {
        let real = folder.canonicalize().unwrap();
        let granted: BTreeSet<PathBuf> = (policy.read.iter())
            .filter(|own| own.starts_with(folder) || own.starts_with(&real))
            .map(|own| own.canonicalize().unwrap())
            .collect();
        granted.into_iter().collect()
    }

    fn real(path: PathBuf) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[test]
    fn a_command_hook_grants_the_file_it_names_and_its_folder_and_not_the_home() {
        let home = tempfile::tempdir().unwrap();
        let gate = file(&home.path().join("x/gate.js"), "process.exit(0)\n");
        file(&home.path().join("x/lib/other.js"), "module.exports = 0\n");
        let fence = worker_fence(home.path(), &["/opt/homebrew/bin/node ~/x/gate.js"]);
        let policy = fenced_policy(&fence, &home_env(home.path(), &[]));
        let folder = home.path().join("x");
        assert_eq!(reads_under(&policy, &folder), [real(folder), real(gate)]);
        assert!(
            !policy.read.contains(&home.path().to_owned()),
            "{:?}",
            policy.read
        );
    }

    #[test]
    fn a_hook_script_in_the_home_itself_grants_the_script_alone() {
        let home = tempfile::tempdir().unwrap();
        let gate = file(&home.path().join("gate.sh"), "#!/bin/sh\n");
        let fence = worker_fence(home.path(), &["sh ~/gate.sh"]);
        let policy = fenced_policy(&fence, &home_env(home.path(), &[]));
        assert!(policy.read.contains(&gate), "{:?}", policy.read);
        assert!(
            !policy.read.contains(&home.path().to_owned()),
            "{:?}",
            policy.read
        );
    }

    #[test]
    fn a_hook_script_under_a_credential_deny_stays_denied() {
        let home = tempfile::tempdir().unwrap();
        file(&home.path().join(".ssh/gate.sh"), "#!/bin/sh\n");
        file(&home.path().join(".config/goose/gate.sh"), "#!/bin/sh\n");
        let fence = worker_fence(
            home.path(),
            &["~/.ssh/gate.sh", "sh $HOME/.config/goose/gate.sh"],
        );
        let policy = fenced_policy(&fence, &home_env(home.path(), &[]));
        for folder in [".ssh", ".config"] {
            let granted = reads_under(&policy, &home.path().join(folder));
            assert!(granted.is_empty(), "{granted:?}");
        }
        assert!(
            policy.no_read.contains(&"~/.ssh/**".to_owned()),
            "{:?}",
            policy.no_read
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_hook_script_grants_the_file_it_links_to() {
        let home = tempfile::tempdir().unwrap();
        let real = file(&home.path().join("real/gate.js"), "process.exit(0)\n");
        std::fs::create_dir_all(home.path().join("hooks")).unwrap();
        let link = home.path().join("hooks/gate.js");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let fence = worker_fence(home.path(), &["node ~/hooks/gate.js"]);
        let policy = fenced_policy(&fence, &home_env(home.path(), &[]));
        let target = real.canonicalize().unwrap();
        assert!(policy.read.contains(&target), "{:?}", policy.read);
        assert!(policy.read.contains(&link), "{:?}", policy.read);
        let folders = [target.parent().unwrap(), link.parent().unwrap()];
        assert!(
            folders
                .iter()
                .all(|f| policy.read.contains(&f.to_path_buf()))
        );
    }

    #[test]
    fn a_hook_program_on_the_path_grants_its_file_and_its_interpreter_with_their_folders() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        let gate = file(&bin.join("kelpie-gate"), "#!/usr/bin/env node\n");
        let node_bin = home.path().join(".nvm/v22/bin");
        let node = file(&node_bin.join("node"), "binary\n");
        let fence = worker_fence(home.path(), &["FOO=1 kelpie-gate --strict"]);
        let policy = fenced_policy(&fence, &home_env(home.path(), &[&bin, &node_bin]));
        assert_eq!(reads_under(&policy, &bin), [real(bin), real(gate)]);
        assert_eq!(
            reads_under(&policy, &node_bin),
            [real(node_bin), real(node)]
        );
    }

    #[test]
    fn a_guard_hook_grants_the_file_it_names_and_its_folder() {
        let home = tempfile::tempdir().unwrap();
        let gate = file(&home.path().join("x/gate.js"), "process.exit(0)\n");
        let mut fence = worker_fence(home.path(), &[]);
        fence.hooks = vec![GuardHook {
            event: HookEvent::PreToolUse,
            matcher: None,
            command: NonBlank::try_from("node ~/x/gate.js".to_owned()).unwrap(),
        }];
        let policy = fenced_policy(&fence, &home_env(home.path(), &[]));
        let folder = home.path().join("x");
        assert_eq!(reads_under(&policy, &folder), [real(folder), real(gate)]);
    }
}
