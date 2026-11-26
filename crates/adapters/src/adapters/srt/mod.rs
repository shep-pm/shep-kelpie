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

use crate::adapters::host::{ProfileExclusions, WindowsInstall};
use crate::ports::{Policy, Sandbox, SandboxError};
use profile::Profile;

mod profile;
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
        if install
            .as_ref()
            .is_some_and(|install| !install.loopback_open)
            && policy.listen
        {
            return Err(SandboxError::Loopback);
        }
        let exclusions = install
            .as_ref()
            .map(|install| install.profile_exclusions.clone());
        let mut wrapped = self.runtime()?;
        let (settings_json, notes) = settings_for(
            policy,
            self.platform,
            loopback_open,
            &exclusions.unwrap_or_default(),
        );
        for note in notes {
            eprintln!("kelpie: {note}");
        }
        let text = serde_json::to_string_pretty(&settings_json).expect("settings are JSON");
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
        // The real user's config the profile read grant reaches, which only the
        // embedder sets: what the command sets itself wins.
        if let Some(install) = &install {
            let set: Vec<_> = command.get_envs().map(|(name, _)| name).collect();
            let set: Vec<_> = set.iter().filter_map(|name| name.to_str()).collect();
            wrapped.envs(install.read_env_for(set));
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
#[cfg(any(test, feature = "test-support"))]
fn settings_on(policy: &Policy, platform: Platform, loopback_open: bool) -> Value {
    settings_for(
        policy,
        platform,
        loopback_open,
        &ProfileExclusions::default(),
    )
    .0
}

// The same, with `exclusions` as what the Windows profile read grant keeps out
// of a session, and the notes of what it left out of the read list
fn settings_for(
    policy: &Policy,
    platform: Platform,
    loopback_open: bool,
    exclusions: &ProfileExclusions,
) -> (Value, Vec<String>) {
    let profile = Profile {
        exclusions,
        entries: &profile::list,
        resolve: &profile::real,
    };
    settings_in(
        policy,
        platform,
        loopback_open,
        crate::home::user_home().as_deref(),
        &Path::exists,
        &profile,
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
    profile: &Profile<'_>,
) -> (Value, Vec<String>) {
    let mut notes = Vec::new();
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
            json!(profile.grants(windows::paths(read_list), &mut notes)),
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
    let settings = json!({
        "network": network,
        "filesystem": {
            "denyRead": deny_read,
            "allowRead": read,
            "allowWrite": write,
            "denyWrite": deny_write,
        },
        "enableWeakerNetworkIsolation": policy.verify_tls,
    });
    (settings, notes)
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

/// What cargo names its `shep-kelpie` binary on this platform
#[cfg(test)]
fn kelpie_name() -> String {
    format!("shep-kelpie{}", std::env::consts::EXE_SUFFIX)
}

/// The `shep-kelpie` of the first folder above `test` that holds one
///
/// cargo writes the binary it builds into its profile folder, which is above the
/// test binary under every build-dir layout it has used: `deps/` held the test
/// binary up to 1.100, and `build/<package>/<unit>/out/` holds it from there.
#[cfg(test)]
fn kelpie_above(test: &Path) -> Option<PathBuf> {
    let name = kelpie_name();
    test.ancestors()
        .skip(1)
        .map(|folder| folder.join(&name))
        .find(|kelpie| kelpie.is_file())
}

/// The `shep-kelpie` binary built for this test run, which has the
/// `crabpot::run_from_args` a test binary lacks
///
/// # Errors
///
/// A message naming the test binary when no folder above it holds a built
/// `shep-kelpie`, which is what a run that built no binary leaves.
#[cfg(test)]
pub(crate) fn built_kelpie() -> Result<PathBuf, String> {
    let test = std::env::current_exe().map_err(|e| e.to_string())?;
    kelpie_above(&test).ok_or_else(|| {
        format!(
            "run `cargo build` for a {} above {}",
            kelpie_name(),
            test.display()
        )
    })
}

#[cfg(test)]
mod live;

#[cfg(test)]
mod profile_env;

#[cfg(test)]
mod tests;
