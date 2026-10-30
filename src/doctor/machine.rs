//! The checks that hold for every project: `claude`, `gh`, the sandbox and the shepherd

use std::path::Path;

use shep_client::shep_core::protocol::request::ProcessInfo;

use super::Line;
use crate::adapters::host::{Host, at_least};
use crate::adapters::{GOOSE_MINIMUM, LOGIN_FIX, LOGIN_VAR, PI_MINIMUM};
use crate::ports::{Clock, Forge, ForgeError, Meter, MeterError};
use crate::shepherd::{ConnectRefused, Connection};

// A tool's own message can run to a page, and the first line says what went wrong.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no message")
}

fn says_logged_out(said: &str) -> bool {
    let said = said.to_lowercase();
    said.contains("/login") || said.contains("logged in")
}

/// `claude` installed and logged in:`/usage` answers only for an account
pub(super) fn claude(meter: &dyn Meter, clock: &dyn Clock) -> Line {
    match meter.read(clock.now()) {
        Ok(_) => Line::ok("claude", "installed and logged in"),
        Err(MeterError::Spawn(why)) => Line::missing(
            "claude",
            format!("cannot run `claude`: {why}"),
            "install Claude Code (https://claude.com/claude-code) and put `claude` on your PATH",
        ),
        Err(MeterError::Unreadable(said)) if says_logged_out(&said) => Line::missing(
            "claude",
            format!(
                "`claude` runs but gave no usage, which it does only when logged in: {}",
                first_line(&said)
            ),
            "run `claude`, then `/login`",
        ),
        Err(MeterError::Unreadable(said)) => Line::unsure(
            "claude",
            format!(
                "`claude` runs, but its usage was unreadable: {}",
                first_line(&said)
            ),
            "run `claude -p /usage` to see what it says",
        ),
        Err(e @ (MeterError::TimedOut | MeterError::Stopped | MeterError::Codex(_))) => {
            Line::unsure(
                "claude",
                e.to_string(),
                "run `claude -p /usage` to see what it says",
            )
        }
    }
}

/// `codex` installed and logged in to an account that answers its usage,
/// for `subject`, a project that spends the Codex account
pub(super) fn codex(subject: String, meter: &dyn Meter, clock: &dyn Clock) -> Line {
    let reason = match meter.read(clock.now()) {
        Ok(usage) => {
            let (session, week) = (usage.session.used_pct, usage.week.used_pct);
            return Line::ok(
                subject,
                format!("reads Codex usage: 5-hour window {session}%, week {week}%"),
            );
        }
        Err(e) => e.to_string(),
    };
    if reason.starts_with("cannot run codex") {
        Line::missing(
            subject,
            reason,
            "install the Codex CLI and put `codex` on your PATH",
        )
    } else if reason.starts_with("codex refused") {
        Line::missing(
            subject,
            reason,
            "run `codex login` as an account whose plan includes Codex",
        )
    } else {
        Line::unsure(
            subject,
            reason,
            "run `codex` and its `/status` to see what it says",
        )
    }
}

/// `pi` on the `PATH` at [`PI_MINIMUM`] or later, for `subject`, a project with an agent on pi
pub(super) fn pi(subject: String, host: &dyn Host) -> Line {
    if !host.on_path("pi") {
        return Line::missing(
            subject,
            "cannot run `pi`: it is not on your PATH",
            "run `brew install pi-coding-agent`",
        );
    }
    let said = match host.program_version("pi") {
        Ok(said) => said,
        Err(why) => {
            return Line::unsure(
                subject,
                format!("`pi --version` gave no version: {why}"),
                "run `pi --version`",
            );
        }
    };
    match at_least(&said, PI_MINIMUM) {
        Some(true) => Line::ok(
            subject,
            format!("pi {said}, kelpie needs {PI_MINIMUM} or later"),
        ),
        Some(false) => Line::missing(
            subject,
            format!("pi {said} is older than the {PI_MINIMUM} kelpie needs"),
            "run `brew upgrade pi-coding-agent`",
        ),
        None => Line::unsure(
            subject,
            format!(
                "`pi --version` printed `{}`, which is not a version number",
                first_line(&said)
            ),
            "run `pi --version`",
        ),
    }
}

