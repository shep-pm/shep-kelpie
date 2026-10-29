//! The sandbox policy every harness starts from

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use crate::dep_sources::{self, Env, ProcessEnv};
use crate::forwarder::{Upstream, WORKER_HOST};
use crate::ports::{AgentCall, Fence, Forward, Policy, Tools};
use crate::profile::{CREDENTIALS, git_config};

use super::hook_reads::fenced_policy;

/// What a fenced session may do, from its fence alone
pub fn fence_policy(fence: &Fence) -> Policy {
    Policy {
        write: fence.write.clone(),
        no_write: fence.no_write.clone(),
        no_read: fence.no_read.clone(),
        read: fence.read.clone(),
        open: Vec::new(),
        scratch: Vec::new(),
        hosts: fence.hosts.clone(),
        sockets: fence.sockets.clone(),
        socket_folders: Vec::new(),
        forward: None,
        denied_hosts: Vec::new(),
        denied_addresses: Vec::new(),
        // Without it, `gh` fails TLS verification on macOS: x509 OSStatus -26276.
        verify_tls: true,
        no_keychain: false,
        listen: false,
        services: Vec::new(),
    }
}

/// What a call is held to before its harness adds its own needs: its fence's
/// policy with what its hooks run, or with no fence the credentials and
/// `unfenced` unread
///
/// Every harness starts here, so a call with no fence is denied `unfenced`
/// whichever one runs it. What that call reads back inside those homes, and what
/// stays denied under them, is in `unfenced_reads`.
pub(crate) fn call_policy(call: &AgentCall, unfenced: &[String]) -> Policy {
    match &call.reach.fence {
        Some(fence) => fenced_policy(fence, &ProcessEnv),
        None => {
            let no_read = CREDENTIALS.iter().map(|&p| p.to_owned());
            let mut policy = Policy {
                no_read: no_read.chain(unfenced.iter().cloned()).collect(),
                ..Policy::default()
            };
            if !unfenced.is_empty() {
                let carve = unfenced_reads(call, unfenced, &ProcessEnv);
                policy.no_read.extend(carve.no_read);
                policy.read = carve.read;
            }
            policy
        }
    }
}

// What a call with no fence is carved back inside the homes it is denied, and
// what stays denied under that carve-back
struct Carve {
    read: Vec<PathBuf>,
    no_read: Vec<String>,
}

// Its own folder, the git its `git` runs through (and a clone's borrowed
// objects), the toolchain and the config `git` starts on: a tool cannot exec
// under a deny of its own folder. A cargo home read back keeps its credentials denied.
fn unfenced_reads(call: &AgentCall, unfenced: &[String], env: &dyn Env) -> Carve {
    let homes: Vec<PathBuf> = unfenced
        .iter()
        .filter_map(|rule| rule.strip_suffix("/**"))
        .map(PathBuf::from)
        .collect();
    let common = crate::worktree::git(
        &call.cwd,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()
    .map(PathBuf::from);
    let borrowed = common.iter().flat_map(|common| alternates(common));
    let tools: Vec<PathBuf> = homes
        .iter()
        .flat_map(|home| dep_sources::toolchain_homes(home, env))
        .collect();
    let config = homes.iter().flat_map(|home| git_config(home));
    Carve {
        read: (std::iter::once(call.cwd.clone()))
            .chain(common.iter().cloned())
            .chain(borrowed)
            .chain(tools.iter().cloned())
            .chain(config)
            .collect(),
        no_read: dep_sources::cargo_credentials(&tools),
    }
}

// The object folders `common`'s `objects/info/alternates` lends it, and the
// ones those lend in turn, as deep as git follows. A relative line is read
// from the `objects` folder it is in, as git reads it.
fn alternates(common: &Path) -> Vec<PathBuf> {
    const DEPTH: usize = 5;
    let mut found: Vec<PathBuf> = Vec::new();
    let mut next = vec![common.join("objects")];
    for _ in 0..DEPTH {
        let lent: Vec<PathBuf> = next.iter().flat_map(|objects| lent_by(objects)).collect();
        next = lent
            .into_iter()
            .filter(|own| !found.contains(own))
            .collect();
        found.extend(next.iter().cloned());
    }
    found
}

fn lent_by(objects: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(objects.join("info/alternates")) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| objects.join(line))
        .collect()
}

