//! `shep kelpie doctor`: what kelpie's projects need on this machine, checked and reported
//!
//! Every check reads through the runner's own ports, so what doctor finds
//! missing is what a runner would stop on. It changes nothing. The one post
//! it can make is a test alert, and only when asked.

pub mod host;
mod machine;
mod project;
mod rulings;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use shep_client::Client;

use self::host::Host;
use crate::adapters::{ClaudeCli, Curl, Gh, LocalReviewer, SystemClock, SystemHost};
use crate::coderabbit::CodeRabbit;
use crate::flock;
use crate::ports::{Alerts, Clock, Forge, Meter, Reviewer};
use crate::review_bot::Profile;
use crate::runner::ProjectName;
use crate::shep_home;
use crate::shepherd;
use crate::tools::Tools;

/// How one check came out
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// In place, with what was found
    Ok(String),
    /// Missing, which a project needs, with the way to put it right
    Missing {
        /// What is wrong
        what: String,
        /// What to do about it
        fix: String,
    },
    /// Not settled either way, which never fails the run
    Unsure {
        /// What could not be told
        what: String,
        /// How to settle it
        next: String,
    },
}

/// One check's line of the report
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// What was checked, such as `claude` or `koji: labels`
    pub subject: String,
    /// How it came out
    pub verdict: Verdict,
}

impl Line {
    fn ok(subject: impl Into<String>, found: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            verdict: Verdict::Ok(found.into()),
        }
    }

    fn missing(
        subject: impl Into<String>,
        what: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            verdict: Verdict::Missing {
                what: what.into(),
                fix: fix.into(),
            },
        }
    }

    fn unsure(
        subject: impl Into<String>,
        what: impl Into<String>,
        next: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            verdict: Verdict::Unsure {
                what: what.into(),
                next: next.into(),
            },
        }
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let subject = &self.subject;
        match &self.verdict {
            Verdict::Ok(found) => write!(f, "ok       {subject}: {found}"),
            Verdict::Missing { what, fix } => {
                write!(f, "MISSING  {subject}: {what}. Fix: {fix}")
            }
            Verdict::Unsure { what, next } => write!(f, "unsure   {subject}: {what}. {next}"),
        }
    }
}

/// Every line doctor found, in the order it looked
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The checks
    pub lines: Vec<Line>,
}

impl Report {
    /// Whether nothing a project needs is missing
    pub fn passed(&self) -> bool {
        self.missing() == 0
    }

    fn missing(&self) -> usize {
        (self.lines.iter())
            .filter(|line| matches!(line.verdict, Verdict::Missing { .. }))
            .count()
    }

    /// The lines as printed, and a last one that says whether to go on
    pub fn render(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.lines.iter().map(Line::to_string).collect();
        lines.push(match self.missing() {
            0 => "nothing a project needs is missing".to_owned(),
            1 => "1 thing a project needs is missing".to_owned(),
            n => format!("{n} things a project needs are missing"),
        });
        lines
    }
}

/// The ports doctor reads through
#[derive(Clone, Copy)]
pub struct Probes<'a> {
    /// The account's usage, which only a logged-in `claude` answers
    pub meter: &'a dyn Meter,
    /// The Codex account's usage on the login in a Codex home, read for a
    /// project that spends it
    pub codex_meter: &'a dyn Fn(&Path) -> Box<dyn Meter>,
    /// The forge
    pub forge: &'a dyn Forge,
    /// The local round's reviewer
    pub reviewer: &'a dyn Reviewer,
    /// The pull request reviewer a project turns on
    pub review_bot: &'a dyn Profile,
    /// The maintainer's webhook
    pub alerts: &'a dyn Alerts,
    /// The machine
    pub host: &'a dyn Host,
    /// The clock
    pub clock: &'a dyn Clock,
}

impl fmt::Debug for Probes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Probes").finish_non_exhaustive()
    }
}

/// Where kelpie's files are on this machine
#[derive(Debug, Clone, Copy)]
pub struct Here<'a> {
    /// The maintainer's home folder, for `~/` in settings
    pub home: &'a Path,
    /// Kelpie's home, which holds its tools
    pub kelpie_home: &'a Path,
    /// The shepherd's home kelpie's runners work theirs out from
    pub shep_home: &'a Path,
    /// Kelpie's own settings file from before the `[kelpie]` section
    pub kelpie_settings: &'a Path,
}

/// What doctor was asked
#[derive(Debug, Clone, Copy, Default)]
pub struct Ask<'a> {
    /// Only this project, every project when absent
    pub project: Option<&'a ProjectName>,
    /// Whether to post a test alert to the webhook
    pub test_alert: bool,
}