// Each of pi's own search tools, the program it runs, and every name pi
// accepts for it: Debian calls `fd` `fdfind`.
const PI_TOOLS: [(&str, &str, &[&str]); 2] =
    [("grep", "rg", &["rg"]), ("find", "fd", &["fd", "fdfind"])];

/// `rg` and `fd` on the `PATH` for `subject`, a project with an agent on pi
///
/// pi's `grep` and `find` look in pi's `bin` folder, empty in every kelpie
/// call, then on `PATH`, then download the program, which the sandbox refuses.
/// A tool with none of those fails in every call (`docs/facts/tool-errors.md`).
/// Each line names the program the `PATH` actually has, since pi takes more
/// than one name for `fd`, such as `fdfind`.
pub(super) fn pi_tools(subject: String, host: &dyn Host) -> Line {
    let mut found: Vec<(&str, &str)> = Vec::new();
    let mut gone: Vec<(&str, &[&str])> = Vec::new();
    for (tool, program, names) in PI_TOOLS {
        match names.iter().copied().find(|name| host.on_path(name)) {
            Some(name) => found.push((program, name)),
            None => gone.push((tool, names)),
        }
    }
    if gone.is_empty() {
        let names: Vec<String> = found.iter().map(|(_, name)| format!("`{name}`")).collect();
        let renamed: Vec<String> = found
            .iter()
            .filter(|(program, name)| program != name)
            .map(|(program, name)| format!("`{name}` is `{program}` under another name"))
            .collect();
        let mut what = format!(
            "{} are on your PATH, which pi's `grep` and `find` run",
            names.join(" and ")
        );
        if !renamed.is_empty() {
            what.push_str(&format!(" ({})", renamed.join("; ")));
        }
        return Line::ok(subject, what);
    }
    let said: Vec<String> = gone
        .iter()
        .map(|(tool, names)| {
            // Every tool in `PI_TOOLS` has at most two accepted names.
            let names: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
            let (list, not) = if names.len() == 1 {
                (names.join(""), "which is not")
            } else {
                (names.join(" or "), "and neither is")
            };
            format!(
                "`{tool}` runs {list}, {not} on your PATH, so pi's `{tool}` tool fails in every call"
            )
        })
        .collect();
    Line::missing(
        subject,
        said.join("; "),
        "install them: `brew install ripgrep fd` on macOS, `apt-get install ripgrep fd-find` on Debian",
    )
}

/// `goose` on the `PATH` at [`GOOSE_MINIMUM`] or later, for `subject`, a project with an agent on goose
pub(super) fn goose(subject: String, host: &dyn Host) -> Line {
    if !host.on_path("goose") {
        return Line::missing(
            subject,
            "cannot run `goose`: it is not on your PATH",
            "run `brew install block-goose-cli`",
        );
    }
    let said = match host.program_version("goose") {
        Ok(said) => said,
        Err(why) => {
            return Line::unsure(
                subject,
                format!("`goose --version` gave no version: {why}"),
                "run `goose --version`",
            );
        }
    };
    match at_least(&said, GOOSE_MINIMUM) {
        Some(true) => Line::ok(
            subject,
            format!("goose {said}, kelpie needs {GOOSE_MINIMUM} or later"),
        ),
        Some(false) => Line::missing(
            subject,
            format!("goose {said} is older than the {GOOSE_MINIMUM} kelpie needs"),
            "run `brew upgrade block-goose-cli`",
        ),
        None => Line::unsure(
            subject,
            format!(
                "`goose --version` printed `{}`, which is not a version number",
                first_line(&said)
            ),
            "run `goose --version`",
        ),
    }
}