/// Where `program` runs, as the files and folders the sandbox must let through
///
/// A call whose sandbox denies the home whole must allow back the harness it
/// starts: a bare name looked up on `PATH`, a link followed to what it runs, each
/// with the folder it runs from and the install folder above a `bin`, and a
/// script's shebang interpreter, which is what the kernel execs (`pi` is Node).
/// `home`, the harness's own folder, sits inside the denied home, so a grant
/// that holds it is the home itself and is never made.
pub(crate) fn program_folder(program: &Path, env: &dyn Env, home: &Path) -> Vec<PathBuf> {
    let Some(found) = (if bare(program) {
        on_path(program, env)
    } else {
        program.is_file().then(|| program.to_owned())
    }) else {
        return Vec::new();
    };
    let real = crate::canonical(&found).unwrap_or_else(|_| found.clone());
    let mut grants: BTreeSet<PathBuf> = [found.clone(), real.clone()].into_iter().collect();
    for file in [&found, &real] {
        grants.extend(install(file, home));
        grants.extend(interpreter(file, env, home));
    }
    grants.into_iter().collect()
}

// A name with no folder is the shell's to find, and kelpie finds it the same way
fn bare(program: &Path) -> bool {
    // `Path::parent` answers an empty folder for a bare name rather than none,
    // and an empty folder is no folder.
    program
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty())
}

// `program` in each `PATH` folder, the first that holds a file of its name
pub(super) fn on_path(program: &Path, env: &dyn Env) -> Option<PathBuf> {
    env.get("PATH")
        .iter()
        .flat_map(|paths| env::split_paths(paths))
        .map(|folder| folder.join(program))
        .find(|candidate| candidate.is_file())
}

// The folder `file` runs from, and the install folder above it when that is a
// `bin`: npm, nvm, volta and bun each keep their tree beside it. A folder holding
// the harness's own home is the denied home or above it, so it is never granted.
fn install(file: &Path, home: &Path) -> Vec<PathBuf> {
    let Some(folder) = file.parent().filter(|own| !own.as_os_str().is_empty()) else {
        return Vec::new();
    };
    let prefix = (folder.file_name() == Some(OsStr::new("bin")))
        .then(|| folder.parent())
        .flatten();
    [Some(folder), prefix]
        .into_iter()
        .flatten()
        .filter(|own| !home.starts_with(own))
        .map(Path::to_owned)
        .collect()
}

// What a script's shebang runs, with the folders it runs from. The kernel execs
// the interpreter, and the sandbox judges it under the same deny.
fn interpreter(file: &Path, env: &dyn Env, home: &Path) -> Vec<PathBuf> {
    let Some(line) = shebang(file) else {
        return Vec::new();
    };
    let words: Vec<&str> = line["#!".len()..].split_whitespace().collect();
    let found = match words.as_slice() {
        ["/usr/bin/env", name, ..] => on_path(Path::new(name), env),
        [path, ..] => Some(PathBuf::from(path)),
        _ => None,
    };
    let Some(program) = found else {
        return Vec::new();
    };
    let real = crate::canonical(&program).unwrap_or_else(|_| program.clone());
    let mut grants: BTreeSet<PathBuf> = [program.clone(), real.clone()].into_iter().collect();
    for file in [&program, &real] {
        grants.extend(install(file, home));
    }
    grants.into_iter().collect()
}

// The `#!` line of `file`, and nothing when it is no script
pub(super) fn shebang(file: &Path) -> Option<String> {
    let mut head = [0u8; 256];
    let read = std::fs::File::open(file).ok()?.read(&mut head).ok()?;
    let text = String::from_utf8_lossy(&head[..read]);
    text.lines()
        .next()
        .filter(|line| line.starts_with("#!"))
        .map(str::to_owned)
}

