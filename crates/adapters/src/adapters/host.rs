//! This machine, as doctor reads it
//!
//! What doctor asks of the machine itself, beyond the runner's ports.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

pub use crabpot::ProfileExclusions;

use crate::adapters::files::is_executable;
use crate::adapters::{LOGIN_VAR, LoginToken};

// How long `--version` has to answer, and how long output left in the pipe is
// waited for once the program has exited.
const VERSION_LIMIT: Duration = Duration::from_secs(10);
const DRAINED: Duration = Duration::from_secs(2);

/// The machine kelpie runs on
pub trait Host: Send + Sync + fmt::Debug {
    /// The programs the sandbox runtime needs that are not on this machine,
    /// none when it can run
    fn sandbox_gaps(&self) -> Vec<&'static str>;

    /// Whether this machine keeps logins in a keychain a sandboxed command can ask
    fn has_keychain(&self) -> bool;

    /// This machine's operating system, as `std::env::consts::OS` names it
    fn os(&self) -> &'static str;

    /// What this machine's crabpot install says about itself, read from
    /// `crabpot::windows_status`, which only a Windows machine answers
    fn windows_status(&self) -> Result<WindowsInstall, String>;

    /// Whether this shell's environment holds the login token `claude setup-token` makes
    fn login_token(&self) -> bool;

    /// Whether `program` is an executable file in a folder of `PATH`
    fn on_path(&self, program: &str) -> bool;

    /// The first line `program --version` prints, given ten seconds to print it
    ///
    /// # Errors
    ///
    /// A message when `program` does not run, does not answer in time, exits
    /// non-zero, or prints nothing.
    fn program_version(&self, program: &str) -> Result<String, String>;
}

/// Whether `found` is `minimum` or later, comparing major, minor and patch as numbers
///
/// None when either does not read as three dot-separated numbers, which a
/// pre-release such as `1.0.4-beta.1` does not.
pub fn at_least(found: &str, minimum: &str) -> Option<bool> {
    Some(numbers(found)? >= numbers(minimum)?)
}

fn numbers(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let mut number = || {
        let part = parts.next()?;
        // `parse` alone would take a leading `+`.
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let numbers = [number()?, number()?, number()?];
    parts.next().is_none().then_some(numbers)
}

/// The programs the sandbox runtime needs on `os`, as `std::env::consts::OS` names it
pub const fn sandbox_needs(os: &str) -> &'static [&'static str] {
    match os.as_bytes() {
        b"macos" => &["sandbox-exec"],
        b"linux" => &["bwrap"],
        _ => &[],
    }
}

/// What a Windows machine's crabpot install says about itself, as far as
/// kelpie reads it: the sandbox user, the setup version, the fence's loopback
/// mode, the Secondary Logon service, the session this process runs in, and
/// whether that user can read this binary.
///
/// The real one is read from `crabpot::windows_status`, which runs this
/// binary again as srt-win for its queries. None of them needs elevation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsInstall {
    /// The sandbox user exists, its credential is stored, and the install's
    /// setup version is this build's. A setup version mismatch reads as not
    /// provisioned, since srt-win refuses a launch for it the way it does for
    /// no install.
    pub provisioned: bool,
    /// What the install recorded as its setup version, None when there is no
    /// install.
    pub setup_installed: Option<u32>,
    /// The setup version this build wants.
    pub setup_expected: u32,
    /// Whether the install's fence leaves loopback open, which is the mode the
    /// install fixed for every session: srt-win refuses a session that asks
    /// for the other.
    pub loopback_open: bool,
    /// Whether the Secondary Logon service can launch the sandbox user.
    pub seclogon_runs: bool,
    /// Whether this process runs in the desktop session, and not in session 0,
    /// the services' session, where crabpot refuses to start a sandbox.
    pub desktop_session: bool,
    /// What the profile read grant keeps out of a session's grants, as `profile
    /// status` lists it: empty when there is no grant.
    pub profile_exclusions: ProfileExclusions,
    /// The variables that name the real user's config files the profile read
    /// grant reaches, each with its value, which nothing sets on a sandboxed
    /// command but the host: empty when there is no current grant.
    pub profile_read_env: Vec<(String, String)>,
    /// Whether the sandbox user can read this binary, which it runs as every
    /// worker's runner: the check a crabpot start makes before it grants
    /// anything. None when the machine could not make it, which refuses nothing.
    pub runner_readable: Option<bool>,
}

