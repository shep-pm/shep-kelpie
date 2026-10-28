//! The sandbox runtime a call runs under
//!
//! It is crabpot, linked into kelpie's own binary, on every platform: a
//! wrapped command is `shep-kelpie --crabpot --settings <file> -- prog args`,
//! which `crabpot::run_from_args` takes over first thing in `main`. On Linux
//! crabpot fences with bubblewrap. On Windows it fences with the WFP filter
//! engine, and the machine needs a one-time `crabpot windows-install`: the
//! install fixes the loopback mode of every session after it, and srt-win
//! refuses a session that asks for the other. A sandbox cannot start inside
//! another, so nothing it runs may start one of its own.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::adapters::host::WindowsInstall;
use crate::ports::{Policy, Sandbox, SandboxError};

mod windows;

// The Mach services a command asks the keychain through, which a covered call may not look up
const KEYCHAIN_SERVICES: [&str; 2] = ["com.apple.securityd.xpc", "com.apple.SecurityServer"];

/// This machine's operating system, as far as the sandbox reads it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Macos,
    Linux,
    Windows,
    /// Any other, which kelpie does not fence on yet
    Other,
}

impl Platform {
    fn of(os: &str) -> Self {
        match os {
            "macos" => Self::Macos,
            "linux" => Self::Linux,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }

    fn here() -> Self {
        Self::of(std::env::consts::OS)
    }

    // Whether only this platform has a keychain to ask, an `open` folder to
    // list or a scratch glob to write.
    fn macos(self) -> bool {
        self == Self::Macos
    }
}

/// The sandbox runtime every call runs in
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRuntime {
    // This machine's operating system
    platform: Platform,
    // The program that is crabpot: kelpie itself, unless a test names the built one.
    kelpie: Option<PathBuf>,
    // What the Windows install says about itself, when a test named one.
    // `None` reads this machine, which only a Windows one answers.
    install: Option<WindowsInstall>,
}

impl SandboxRuntime {
    /// The runtime on this machine
    pub fn new() -> Self {
        Self {
            platform: Platform::here(),
            kelpie: None,
            install: None,
        }
    }

    /// The same, as on macOS, Linux or Windows, which a test runs on any of them
    #[cfg(test)]
    pub(crate) fn on(self, platform: Platform) -> Self {
        Self { platform, ..self }
    }

    /// The same, with `install` as what the Windows machine reports, since a
    /// test on another platform cannot read one
    #[cfg(test)]
    pub(crate) fn with_install(self, install: WindowsInstall) -> Self {
        Self {
            install: Some(install),
            ..self
        }
    }

    /// The same, with `kelpie` as crabpot, since a test binary has no
    /// `run_from_args`
    #[cfg(test)]
    pub(crate) fn run_by(self, kelpie: PathBuf) -> Self {
        Self {
            kelpie: Some(kelpie),
            ..self
        }
    }

    /// The runtime for a test that runs its wrapped command for real: through
    /// the built `shep-kelpie`, and a panic when there is none, since a test
    /// binary would refuse `--crabpot` and the test would prove nothing
    #[cfg(test)]
    pub(crate) fn under_test() -> Self {
        let srt = Self::new();
        match built_kelpie() {
            Ok(kelpie) => srt.run_by(kelpie),
            Err(why) => panic!("no kelpie to run: {why}"),
        }
    }

    // The program that runs a command in crabpot's sandbox
    fn crabpot_exe(&self) -> Result<PathBuf, SandboxError> {
        match &self.kelpie {
            Some(kelpie) => Ok(kelpie.clone()),
            None => std::env::current_exe().map_err(|e| SandboxError::Executable(e.to_string())),
        }
    }

    // What the Windows install says about itself, refused when no session can
    // be opened under it. None off Windows, where the install is the machine.
    fn install_status(&self) -> Result<Option<WindowsInstall>, SandboxError> {
        if self.platform != Platform::Windows {
            return Ok(None);
        }
        let install = match &self.install {
            Some(install) => install.clone(),
            None => crate::adapters::host::read_windows_status().map_err(SandboxError::Install)?,
        };
        match install.refusal() {
            Some(why) => Err(SandboxError::Install(why)),
            None => Ok(Some(install)),
        }
    }