/// `gh` installed and logged in
pub(super) fn gh(forge: &dyn Forge) -> Line {
    match forge.viewer() {
        Ok(login) => Line::ok("gh", format!("logged in as {login}")),
        Err(ForgeError::Spawn(why)) => Line::missing(
            "gh",
            format!("cannot run `gh`: {why}"),
            "install the GitHub CLI (https://cli.github.com) and put `gh` on your PATH",
        ),
        Err(ForgeError::Failed(said)) => Line::missing(
            "gh",
            format!(
                "not logged in, or GitHub cannot be reached: {}",
                first_line(&said)
            ),
            "run `gh auth login`",
        ),
        Err(e) => Line::missing(
            "gh",
            e.to_string(),
            "update `gh`, then run `gh auth status`",
        ),
    }
}

/// The sandbox runtime every agent runs in, and what this machine needs for it
///
/// macOS and Linux run crabpot, linked into kelpie, and need nothing installed
/// beyond `sandbox_needs`. Windows needs a one-time `crabpot windows-install`
/// that fixes the loopback mode of every later session, so the fix names
/// `--allow-loopback` only when `listen` says a project plans a dev server that
/// binds a local port.
pub(super) fn sandbox(host: &dyn Host, listen: bool) -> Line {
    if host.os() == "windows" {
        return windows_sandbox(host, listen);
    }
    let gaps = host.sandbox_gaps();
    if !gaps.is_empty() {
        let list = gaps.join(" and ");
        // The fix names what is missing, and the example fits the machine:
        // `sandbox-exec` ships with macOS, and Debian calls `bwrap` `bubblewrap`.
        let packages: Vec<_> = gaps
            .iter()
            .map(|program| match *program {
                "bwrap" => "bubblewrap",
                other => other,
            })
            .collect();
        let packages = packages.join(" ");
        let fix = match host.os() {
            "macos" => format!("{list} ships with macOS at /usr/bin/sandbox-exec, so restore it"),
            _ => format!(
                "install {list} with your package manager, such as `sudo apt-get install {packages}` on Debian and Ubuntu"
            ),
        };
        return Line::missing(
            "sandbox",
            format!(
                "the sandbox runtime needs {list}, which this machine lacks, so no agent can run"
            ),
            fix,
        );
    }
    // crabpot is in kelpie, so there is no node `srt` to miss.
    Line::ok("sandbox", "the sandbox runtime can run")
}

// The Windows line, naming each state of the crabpot install that stops a
// worker: no install or another setup version, the Secondary Logon service
// off, session 0, a runner the sandbox user cannot read, and a closed fence
// a listening policy cannot run on.
fn windows_sandbox(host: &dyn Host, listen: bool) -> Line {
    // Opening loopback is the widening it is: a sandboxed worker reaches every
    // service on the machine's own address (`docs/facts/the-second-harness.md`).
    let install = |allow_loopback: bool| {
        format!(
            "run `crabpot windows-install{}`",
            if allow_loopback {
                " --allow-loopback"
            } else {
                ""
            }
        )
    };
    let status = match host.windows_status() {
        Ok(status) => status,
        Err(why) => {
            return Line::missing(
                "sandbox",
                format!("the crabpot install cannot be read: {why}, so no runner starts"),
                install(listen),
            );
        }
    };
    if !status.provisioned {
        let what = match status.setup_installed {
            Some(found) => format!(
                "the crabpot install is setup version {found}, this build wants {}, so no runner starts",
                status.setup_expected
            ),
            None => "there is no crabpot install, so no runner starts".to_owned(),
        };
        return Line::missing("sandbox", what, install(listen));
    }
    if !status.seclogon_runs {
        return Line::missing(
            "sandbox",
            "the Secondary Logon service is off, so the sandbox user cannot be launched",
            "start the Secondary Logon service (`seclogon`), then run `crabpot windows-install`",
        );
    }
    if !status.desktop_session {
        return Line::missing(
            "sandbox",
            "kelpie runs in session 0, the services' session, where crabpot refuses to start a sandbox",
            "run kelpie in the desktop session",
        );
    }
    // The name is the one crabpot checked: it reads the access list of
    // `current_exe`, which is the exe a session runs as its runner.
    if status.runner_readable == Some(false) {
        let exe = std::env::current_exe()
            .map(|exe| exe.display().to_string())
            .unwrap_or_else(|_| "<kelpie's own exe>".to_owned());
        return Line::missing(
            "sandbox",
            format!(
                "the sandbox user cannot read kelpie at `{exe}`, which it runs as every \
                 worker's runner, so no worker starts; a `cargo install` moves the new exe in \
                 from `%TEMP%`, so it keeps that folder's access list and inherits nothing \
                 from the folder it lands in",
            ),
            format!(
                "run `icacls \"{exe}\" /reset`, which drops the list the exe carried in and \
                 re-applies the entries its own folder passes down",
            ),
        );
    }
    if listen && !status.loopback_open {
        return Line::missing(
            "sandbox",
            "a configured project runs a dev server that binds a local port, and this crabpot install closes loopback, so its workers cannot run",
            format!(
                "{}; opening loopback lets every sandboxed worker on this machine reach every service on its own loopback address",
                install(true)
            ),
        );
    }
    let mode = if status.loopback_open {
        "loopback open"
    } else {
        "loopback closed"
    };
    Line::ok("sandbox", format!("the crabpot install can run, {mode}"))
}