impl WindowsInstall {
    /// An install that can run a session, with `loopback_open` as the fence
    /// left it, which a test names instead of reading this machine
    pub fn provisioned(loopback_open: bool) -> Self {
        Self {
            provisioned: true,
            setup_installed: Some(1),
            setup_expected: 1,
            loopback_open,
            seclogon_runs: true,
            desktop_session: true,
            profile_exclusions: ProfileExclusions::default(),
            profile_read_env: Vec::new(),
            runner_readable: Some(true),
        }
    }

    /// The same, with `exclusions` as what the profile read grant keeps out
    #[must_use]
    pub fn excluding(self, exclusions: ProfileExclusions) -> Self {
        Self {
            profile_exclusions: exclusions,
            ..self
        }
    }

    /// The same, with `env` as the variables the profile read grant reaches
    #[must_use]
    pub fn reading<'a>(self, env: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            profile_read_env: env
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            ..self
        }
    }

    /// The profile read variables a command has not already set, as crabpot's
    /// `WindowsStatus::profile_read_env_for` picks them: a name in `supplied`
    /// matches without regard to case, as Windows reads variable names
    pub fn read_env_for<'a>(
        &self,
        supplied: impl IntoIterator<Item = &'a str>,
    ) -> Vec<(&str, &str)> {
        let supplied: Vec<String> = supplied.into_iter().map(str::to_uppercase).collect();
        self.profile_read_env
            .iter()
            .filter(|(name, _)| !supplied.contains(&name.to_uppercase()))
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }

    /// Why no session can be opened under this status, in a sentence that
    /// names the fix, or None when one can
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        if !self.provisioned {
            Some(match self.setup_installed {
                Some(found) => format!(
                    "the crabpot install is setup version {found}, this build wants {}, so run `crabpot windows-install`",
                    self.setup_expected
                ),
                None => "there is no crabpot install, so run `crabpot windows-install`".to_owned(),
            })
        } else if !self.seclogon_runs {
            Some(
                "the Secondary Logon service is off, so the sandbox user cannot be launched"
                    .to_owned(),
            )
        } else if !self.desktop_session {
            Some(
                "kelpie runs in session 0, the services' session, where crabpot refuses to start a sandbox"
                    .to_owned(),
            )
        } else {
            None
        }
    }
}

/// What this machine's crabpot install says about itself, read through
/// `crabpot::windows_status`, which answers only on Windows
///
/// # Errors
///
/// A message when the queries cannot run, fail, or print what cannot be read.
pub fn read_windows_status() -> Result<WindowsInstall, String> {
    crabpot::windows_status()
        .map(|status| WindowsInstall {
            provisioned: status.user == crabpot::UserState::Provisioned,
            setup_installed: status.setup_version.installed,
            setup_expected: status.setup_version.expected,
            loopback_open: status.loopback == crabpot::LoopbackMode::Open,
            seclogon_runs: status.seclogon_runs,
            desktop_session: status.in_desktop_session,
            profile_exclusions: status.profile_exclusions,
            profile_read_env: status.profile_read_env,
            runner_readable: status.runner_readable,
        })
        .map_err(|e| e.to_string())
}

/// The programs in `needs` that `on_path` does not find
pub fn gaps(needs: &[&'static str], on_path: impl Fn(&str) -> bool) -> Vec<&'static str> {
    needs
        .iter()
        .copied()
        .filter(|program| !on_path(program))
        .collect()
}

/// The machine kelpie is running on
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl Host for SystemHost {
    fn sandbox_gaps(&self) -> Vec<&'static str> {
        gaps(sandbox_needs(env::consts::OS), on_path)
    }

    fn has_keychain(&self) -> bool {
        env::consts::OS == "macos"
    }

    fn os(&self) -> &'static str {
        env::consts::OS
    }

    fn windows_status(&self) -> Result<WindowsInstall, String> {
        read_windows_status()
    }

    fn login_token(&self) -> bool {
        env::var(LOGIN_VAR).is_ok_and(|token| LoginToken::new(token).is_some())
    }

    fn on_path(&self, program: &str) -> bool {
        on_path(program)
    }

    fn program_version(&self, program: &str) -> Result<String, String> {
        version_within(program, VERSION_LIMIT)
    }
}

// The first line `program --version` prints, with the child killed when it
// runs over `limit`.
fn version_within(program: impl AsRef<OsStr>, limit: Duration) -> Result<String, String> {
    let name = program.as_ref().to_string_lossy().into_owned();
    let mut child = crate::spawn::command(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    let mut pipe = child.stdout.take();
    let (send, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = send.send(bytes);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{name} --version did not answer in {}s",
                    limit.as_secs_f32()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("cannot wait for {name}: {e}")),
        }
    };
    if !status.success() {
        return Err(format!("{name} --version exited {status}"));
    }
    let said = heard.recv_timeout(DRAINED).unwrap_or_default();
    String::from_utf8_lossy(&said)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{name} --version printed nothing"))
}