    /// Whether a call can run: macOS and Linux need nothing installed, since
    /// crabpot is in kelpie, and Windows needs a `crabpot windows-install` of
    /// this build's setup version, held by a running Secondary Logon service
    /// and a desktop session
    ///
    /// # Errors
    ///
    /// [`SandboxError::Install`] when the Windows install is missing, of
    /// another setup version, or held by a service or session that refuses it.
    pub fn check(&self) -> Result<(), SandboxError> {
        self.install_status().map(|_| ())
    }

    // The command that runs a program under `--settings`: crabpot, in kelpie's
    // own binary, on every platform
    fn runtime(&self) -> Result<Command, SandboxError> {
        let mut crabpot = crate::spawn::command(self.crabpot_exe()?);
        crabpot.arg("--crabpot");
        Ok(crabpot)
    }
}

impl Default for SandboxRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox for SandboxRuntime {
    fn wrap(
        &self,
        policy: &Policy,
        settings: &Path,
        command: &Command,
    ) -> Result<Command, SandboxError> {
        let install = self.install_status()?;
        // The loopback mode is the install's, not the session's: a session
        // that asks for the other is refused by srt-win, so the settings ask
        // for the install's mode, and a policy that listens on a closed
        // install is refused here, with the doctor's hint.
        let loopback_open = install
            .as_ref()
            .is_some_and(|install| install.loopback_open);
        if install.is_some_and(|install| !install.loopback_open) && policy.listen {
            return Err(SandboxError::Loopback);
        }
        let mut wrapped = self.runtime()?;
        let text = serde_json::to_string_pretty(&settings_on(policy, self.platform, loopback_open))
            .expect("settings are JSON");
        let folder = settings.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(folder)
            .and_then(|()| std::fs::write(settings, text))
            .map_err(|e| SandboxError::Settings(settings.to_owned(), e.kind().to_string()))?;
        wrapped
            .arg("--settings")
            .arg(settings)
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        if let Some(dir) = command.get_current_dir() {
            wrapped.current_dir(dir);
        }
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => wrapped.env(name, value),
                None => wrapped.env_remove(name),
            };
        }
        Ok(wrapped)
    }
}

/// `srt`'s settings file for `policy`, on this machine
#[cfg(any(test, feature = "test-support"))]
pub fn srt_settings(policy: &Policy) -> Value {
    settings_on(policy, Platform::here(), false)
}

// `srt`'s settings file for `policy`, on this platform: `open`, `scratch` and
// `socket_folders` reach srt on macOS alone, and on Windows `allowLocalBinding`
// is the install's loopback mode, not the policy's `listen`, since srt-win
// refuses a session that asks for the other mode the install fixed.
fn settings_on(policy: &Policy, platform: Platform, loopback_open: bool) -> Value {
    settings_in(
        policy,
        platform,
        loopback_open,
        crate::home::user_home().as_deref(),
        &Path::exists,
    )
}