/// Checks the machine and each project the shepherd at `shep_home` holds
pub async fn check(shep_home: &Path, probes: Probes<'_>, here: Here<'_>, ask: Ask<'_>) -> Report {
    let mut lines = vec![
        machine::claude(probes.meter, probes.clock),
        machine::gh(probes.forge),
        machine::sandbox(probes.host, &Tools::under(here.kelpie_home)),
    ];
    match shepherd::connect(shep_home).await {
        Ok(client) => {
            lines.push(machine::shepherd(&client, shep_home));
            lines.push(match flock::flock(&client).await {
                Ok(rows) => machine::dog(&rows),
                Err(e) => Line::missing("dog", e, "put the shepherd right"),
            });
            lines.extend(projects(&client, probes, here, ask).await);
        }
        Err(refused) => {
            lines.push(machine::refused(&refused, shep_home));
            lines.push(Line::unsure(
                "projects",
                "not checked, since the shepherd holds them",
                "put the shepherd right, then run doctor again",
            ));
        }
    }
    Report { lines }
}

async fn projects(client: &Client, probes: Probes<'_>, here: Here<'_>, ask: Ask<'_>) -> Vec<Line> {
    let read = async {
        let tables = flock::tables(client).await?;
        let section = shepherd::read_section(client).await?;
        Ok::<_, String>((tables, section))
    };
    let (tables, section) = match read.await {
        Ok(read) => read,
        Err(e) => return vec![Line::missing("projects", e, "put the shepherd right")],
    };
    let mut lines = Vec::new();
    let kelpie = match rulings::kelpie_settings(&section, here.kelpie_settings) {
        Ok(kelpie) => Some(kelpie),
        Err(line) => {
            lines.push(line);
            None
        }
    };
    if ask.test_alert {
        lines.push(rulings::test_alert(kelpie.as_ref(), probes.alerts));
    }
    if let Some(name) = ask.project.filter(|n| !tables.contains_key(n.as_str())) {
        lines.push(Line::missing(
            name.as_str(),
            "no kelpie runner has this name",
            "`shep kelpie add` in its checkout sets one up",
        ));
    }
    for (sheep, table) in &tables {
        if ask.project.is_none_or(|n| n.as_str() == sheep) {
            lines.extend(project::checks(sheep, table, probes, here, kelpie.as_ref()));
        }
    }
    lines
}

/// Runs `shep kelpie doctor [<project>] [--test-alert]`
pub fn main(args: &[String]) -> ExitCode {
    let (project, test_alert) = match read_args(args) {
        Ok(read) => read,
        Err(message) => {
            eprintln!("shep kelpie doctor: {message}");
            return ExitCode::from(2);
        }
    };
    let ran = (|| -> Result<Report, String> {
        let shep_home = shep_home::required(shep_home::FLOCK_FIX)?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let kelpie_home = crate::home::kelpie_home_of(&shep_home);
        let here = Here {
            home: &home,
            kelpie_home: &kelpie_home,
            shep_home: &shep_home,
            kelpie_settings: &kelpie_home.join("settings.toml"),
        };
        let ask = Ask {
            project: project.as_ref(),
            test_alert,
        };
        let claude = ClaudeCli::default();
        let meter = claude.meter();
        let codex_meter = |codex_home: &Path| {
            Box::new(claude.codex_meter(codex_home.to_owned())) as Box<dyn Meter>
        };
        let probes = Probes {
            meter: &meter,
            codex_meter: &codex_meter,
            forge: &Gh,
            reviewer: &LocalReviewer::default(),
            review_bot: &CodeRabbit,
            alerts: &Curl,
            host: &SystemHost,
            clock: &SystemClock,
        };
        shepherd::block_on(async { Ok(check(&shep_home, probes, here, ask).await) })
    })();
    match ran {
        Ok(report) => {
            for line in report.render() {
                println!("{line}");
            }
            if report.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("shep kelpie doctor: {message}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage: shep kelpie doctor [<project>] [--test-alert]";

fn read_args(args: &[String]) -> Result<(Option<ProjectName>, bool), String> {
    let (flags, names): (Vec<_>, Vec<_>) = args.iter().partition(|a| a.starts_with("--"));
    if flags.iter().any(|f| *f != "--test-alert") || names.len() > 1 {
        return Err(USAGE.to_owned());
    }
    let project = (names.first())
        .map(|name| ProjectName::try_from(name.as_str()).map_err(|e| e.to_string()))
        .transpose()?;
    Ok((project, !flags.is_empty()))
}

#[cfg(test)]
mod tests;