// A program is on the path when a folder of `PATH` holds an executable file
// of its name. macOS keeps `sandbox-exec` in `/usr/bin`, which `PATH` names.
fn on_path(program: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|folder| {
        crate::spawn::program_names(program).iter().any(|name| {
            std::fs::metadata(Path::new(&folder).join(name))
                .is_ok_and(|m| m.is_file() && is_executable(&m))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::Fake;

    #[test]
    fn macos_needs_its_own_sandbox_and_linux_needs_bwrap_alone() {
        assert_eq!(sandbox_needs("macos"), ["sandbox-exec"]);
        assert_eq!(sandbox_needs("linux"), ["bwrap"]);
        assert!(sandbox_needs("windows").is_empty());
    }

    #[test]
    fn a_version_is_compared_by_its_numbers_not_its_text() {
        for under in ["1.0.4", "0.99.2", "0.85.1", "1.0.3", "0.100.0"] {
            assert_eq!(at_least("1.0.10", under), Some(true), "1.0.10 over {under}");
            assert_eq!(
                at_least(under, "1.0.10"),
                Some(false),
                "{under} under 1.0.10"
            );
        }
        assert_eq!(at_least("1.0.4", "1.0.4"), Some(true));
        for text in [
            "1.0.4-beta.1",
            "1.0",
            "1.0.4.1",
            "v1.0.4",
            "",
            "latest",
            "1.0.x",
            "1.+0.4",
        ] {
            assert_eq!(at_least(text, "1.0.4"), None, "{text:?} found");
            assert_eq!(at_least("1.0.4", text), None, "{text:?} minimum");
        }
    }

    #[test]
    fn a_gap_is_a_needed_program_off_the_path() {
        let needs = sandbox_needs("linux");
        assert_eq!(gaps(needs, |_| true), Vec::<&str>::new());
        assert_eq!(gaps(needs, |_| false), ["bwrap"]);
    }

    #[test]
    fn an_install_that_cannot_open_a_session_names_its_state_and_its_fix() {
        let ready = WindowsInstall::provisioned(false);
        assert_eq!(ready.refusal(), None);
        let none = WindowsInstall {
            provisioned: false,
            setup_installed: None,
            setup_expected: 1,
            ..ready.clone()
        };
        let why = none.refusal().unwrap();
        assert!(
            why.contains("no crabpot install") && why.contains("crabpot windows-install"),
            "{why}"
        );
        let outdated = WindowsInstall {
            provisioned: false,
            setup_installed: Some(3),
            setup_expected: 4,
            ..ready.clone()
        };
        let why = outdated.refusal().unwrap();
        assert!(
            why.contains("setup version 3") && why.contains("wants 4") && why.contains("install"),
            "{why}"
        );
        let no_service = WindowsInstall {
            seclogon_runs: false,
            ..ready.clone()
        };
        assert!(no_service.refusal().unwrap().contains("Secondary Logon"));
        let services_only = WindowsInstall {
            desktop_session: false,
            ..ready
        };
        assert!(services_only.refusal().unwrap().contains("session 0"));
    }

    #[test]
    fn a_program_every_machine_has_is_on_the_path_and_a_made_up_one_is_not() {
        assert!(on_path(if cfg!(windows) { "cmd" } else { "sh" }));
        assert!(!on_path("kelpie-no-such-program"));
    }

    #[test]
    fn a_program_that_prints_a_version_is_read_and_one_that_hangs_or_is_missing_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, fake: Fake| crate::test::write_fake(dir.path(), name, &fake);
        let limit = Duration::from_secs(5);

        let pi = script("pi", Fake::new().prints("1.0.4").prints("a second line"));
        assert_eq!(version_within(&pi, limit), Ok("1.0.4".into()));

        let goose = script("goose", Fake::new().prints(" 1.54.0"));
        assert_eq!(version_within(&goose, limit), Ok("1.54.0".into()));

        let hang = script("hang", Fake::new().sleeps(30));
        let started = Instant::now();
        let err = version_within(&hang, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));

        let sick = script("sick", Fake::new().prints("1.0.4").exits(3));
        assert!(version_within(&sick, limit).unwrap_err().contains("exited"));

        let silent = script("silent", Fake::new());
        let err = version_within(&silent, limit).unwrap_err();
        assert!(err.contains("printed nothing"), "{err}");

        let err = version_within(dir.path().join("absent"), limit).unwrap_err();
        assert!(err.contains("cannot run"), "{err}");
    }
}