// The same, with `home` as what a `~/` path in a deny list is under and
// `present` as whether a path is there
fn settings_in(
    policy: &Policy,
    platform: Platform,
    loopback_open: bool,
    home: Option<&Path>,
    present: &dyn Fn(&Path) -> bool,
) -> Value {
    // Without `strictAllowlist`, a host off the list is asked about rather than refused.
    let mut network = json!({
        "allowedDomains": policy.hosts,
        "deniedDomains": policy.denied_hosts,
        "strictAllowlist": true,
    });
    if !policy.denied_addresses.is_empty() {
        network["deniedResolvedAddresses"] = json!(policy.denied_addresses);
    }
    if let Some(forward) = &policy.forward {
        let mut hosts = policy.hosts.clone();
        hosts.push(forward.host.clone());
        network["allowedDomains"] = json!(hosts);
        network["mitmProxy"] = json!({
            "socketPath": crate::ipc::address(&forward.socket),
            "domains": [&forward.host],
        });
    }
    let folders = policy.socket_folders.iter().filter(|_| platform.macos());
    let sockets: Vec<&PathBuf> = policy.sockets.iter().chain(folders).collect();
    if !sockets.is_empty() {
        network["allowUnixSockets"] = json!(sockets);
    }
    match platform {
        Platform::Windows => {
            if loopback_open {
                network["allowLocalBinding"] = json!(true);
            }
        }
        _ if policy.listen => network["allowLocalBinding"] = json!(true),
        _ => {}
    }
    if !policy.services.is_empty() {
        network["allowMachLookup"] = json!(policy.services);
    }
    // Only macOS has a keychain to ask. crabpot denies these last, over any allow.
    if policy.no_keychain && platform.macos() {
        network["denyMachLookup"] = json!(KEYCHAIN_SERVICES);
    }
    // On Windows the profile read grant gives the home and default ACLs the
    // system drive, and crabpot refuses a read grant on a root or above a
    // profile exclusion, so the list holds only paths with a drive below it.
    let read_list = policy
        .read
        .iter()
        .filter(|folder| platform != Platform::Windows || below_a_drive(folder));
    let (mut read, mut write, deny_read, deny_write) = match platform {
        Platform::Windows => (
            json!(windows::paths(read_list)),
            json!(windows::paths(&policy.write)),
            json!(windows::paths(existing(&policy.no_read, home, present))),
            json!(windows::paths(existing(&policy.no_write, home, present))),
        ),
        _ => (
            json!(read_list.collect::<Vec<_>>()),
            json!(policy.write),
            json!(policy.no_read),
            json!(policy.no_write),
        ),
    };
    if platform.macos() {
        let (read, write) = (array(&mut read), array(&mut write));
        read.extend(policy.open.iter().map(|folder| alone(folder).into()));
        for glob in &policy.scratch {
            write.extend([json!(glob), json!(format!("{glob}/*"))]);
        }
    }
    json!({
        "network": network,
        "filesystem": {
            "denyRead": deny_read,
            "allowRead": read,
            "allowWrite": write,
            "denyWrite": deny_write,
        },
        "enableWeakerNetworkIsolation": policy.verify_tls,
    })
}

// What `paths` holds that is there, since srt-win makes an empty placeholder
// for a missing deny target and never deletes it. A worktree file made during
// the call is caught by `fence::differ`. `<folder>/**` is there when the folder
// is, `~/` is under `home`, and any other glob is kept for the list to drop.
fn existing<'a, T: AsRef<Path>>(
    paths: &'a [T],
    home: Option<&Path>,
    present: &dyn Fn(&Path) -> bool,
) -> Vec<&'a T> {
    paths
        .iter()
        .filter(|path| {
            let text = path.as_ref().to_string_lossy();
            let text = text.trim_end_matches("/**").trim_end_matches(r"\**");
            if text.contains(['*', '?']) {
                return true;
            }
            match (text.strip_prefix("~/").or(text.strip_prefix(r"~\")), home) {
                (Some(rest), Some(home)) => present(&home.join(rest.replace('\\', "/"))),
                _ => present(Path::new(text)),
            }
        })
        .collect()
}

// Whether `folder` is a Windows path with a drive letter and something below
// the drive's root, read from its text since a Unix build reads no drive in a path
fn below_a_drive(folder: &Path) -> bool {
    let text = folder.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut chars = text.chars();
    let drive = chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':');
    drive && !chars.as_str().trim_matches(['\\', '/']).is_empty()
}

fn array(value: &mut Value) -> &mut Vec<Value> {
    value
        .as_array_mut()
        .expect("a list of paths is a JSON array")
}

// `folder` as a glob that matches it and nothing under it. srt reads a path
// with no glob character as the folder and all it holds, and on macOS turns a
// glob into a regex of the whole path, so one bracketed character is enough.
fn alone(folder: &Path) -> String {
    let text = folder.to_string_lossy();
    let mut glob = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '*' | '?' | '[' | ']' => glob.extend(['[', c, ']']),
            c => glob.push(c),
        }
    }
    if glob.len() == text.len()
        && let Some(last) = glob.pop()
    {
        glob.extend(['[', last, ']']);
    }
    glob
}

/// The `shep-kelpie` binary built beside this test binary, which has the
/// `crabpot::run_from_args` a test binary lacks, or why there is none
#[cfg(test)]
pub(crate) fn built_kelpie() -> Result<PathBuf, String> {
    let test = std::env::current_exe().map_err(|e| e.to_string())?;
    // `<target>/debug/deps/<test>` beside `<target>/debug/shep-kelpie`
    let kelpie = test
        .parent()
        .and_then(Path::parent)
        .map(|debug| debug.join(format!("shep-kelpie{}", std::env::consts::EXE_SUFFIX)))
        .ok_or_else(|| format!("{} has no folder above it", test.display()))?;
    match kelpie.is_file() {
        true => Ok(kelpie),
        false => Err(format!("run `cargo build` for {}", kelpie.display())),
    }
}