/// What the sandbox cannot do on this machine, when it is Linux
///
/// The sandbox skips the globs of `denyWrite` there, and turns every Unix socket off
/// whatever the policy lists (`docs/facts/worker-profile-and-sandbox.md`).
pub(super) fn sandbox_limits(host: &dyn Host) -> Option<Line> {
    (host.os() == "linux").then(|| {
        Line::ok(
            "sandbox: limits",
            "on Linux the sandbox fences only the nested harness folders that exist when a call \
             starts, so nested harness folders made during a call are caught after it, and \
             workers cannot reach the lease door, so `cargo-test` runs unleased",
        )
    })
}

/// The login a project's `claude` calls run with
///
/// Each call keeps its own config folder, which holds no login, so the calls log in
/// with the token alone. The runner reads it from its sheep's `env`, whose
/// secret-backed names are `env`, and doctor from this shell's. A plain value there
/// could be empty, which the runner refuses and shep's reply does not show, so it
/// does not count.
pub(super) fn login(subject: String, env: &[String], host: &dyn Host) -> Line {
    let runner = env.iter().any(|name| name == LOGIN_VAR);
    match (runner, host.login_token()) {
        (true, _) => Line::ok(
            subject,
            "claude logs in with CLAUDE_CODE_OAUTH_TOKEN from the runner's `env`, as each call \
             keeps a config folder of its own",
        ),
        (false, true) => Line::ok(
            subject,
            "claude logs in with CLAUDE_CODE_OAUTH_TOKEN, as each call keeps a config folder \
             of its own",
        ),
        (false, false) => Line::missing(
            subject,
            "CLAUDE_CODE_OAUTH_TOKEN is set in neither the runner's `env` nor this shell, so \
             claude has no login: each call keeps a config folder of its own, where the \
             keychain's is not",
            LOGIN_FIX,
        ),
    }
}

/// The shepherd, on the pinned shep line
pub(super) fn shepherd(client: &dyn Connection, shep_home: &Path) -> Line {
    let version = &client.daemon().daemon_version;
    Line::ok(
        "shepherd",
        format!("shep {version} at {}", shep_home.display()),
    )
}

/// kelpie's dog, which holds the leases and is `online` only once it has named itself
pub(super) fn dog(rows: &[ProcessInfo]) -> Line {
    match crate::flock::dog_problem(rows) {
        None => Line::ok("dog", "kelpie's dog is running and has named itself"),
        Some((what, fix)) => Line::missing("dog", what, fix),
    }
}

/// The shepherd kelpie could not use, and why
pub(super) fn refused(refused: &ConnectRefused, shep_home: &Path) -> Line {
    let fix = match refused {
        ConnectRefused::Unreachable(_) => {
            "start your shepherd, or set SHEP_HOME to the one you run"
        }
        ConnectRefused::Skew(_) => {
            "run the shep kelpie is built for or a later one, or a kelpie built for your shep"
        }
    };
    Line::missing("shepherd", refused.describe(shep_home), fix)
}
