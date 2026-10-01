//! The checks that hold for every project: `claude`, `gh`, the sandbox and the shepherd

use std::path::Path;

use shep_client::Client;
use shep_client::shep_core::protocol::request::ProcessInfo;

use super::Line;
use super::host::Host;
use crate::ports::{Clock, Forge, ForgeError, Meter, MeterError};
use crate::preview::Tools;
use crate::shepherd::ConnectRefused;

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
        Err(e @ (MeterError::TimedOut | MeterError::Stopped)) => Line::unsure(
            "claude",
            e.to_string(),
            "run `claude -p /usage` to see what it says",
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

/// The sandbox runtime installed under `kelpie_home`, with what it needs, which every agent runs in
pub(super) fn sandbox(host: &dyn Host, kelpie_home: &Path) -> Line {
    let gaps = host.sandbox_gaps();
    if !gaps.is_empty() {
        let list = gaps.join(" and ");
        return Line::missing(
            "sandbox",
            format!(
                "the sandbox runtime needs {list}, which this machine lacks, so no agent can run"
            ),
            format!(
                "install {list} with your package manager, such as `sudo apt-get install bubblewrap socat` on Debian"
            ),
        );
    }
    let srt = Tools::under(kelpie_home).sandbox();
    if !srt.is_file() {
        return Line::missing(
            "sandbox",
            format!(
                "the sandbox runtime is not at {}, so no runner starts",
                srt.display()
            ),
            "run `shep kelpie tools install`",
        );
    }
    Line::ok("sandbox", "the sandbox runtime can run")
}

/// The shepherd, on the pinned shep line
pub(super) fn shepherd(client: &Client, shep_home: &Path) -> Line {
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
        ConnectRefused::Skew(_) => "run a shep and a kelpie built for the same shep minor",
    };
    Line::missing("shepherd", refused.describe(shep_home), fix)
}