#[cfg(test)]
mod live;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_machine_with_no_install_refuses_to_run_anything() {
        let dir = tempfile::tempdir().unwrap();
        let srt = SandboxRuntime::new()
            .on(Platform::Windows)
            .with_install(WindowsInstall {
                provisioned: false,
                setup_installed: None,
                setup_expected: 1,
                loopback_open: false,
                seclogon_runs: true,
                desktop_session: true,
            });
        let settings = dir.path().join("call.srt.json");
        let err = srt
            .wrap(
                &Policy::default(),
                &settings,
                &crate::spawn::command("claude"),
            )
            .unwrap_err();
        assert!(matches!(err, SandboxError::Install(_)), "{err:?}");
        assert!(err.to_string().contains("crabpot windows-install"), "{err}");
        assert!(!settings.exists());
    }

    #[test]
    fn the_forwarded_traffic_goes_to_the_address_the_forwarder_listens_on() {
        let socket = PathBuf::from("/k/worker/model.sock");
        let policy = Policy {
            forward: Some(crate::ports::Forward {
                host: "model.kelpie.test".into(),
                socket: socket.clone(),
            }),
            ..Policy::default()
        };
        let settings = settings_on(&policy, Platform::here(), false);
        let dialed = settings["network"]["mitmProxy"]["socketPath"]
            .as_str()
            .unwrap();
        assert_eq!(Path::new(dialed), crate::ipc::address(&socket));
        if cfg!(windows) {
            assert!(dialed.starts_with(r"\\.\pipe\kelpie-"), "{dialed}");
        } else {
            assert_eq!(dialed, "/k/worker/model.sock");
        }
    }

    #[test]
    fn a_forwarded_host_is_the_only_one_allowed_and_its_traffic_goes_to_the_socket() {
        let policy = Policy {
            forward: Some(crate::ports::Forward {
                host: "model.kelpie.test".into(),
                socket: PathBuf::from("/k/worker/model.sock"),
            }),
            ..Policy::default()
        };
        let network = &srt_settings(&policy)["network"];
        assert_eq!(network["allowedDomains"], json!(["model.kelpie.test"]));
        assert_eq!(network["deniedDomains"], json!([]));
        assert!(network.get("deniedResolvedAddresses").is_none());
        assert_eq!(
            network["mitmProxy"]["domains"],
            json!(["model.kelpie.test"])
        );
        assert!(network.get("allowUnixSockets").is_none());
        let denying = Policy {
            denied_hosts: vec!["192.0.2.9".into(), "localhost".into()],
            denied_addresses: vec!["192.0.2.9".into()],
            ..policy.clone()
        };
        let network = &srt_settings(&denying)["network"];
        assert_eq!(network["deniedDomains"], json!(["192.0.2.9", "localhost"]));
        assert_eq!(network["deniedResolvedAddresses"], json!(["192.0.2.9"]));
        let plain = &srt_settings(&Policy::default())["network"];
        assert!(plain.get("mitmProxy").is_none());
    }

    #[test]
    fn a_policy_that_listens_binds_a_local_port_and_looks_up_its_services() {
        let listening = Policy {
            listen: true,
            services: vec!["com.apple.FSEvents".into()],
            ..Policy::default()
        };
        let network = &settings_on(&listening, Platform::Macos, false)["network"];
        assert_eq!(network["allowLocalBinding"], json!(true));
        assert_eq!(network["allowMachLookup"], json!(["com.apple.FSEvents"]));
        let plain = &settings_on(&Policy::default(), Platform::Macos, false)["network"];
        assert!(plain.get("allowLocalBinding").is_none(), "{plain}");
        assert!(plain.get("allowMachLookup").is_none(), "{plain}");
    }

    // On Windows the loopback mode is the install's, not the policy's: every
    // worker on an `--allow-loopback` install gets `allowLocalBinding`, and a
    // closed install writes nothing, whether or not the policy listens.
    #[test]
    fn on_windows_the_settings_ask_for_the_installs_loopback_mode_not_the_policys() {
        let listening = Policy {
            listen: true,
            ..Policy::default()
        };
        let open = &settings_on(&Policy::default(), Platform::Windows, true)["network"];
        assert_eq!(open["allowLocalBinding"], json!(true));
        let open_listening = &settings_on(&listening, Platform::Windows, true)["network"];
        assert_eq!(open_listening["allowLocalBinding"], json!(true));
        for policy in [&Policy::default(), &listening] {
            let closed = &settings_on(policy, Platform::Windows, false)["network"];
            assert!(closed.get("allowLocalBinding").is_none(), "{closed}");
        }
    }

    // From 0.0.80 srt blocks the user namespaces a command makes itself unless
    // this is set, and setting it gives up the write denies the fence rests on
    // (`docs/facts/worker-profile-and-sandbox.md`).
    #[test]
    fn a_command_may_not_make_user_namespaces_of_its_own() {
        for platform in [Platform::Macos, Platform::Linux] {
            let settings = settings_on(&Policy::default(), platform, false);
            assert!(
                settings.get("allowNestedUserNamespaces").is_none(),
                "{settings}"
            );
        }
    }

    #[test]
    fn on_macos_an_opened_folder_is_read_alone_and_a_scratch_glob_is_written_one_level_down() {
        let policy = Policy {
            read: vec![PathBuf::from("/k/wt/7")],
            write: vec![PathBuf::from("/k/wt/7")],
            open: vec![PathBuf::from("/k/graphs"), PathBuf::from("/k/odd[1]*?")],
            scratch: vec!["/private/tmp/cbm-search-*".into()],
            ..Policy::default()
        };
        let files = &settings_on(&policy, Platform::Macos, false)["filesystem"];
        assert_eq!(
            files["allowRead"],
            json!(["/k/wt/7", "/k/graph[s]", "/k/odd[[]1[]][*][?]"])
        );
        assert_eq!(
            files["allowWrite"],
            json!([
                "/k/wt/7",
                "/private/tmp/cbm-search-*",
                "/private/tmp/cbm-search-*/*"
            ])
        );
        let files = &settings_on(&policy, Platform::Linux, false)["filesystem"];
        assert_eq!(files["allowRead"], json!(["/k/wt/7"]));
        assert_eq!(files["allowWrite"], json!(["/k/wt/7"]));
    }

    #[test]
    fn on_windows_the_read_list_holds_no_root_and_no_path_without_a_drive() {
        let read = [
            r"C:\Users\R\.kelpie\wt\7",
            r"C:\Users\R\.kelpie\build",
            r"\\?\C:\Users\R\AppData\Roaming\npm",
            "/",
            "/bin",
            "/bin/sh",
            r"C:\",
            r"\\?\C:\",
        ];
        let policy = Policy {
            read: read.iter().map(PathBuf::from).collect(),
            ..Policy::default()
        };
        let files = &settings_on(&policy, Platform::Windows, false)["filesystem"];
        assert_eq!(
            files["allowRead"],
            json!([
                r"C:\Users\R\.kelpie\wt\7",
                r"C:\Users\R\.kelpie\build",
                r"\\?\C:\Users\R\AppData\Roaming\npm",
            ])
        );
        for platform in [Platform::Macos, Platform::Linux] {
            let files = &settings_on(&policy, platform, false)["filesystem"];
            assert_eq!(files["allowRead"], json!(read), "{platform:?}");
        }
    }

    // A path srt-win denies that is not there gets an empty placeholder it never
    // deletes, so the fence names only what exists when the call starts.
    fn denying(home: &Path) -> Policy {
        let wt = home.join("wt");
        std::fs::create_dir_all(wt.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(wt.join(".mcp.json"), "{}").unwrap();
        std::fs::write(home.join(".netrc"), "machine").unwrap();
        Policy {
            no_write: vec![
                wt.join(".codex"),
                wt.join("**").join(".codex"),
                wt.join(".mcp.json"),
                wt.join(".pi"),
                wt.join(".agents"),
            ],
            no_read: vec![
                "~/.netrc".into(),
                "~/.git-credentials".into(),
                "~/.ssh/**".into(),
                "~/.aws/**".into(),
                "~/.kelpie/settings.toml".into(),
                home.join(".cargo")
                    .join("credentials")
                    .display()
                    .to_string(),
            ],
            ..Policy::default()
        }
    }

    #[test]
    fn on_windows_a_missing_deny_target_is_left_out_of_both_lists_and_an_existing_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let policy = denying(home);
        let wt = home.join("wt");
        let files = &settings_in(&policy, Platform::Windows, false, Some(home), &Path::exists)["filesystem"];
        let kept = [wt.join(".codex"), wt.join(".mcp.json")];
        assert_eq!(files["denyWrite"], json!(windows::paths(kept)));
        assert_eq!(files["denyRead"], json!([r"~\.netrc", r"~\.ssh"]));
    }

    #[test]
    fn macos_and_linux_keep_every_deny_target_whether_it_exists_or_not() {
        let dir = tempfile::tempdir().unwrap();
        let policy = denying(dir.path());
        for platform in [Platform::Macos, Platform::Linux] {
            let files = &settings_in(&policy, platform, false, Some(dir.path()), &Path::exists)["filesystem"];
            assert_eq!(files["denyWrite"], json!(policy.no_write), "{platform:?}");
            assert_eq!(files["denyRead"], json!(policy.no_read), "{platform:?}");
        }
    }

    #[test]
    fn on_macos_a_socket_folder_is_listed_after_the_sockets_and_off_macos_it_is_not() {
        let socket = PathBuf::from("/k/dog/lease.sock");
        let folder = PathBuf::from("/private/k/call.tmp");
        let policy = |sockets: Vec<PathBuf>, socket_folders: Vec<PathBuf>| Policy {
            sockets,
            socket_folders,
            ..Policy::default()
        };
        let both = policy(vec![socket.clone()], vec![folder.clone()]);
        let folder_only = policy(vec![], vec![folder.clone()]);
        let socket_only = policy(vec![socket.clone()], vec![]);
        let neither = policy(vec![], vec![]);

        let listed = |policy: &Policy, platform: Platform| {
            settings_on(policy, platform, false)["network"]
                .get("allowUnixSockets")
                .cloned()
        };
        assert_eq!(
            listed(&both, Platform::Macos),
            Some(json!(["/k/dog/lease.sock", "/private/k/call.tmp"]))
        );
        assert_eq!(
            listed(&folder_only, Platform::Macos),
            Some(json!(["/private/k/call.tmp"]))
        );
        assert_eq!(
            listed(&socket_only, Platform::Macos),
            Some(json!(["/k/dog/lease.sock"]))
        );
        assert_eq!(listed(&neither, Platform::Macos), None);

        assert_eq!(
            listed(&both, Platform::Linux),
            Some(json!(["/k/dog/lease.sock"]))
        );
        assert_eq!(listed(&folder_only, Platform::Linux), None);
        assert_eq!(
            listed(&socket_only, Platform::Linux),
            Some(json!(["/k/dog/lease.sock"]))
        );
        assert_eq!(listed(&neither, Platform::Linux), None);
    }

    // The call every wrap test makes, and the settings file it writes
    fn inner() -> Command {
        let mut inner = crate::spawn::command("claude");
        inner
            .args(["-p", "it's"])
            .current_dir("/k/wt/7")
            .env("CARGO_TARGET_DIR", "/k/targets/7")
            .env_remove("CLAUDECODE");
        inner
    }

    fn policy() -> Policy {
        Policy {
            write: vec![PathBuf::from("/k/wt/7")],
            hosts: vec!["api.anthropic.com".into()],
            ..Policy::default()
        }
    }

    fn written(settings: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap()
    }

    fn assert_carries_the_call(wrapped: &Command) {
        assert_eq!(wrapped.get_current_dir(), Some(Path::new("/k/wt/7")));
        let envs: Vec<_> = wrapped.get_envs().collect();
        assert!(envs.contains(&("CARGO_TARGET_DIR".as_ref(), Some("/k/targets/7".as_ref()))));
        assert!(envs.contains(&("CLAUDECODE".as_ref(), None)));
    }

    #[test]
    fn on_macos_the_wrapped_command_is_current_exe_crabpot_settings() {
        // Nothing is installed: crabpot is in kelpie, so no node `srt` is needed.
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("worker").join("call.srt.json");
        let wrapped = SandboxRuntime::new()
            .on(Platform::Macos)
            .wrap(&policy(), &settings, &inner())
            .unwrap();
        assert_eq!(
            wrapped.get_program(),
            std::env::current_exe().unwrap().as_os_str()
        );
        let args: Vec<_> = wrapped.get_args().collect();
        assert_eq!(
            args,
            [
                "--crabpot".as_ref(),
                "--settings".as_ref(),
                settings.as_os_str(),
                "--".as_ref(),
                "claude".as_ref(),
                "-p".as_ref(),
                "it's".as_ref(),
            ]
        );
        assert_carries_the_call(&wrapped);
        let written = written(&settings);
        assert_eq!(written, settings_on(&policy(), Platform::Macos, false));
        assert_eq!(written["network"]["strictAllowlist"], true);
        assert_eq!(written["filesystem"]["allowWrite"], json!(["/k/wt/7"]));
    }

    // No platform has a node `srt` to install. Windows needs a crabpot
    // install instead, and `check` is the gate that names it.
    #[test]
    fn no_platform_needs_a_node_runtime_and_windows_needs_its_install() {
        let dir = tempfile::tempdir().unwrap();
        let srt = SandboxRuntime::new();
        srt.clone().on(Platform::Macos).check().unwrap();
        srt.clone().on(Platform::Linux).check().unwrap();
        srt.clone()
            .on(Platform::Windows)
            .with_install(WindowsInstall::provisioned(false))
            .check()
            .unwrap();
        let outdated = srt
            .clone()
            .on(Platform::Windows)
            .with_install(WindowsInstall {
                provisioned: false,
                setup_installed: Some(3),
                setup_expected: 4,
                loopback_open: false,
                seclogon_runs: true,
                desktop_session: true,
            });
        let err = outdated.clone().check().unwrap_err();
        assert!(matches!(err, SandboxError::Install(_)), "{err:?}");
        assert!(
            err.to_string().contains("setup version 3")
                && err.to_string().contains("wants 4")
                && err.to_string().contains("crabpot windows-install"),
            "{err}"
        );
        outdated
            .wrap(&policy(), &dir.path().join("s.json"), &inner())
            .unwrap_err();
        let off = srt
            .clone()
            .on(Platform::Windows)
            .with_install(WindowsInstall {
                seclogon_runs: false,
                ..WindowsInstall::provisioned(false)
            });
        let err = off.check().unwrap_err();
        assert!(err.to_string().contains("Secondary Logon"), "{err}");
        let zero = srt.on(Platform::Windows).with_install(WindowsInstall {
            desktop_session: false,
            ..WindowsInstall::provisioned(false)
        });
        let err = zero.check().unwrap_err();
        assert!(err.to_string().contains("session 0"), "{err}");
    }

    // Nothing is installed: crabpot is in kelpie on Linux too, so no node `srt`
    // and no `socat` is on that path.
    #[test]
    fn on_linux_the_wrapped_command_is_current_exe_crabpot_too() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("worker").join("call.srt.json");
        let wrapped = SandboxRuntime::new()
            .on(Platform::Linux)
            .wrap(&policy(), &settings, &inner())
            .unwrap();
        assert_eq!(
            wrapped.get_program(),
            std::env::current_exe().unwrap().as_os_str()
        );
        let args: Vec<_> = wrapped.get_args().collect();
        assert_eq!(
            args,
            [
                "--crabpot".as_ref(),
                "--settings".as_ref(),
                settings.as_os_str(),
                "--".as_ref(),
                "claude".as_ref(),
                "-p".as_ref(),
                "it's".as_ref(),
            ]
        );
        assert_carries_the_call(&wrapped);
        let written = written(&settings);
        assert_eq!(written, settings_on(&policy(), Platform::Linux, false));
        assert_eq!(written["network"]["strictAllowlist"], true);
        assert_eq!(written["filesystem"]["allowWrite"], json!(["/k/wt/7"]));
    }

    // A Windows worker runs through the same binary, and its settings carry
    // the install's loopback mode: every worker on an `--allow-loopback`
    // install gets `allowLocalBinding`, whether or not its policy listens.
    #[test]
    fn on_windows_the_wrapped_command_is_current_exe_crabpot_too() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("worker").join("call.srt.json");
        let srt = SandboxRuntime::new()
            .on(Platform::Windows)
            .with_install(WindowsInstall::provisioned(true));
        let wrapped = srt.wrap(&policy(), &settings, &inner()).unwrap();
        assert_eq!(
            wrapped.get_program(),
            std::env::current_exe().unwrap().as_os_str()
        );
        let args: Vec<_> = wrapped.get_args().collect();
        assert_eq!(
            args,
            [
                "--crabpot".as_ref(),
                "--settings".as_ref(),
                settings.as_os_str(),
                "--".as_ref(),
                "claude".as_ref(),
                "-p".as_ref(),
                "it's".as_ref(),
            ]
        );
        assert_carries_the_call(&wrapped);
        let written = written(&settings);
        assert_eq!(written, settings_on(&policy(), Platform::Windows, true));
        assert_eq!(written["network"]["allowLocalBinding"], json!(true));
    }

    // A session may not ask for the other loopback mode than the install
    // fixed (srt-win refuses it as exit 19), so a policy that listens on a
    // closed install is refused here, with the doctor's hint.
    #[test]
    fn a_listening_policy_on_a_closed_install_is_refused_with_the_doctors_hint() {
        let dir = tempfile::tempdir().unwrap();
        let srt = SandboxRuntime::new()
            .on(Platform::Windows)
            .with_install(WindowsInstall::provisioned(false));
        let listening = Policy {
            listen: true,
            ..policy()
        };
        let settings = dir.path().join("call.srt.json");
        let err = srt.wrap(&listening, &settings, &inner()).unwrap_err();
        assert!(matches!(err, SandboxError::Loopback), "{err:?}");
        let said = err.to_string();
        assert!(
            said.contains("crabpot windows-install --allow-loopback"),
            "{said}"
        );
        assert!(said.contains("loopback address"), "{said}");
        assert!(!settings.exists());
        // A policy that does not listen runs, and asks for nothing local.
        let wrapped = srt.wrap(&policy(), &settings, &inner()).unwrap();
        assert_eq!(
            written(&settings),
            settings_on(&policy(), Platform::Windows, false)
        );
        assert!(
            written(&settings)["network"]
                .get("allowLocalBinding")
                .is_none(),
            "{wrapped:?}"
        );
    }

    #[test]
    fn no_keychain_adds_deny_mach_lookup_for_both_services() {
        let covered = Policy {
            no_keychain: true,
            services: vec!["com.apple.FSEvents".into()],
            ..Policy::default()
        };
        let network = &settings_on(&covered, Platform::Macos, false)["network"];
        assert_eq!(
            network["denyMachLookup"],
            json!(["com.apple.securityd.xpc", "com.apple.SecurityServer"])
        );
        // The allow is kept: crabpot denies last, over every allow.
        assert_eq!(network["allowMachLookup"], json!(["com.apple.FSEvents"]));
        // A call that leaves the keychain reachable denies nothing.
        let open = &settings_on(&Policy::default(), Platform::Macos, false)["network"];
        assert!(open.get("denyMachLookup").is_none(), "{open}");
        // There is no keychain to ask off macOS.
        let linux = &settings_on(&covered, Platform::Linux, false)["network"];
        assert!(linux.get("denyMachLookup").is_none(), "{linux}");
    }

    // The settings are read by crabpot's own parser, which refuses a key it
    // cannot apply yet rather than drop it.
    #[test]
    fn crabpot_reads_every_setting_a_policy_can_write_on_macos() {
        let policy = Policy {
            read: vec![PathBuf::from("/k/wt/7")],
            write: vec![PathBuf::from("/k/wt/7")],
            no_read: vec!["/Users/me/**".into()],
            no_write: vec![PathBuf::from("/k/wt/7/.git/hooks")],
            open: vec![PathBuf::from("/k/graphs")],
            scratch: vec!["/private/tmp/cbm-search-*".into()],
            hosts: vec!["github.com".into()],
            denied_hosts: vec!["localhost".into()],
            denied_addresses: vec!["192.0.2.9".into()],
            forward: Some(crate::ports::Forward {
                host: "model.kelpie.test".into(),
                socket: PathBuf::from("/k/worker/model.sock"),
            }),
            sockets: vec![PathBuf::from("/k/dog/lease.sock")],
            socket_folders: vec![PathBuf::from("/private/k/call.tmp")],
            listen: true,
            services: vec!["com.apple.FSEvents".into()],
            verify_tls: true,
            no_keychain: true,
        };
        let text = settings_on(&policy, Platform::Macos, false).to_string();
        let config = crabpot::Config::from_json(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        assert_eq!(
            config.network.deny_mach_lookup,
            Some(vec![
                "com.apple.securityd.xpc".to_owned(),
                "com.apple.SecurityServer".to_owned()
            ])
        );
    }
}