/// Lets a call that runs a work item's tests bind Unix sockets in `scratch`,
/// the folder its `TMPDIR` names, where the tests make theirs
///
/// srt matches the real path, so `scratch` is resolved where it exists.
pub(crate) fn bind_in_scratch(policy: &mut Policy, call: &AgentCall, scratch: &Path) {
    if call.tools == Tools::Work {
        let real = crate::canonical(scratch);
        policy
            .socket_folders
            .push(real.unwrap_or_else(|_| scratch.to_owned()));
    }
}

/// Holds a call on a model server to the one host it may dial: the forwarder at `socket`
pub(crate) fn dial_the_forwarder(policy: &mut Policy, upstream: &Upstream, socket: &Path) {
    deny_the_server(policy, upstream);
    policy.forward = Some(Forward {
        host: WORKER_HOST.to_owned(),
        socket: socket.to_owned(),
    });
}

// Whatever `allowed_domains` says, the sandbox never reaches the model server
// or this machine's loopback by name or by the address the server resolves to.
pub(crate) fn deny_the_server(policy: &mut Policy, upstream: &Upstream) {
    let bracketed = |host: &str| match host.contains(':') {
        true => format!("[{host}]"),
        false => host.to_owned(),
    };
    for host in [upstream.host(), "localhost", "127.0.0.1", "::1"] {
        let host = bracketed(host);
        if !policy.denied_hosts.contains(&host) {
            policy.denied_hosts.push(host);
        }
    }
    let resolved = upstream.address().to_socket_addrs().into_iter().flatten();
    for address in resolved {
        let address = match address.ip() {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address.ip(), IpAddr::V4),
            v4 => v4,
        };
        let address = address.to_string();
        if !policy.denied_addresses.contains(&address) {
            policy.denied_addresses.push(address);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{call_policy, interpreter, program_folder, unfenced_reads};
    use crate::dep_sources::Env;
    use crate::ports::{AgentCall, Role, Session, SessionId, Tools};
    use crate::settings::{AgentHarness, Effort};
    use crate::test::{NO_PROGRAM, linked_worktree, write_script};
    use std::path::{Path, PathBuf};

    // A session id no live call holds
    fn id() -> SessionId {
        SessionId("5a2e9d40-1c7b-4b38-8e6f-2d4a9c1b7e53".to_owned())
    }

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

    // A call with no fence, run in `cwd`, which is what an unfenced policy is for
    fn unfenced_call(cwd: &Path) -> AgentCall {
        AgentCall::new(
            Role::Reviewer,
            Tools::Review,
            AgentHarness::ClaudeCode,
            "model",
            Effort::Low,
            Session::New(id()),
            cwd.to_owned(),
            "read it",
        )
    }

    // The deny a whole home gets
    fn glob(folder: &Path) -> String {
        format!("{}/**", folder.display())
    }

    // A folder on the `PATH` that holds a `claude` of its own
    fn on_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let program = bin.join("claude");
        write_script(&program, "#!/bin/sh\nexit 0\n");
        (dir, program)
    }

    // A file with a shebang, written without running it: `write_script` probes
    // that what it writes runs, and these are no shell scripts.
    fn write_shebang(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    // A `PATH` of these folders, in order
    fn path_env(folders: &[&Path]) -> Fixed {
        let joined = folders
            .iter()
            .map(|folder| folder.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        Fixed(vec![("PATH", joined)])
    }

    #[test]
    fn a_bare_program_name_is_found_on_the_path_that_holds_it() {
        let (dir, program) = on_path();
        let env = path_env(&[program.parent().unwrap()]);
        let grant = program_folder(Path::new("claude"), &env, &dir.path().join("state"));
        assert!(grant.contains(&program), "{grant:?}");
    }

    #[test]
    fn a_bare_name_no_path_holds_gets_no_grant() {
        let env = path_env(&[Path::new("/nowhere/bin")]);
        assert!(program_folder(Path::new("claude"), &env, Path::new("/state")).is_empty());
    }

    #[test]
    fn a_program_that_is_no_file_gets_no_grant() {
        assert!(program_folder(&NO_PROGRAM, &Fixed(vec![]), Path::new("/state")).is_empty());
    }

    #[test]
    fn a_folder_named_as_the_program_gets_no_grant() {
        let (_dir, program) = on_path();
        let folder = program.parent().unwrap();
        assert!(program_folder(folder, &Fixed(vec![]), Path::new("/state")).is_empty());
    }

    #[test]
    fn a_program_grants_the_folder_it_runs_from_and_the_install_above_a_bin() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(".npm-global/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join("claude");
        write_script(&program, "#!/bin/sh\nexit 0\n");
        let grant = program_folder(&program, &Fixed(vec![]), &dir.path().join(".claude"));
        assert!(grant.contains(&bin), "{grant:?}");
        assert!(grant.contains(&dir.path().join(".npm-global")), "{grant:?}");
    }

    #[test]
    fn a_scripts_interpreter_is_granted_with_the_tree_it_runs_from() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join(".nvm/versions/node/v22/bin");
        std::fs::create_dir_all(&node).unwrap();
        write_script(&node.join("node"), "#!/bin/sh\nexit 0\n");
        let bin = dir.path().join(".volta/bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_shebang(&bin.join("pi"), "#!/usr/bin/env node\n");
        let env = path_env(&[&bin, &node]);
        let grant = program_folder(&bin.join("pi"), &env, &dir.path().join(".pi"));
        assert!(grant.contains(&node.join("node")), "{grant:?}");
        assert!(grant.contains(&node), "{grant:?}");
        assert!(
            grant.contains(&node.parent().unwrap().to_path_buf()),
            "the install folder above the interpreter's bin: {grant:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_held_by_a_link_grants_what_the_link_runs() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join(".npm-global/lib/node_modules/pi");
        std::fs::create_dir_all(&package).unwrap();
        let real = package.join("cli.js");
        write_shebang(&real, "#!/usr/bin/env node\n");
        let bin = dir.path().join(".npm-global/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("pi")).unwrap();
        let grant = program_folder(&bin.join("pi"), &Fixed(vec![]), &dir.path().join(".pi"));
        let resolved = package.canonicalize().unwrap();
        assert!(grant.contains(&resolved), "{grant:?}");
    }

    #[test]
    fn no_grant_opens_the_denied_home_whole() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("claude");
        write_script(&program, "#!/bin/sh\nexit 0\n");
        let grant = program_folder(&program, &Fixed(vec![]), &dir.path().join(".claude"));
        assert!(grant.contains(&program), "{grant:?}");
        assert!(!grant.contains(&dir.path().to_path_buf()), "{grant:?}");
    }

    #[test]
    fn an_unfenced_call_reads_the_git_dir_its_worktree_points_to() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, worktree) = linked_worktree(dir.path());
        let policy = call_policy(&unfenced_call(&worktree), &[glob(dir.path())]);
        let common = repo.join(".git").canonicalize().unwrap();
        assert!(
            policy
                .read
                .iter()
                .any(|own| own.canonicalize().unwrap_or_default() == common),
            "{:?}",
            policy.read
        );
    }

    #[test]
    fn an_unfenced_call_in_a_clone_reads_the_objects_it_borrows() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _) = linked_worktree(dir.path());
        let clone = dir.path().join("clone");
        let output = std::process::Command::new("git")
            .args(["clone", "--quiet", "--shared"])
            .args([&repo, &clone])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let lent = repo.join(".git/objects").canonicalize().unwrap();
        let policy = call_policy(&unfenced_call(&clone), &[glob(dir.path())]);
        assert!(
            policy
                .read
                .iter()
                .any(|own| own.canonicalize().unwrap_or_default() == lent),
            "{:?}",
            policy.read
        );
    }

    #[test]
    fn an_unfenced_call_in_a_clone_of_a_clone_reads_the_objects_both_lend() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _) = linked_worktree(dir.path());
        let (middle, clone) = (dir.path().join("middle"), dir.path().join("clone"));
        for (from, to) in [(&repo, &middle), (&middle, &clone)] {
            let output = std::process::Command::new("git")
                .args(["clone", "--quiet", "--shared"])
                .args([from, to])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        let policy = call_policy(&unfenced_call(&clone), &[glob(dir.path())]);
        for lender in [&repo, &middle] {
            let lent = lender.join(".git/objects").canonicalize().unwrap();
            assert!(
                policy
                    .read
                    .iter()
                    .any(|own| own.canonicalize().unwrap_or_default() == lent),
                "{lent:?} in {:?}",
                policy.read
            );
        }
    }

    #[test]
    fn an_unfenced_call_reads_the_toolchain_and_config_under_the_denied_home() {
        let dir = tempfile::tempdir().unwrap();
        let tools = [".cargo", ".rustup", "go/bin", "go/pkg/mod", ".bun"];
        for folder in tools {
            std::fs::create_dir_all(dir.path().join(folder)).unwrap();
        }
        std::fs::write(dir.path().join(".gitconfig"), "[user]\n").unwrap();
        let git = dir.path().join(".config/git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("ignore"), ".DS_Store\n").unwrap();
        std::fs::write(git.join("credentials"), "https://x:token@github.com\n").unwrap();
        let env = Fixed(vec![
            (
                "CARGO_HOME",
                dir.path().join(".cargo").display().to_string(),
            ),
            (
                "RUSTUP_HOME",
                dir.path().join(".rustup").display().to_string(),
            ),
        ]);
        let carve = unfenced_reads(&unfenced_call(dir.path()), &[glob(dir.path())], &env);
        for folder in tools.iter().chain(&[".gitconfig", ".config/git/ignore"]) {
            let own = dir.path().join(folder);
            assert!(carve.read.contains(&own), "{own:?} in {:?}", carve.read);
        }
        assert!(!carve.read.contains(&git.join("credentials")));
        // Each is read back whole, so its own registry token stays denied by name.
        for name in ["credentials", "credentials.toml"] {
            let own = format!("{}/{}", dir.path().join(".cargo").display(), name);
            assert!(carve.no_read.contains(&own), "{own}: {:?}", carve.no_read);
        }
    }

    // The wiring and not only the rule: `call_policy` is where every harness
    // starts, and the default cargo home needs no variable to be found.
    #[test]
    fn an_unfenced_call_denies_the_credentials_of_a_cargo_home_it_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join(".cargo");
        std::fs::create_dir_all(cargo.join("registry/src")).unwrap();
        std::fs::write(cargo.join("credentials.toml"), "token\n").unwrap();
        let policy = call_policy(&unfenced_call(&dir.path().join("wt")), &[glob(dir.path())]);
        assert!(policy.read.contains(&cargo), "{:?}", policy.read);
        for name in ["credentials", "credentials.toml"] {
            let own = format!("{}/{}", cargo.display(), name);
            assert!(policy.no_read.contains(&own), "{own}: {:?}", policy.no_read);
        }
    }

    #[test]
    fn an_unfenced_call_reads_its_own_folder_and_nothing_of_a_home_it_is_not_in() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("elsewhere/wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let carve = unfenced_reads(
            &unfenced_call(&worktree),
            &[glob(Path::new("/Users/me"))],
            &Fixed(vec![]),
        );
        assert!(carve.read.contains(&worktree), "{:?}", carve.read);
        assert!(
            carve.read.iter().all(|own| !own.starts_with("/Users/me")),
            "{:?}",
            carve.read
        );
        assert!(carve.no_read.is_empty(), "{:?}", carve.no_read);
    }

    #[test]
    fn a_call_with_no_home_denied_reads_nothing_extra() {
        let dir = tempfile::tempdir().unwrap();
        let policy = call_policy(&unfenced_call(dir.path()), &[]);
        assert!(policy.read.is_empty(), "{:?}", policy.read);
    }

    #[test]
    fn a_compiled_program_names_no_interpreter() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("codex");
        std::fs::write(&program, [0x7f, b'E', b'L', b'F']).unwrap();
        assert!(interpreter(&program, &Fixed(vec![]), &dir.path().join(".codex")).is_empty());
    }
}
