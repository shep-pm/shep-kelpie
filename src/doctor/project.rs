//! The checks that hold for one project, from its settings

use serde_json::{Map, Value};

use super::host::Host;
use super::{Here, Line, Probes, rulings};
use crate::flock::add::LABELS;
use crate::ports::{NewLabel, Visibility};
use crate::preview::Tools;
use crate::runner::{ProjectName, ProjectPaths, SUMMON_LABEL, check_instructions, check_repo};
use crate::settings::{LocalRound, Settings};
use crate::webhook::KelpieSettings;

/// Every check for the project on `sheep`, whose table is `table`
///
/// A table that does not load is the one line: nothing else can be read from it.
pub(super) fn checks(
    sheep: &str,
    table: &Map<String, Value>,
    probes: Probes<'_>,
    here: Here<'_>,
    kelpie: Option<&KelpieSettings>,
) -> Vec<Line> {
    let at = |what: &str| format!("{sheep}: {what}");
    let wrong = |what: String| {
        vec![Line::missing(
            at("settings"),
            what,
            "correct the setting it names in the runner's `[app.dogs.kelpie]` table",
        )]
    };
    // A relative path in the settings is taken from the project's own folder, as the runner does.
    let paths = match ProjectName::try_from(sheep) {
        Ok(name) => ProjectPaths::under(here.kelpie_home, &name),
        Err(e) => return wrong(e.to_string()),
    };
    let folder = paths.settings.parent().unwrap_or(here.home);
    let settings = match Settings::from_table(table, sheep, here.home, folder) {
        Ok(settings) => settings,
        Err(e) => return wrong(e.to_string()),
    };
    let repo = &settings.forge;
    let slug = repo.as_str();
    let mut lines = vec![checkout(at("checkout"), &settings)];
    if settings.worker.instructions_file.is_some() {
        lines.push(instructions(at("instructions"), &settings));
    }

    lines.push(match probes.forge.can_push(repo) {
        Ok(true) => Line::ok(at("push access"), format!("may push to {slug}")),
        Ok(false) => Line::missing(
            at("push access"),
            format!("the account `gh` is logged in as cannot push to {slug}"),
            "ask for write access to it, or `gh auth login` as an account that has it",
        ),
        Err(e) => Line::missing(
            at("push access"),
            format!("cannot read the account's access to {slug}: {e}"),
            format!("run `gh repo view {slug}` to see why"),
        ),
    });

    let wanted: Vec<NewLabel> = (LABELS.into_iter())
        .filter(|l| settings.coderabbit.enabled || l.name != SUMMON_LABEL)
        .collect();
    lines.push(match probes.forge.repo_labels(repo) {
        Ok(have) => {
            let gone: Vec<_> = (wanted.iter().map(|l| l.name))
                .filter(|name| !have.iter().any(|l| l == name))
                .collect();
            if gone.is_empty() {
                Line::ok(at("labels"), format!("{slug} has {}", names(&wanted)))
            } else {
                Line::missing(
                    at("labels"),
                    format!("{slug} has no {}", gone.join(", ")),
                    format!(
                        "`shep kelpie add` in {} makes them",
                        settings.repo.display()
                    ),
                )
            }
        }
        Err(e) => Line::missing(
            at("labels"),
            format!("cannot read {slug}'s labels: {e}"),
            format!("run `gh label list --repo {slug}` to see why"),
        ),
    });

    if settings.coderabbit.enabled {
        lines.push(review_bot(
            &at(&probes.review_bot.name().to_lowercase()),
            &settings,
            probes,
        ));
    }
    lines.push(local_round(
        at("local review"),
        &settings.review.local,
        probes,
    ));
    if settings.preview.enabled {
        lines.push(preview(at("preview tools"), here, probes.host));
    }
    if let Some(kelpie) = kelpie {
        let project = settings.ruling_channels.as_ref();
        lines.push(rulings::channel(at("rulings"), project, kelpie));
    }
    lines
}

fn checkout(subject: String, settings: &Settings) -> Line {
    match check_repo(settings) {
        Ok(()) => Line::ok(
            subject,
            format!(
                "{} is a git checkout with an origin",
                settings.repo.display()
            ),
        ),
        Err(e) => Line::missing(
            subject,
            e.to_string(),
            "set `repo` to where the checkout is, or run `shep kelpie add` in it",
        ),
    }
}

fn instructions(subject: String, settings: &Settings) -> Line {
    match check_instructions(settings) {
        Ok(()) => Line::ok(subject, "the worker's extra instructions can be read"),
        Err(e) => Line::missing(
            subject,
            e.to_string(),
            "put the file there, or correct `worker.instructions_file`",
        ),
    }
}

fn names(labels: &[NewLabel]) -> String {
    let quoted: Vec<_> = labels.iter().map(|l| format!("`{}`", l.name)).collect();
    quoted.join(", ")
}

// The runner refuses to start with the bot on for a repo that is not
// public, and a bot that has never commented on it is likely not installed.
fn review_bot(subject: &str, settings: &Settings, probes: Probes<'_>) -> Line {
    let (repo, bot) = (&settings.forge, probes.review_bot);
    let slug = repo.as_str();
    match probes.forge.visibility(repo) {
        Ok(Visibility::Public) => {}
        Ok(_) => {
            return Line::missing(
                subject,
                format!(
                    "{slug} is not public, and {}'s free plan reviews public repos only",
                    bot.name()
                ),
                "set `coderabbit.enabled = false` for this project, or make the repo public",
            );
        }
        Err(e) => {
            return Line::unsure(
                subject,
                format!("cannot read whether {slug} is public: {e}"),
                format!("run `gh repo view {slug}` to see why"),
            );
        }
    }
    match probes.forge.review_bot_seen(repo, bot.login()) {
        Ok(true) => Line::ok(
            subject,
            format!("{} has commented on a pull request of {slug}", bot.name()),
        ),
        Ok(false) => Line::unsure(
            subject,
            format!(
                "{} has not commented on any pull request of {slug}, so it may not be installed there",
                bot.name()
            ),
            format!(
                "install it on {slug}, or set `coderabbit.enabled = false`; a repo with no reviewed pull request yet reads this way"
            ),
        ),
        Err(e) => Line::unsure(
            subject,
            format!("cannot read whether {} is on {slug}: {e}", bot.name()),
            format!("run `gh search prs --repo {slug}` to see why"),
        ),
    }
}

fn local_round(subject: String, local: &LocalRound, probes: Probes<'_>) -> Line {
    match probes.reviewer.check(local) {
        Ok(()) if !local.is_on() => Line::ok(subject, "off, so every round is the Claude round"),
        Ok(()) => Line::ok(subject, "ready"),
        Err(reason) => Line::missing(
            subject,
            reason,
            "install it, point `review.local` at one that exists, or set its `kind` to `off`",
        ),
    }
}

fn preview(subject: String, here: Here<'_>, host: &dyn Host) -> Line {
    let tools = Tools::under(here.kelpie_home);
    let missing = tools.missing();
    if missing.is_empty() {
        let libraries = host.browser_gaps(&tools.browsers());
        if libraries.is_empty() {
            return Line::ok(
                subject,
                format!("installed under {}", tools.dir().display()),
            );
        }
        return Line::missing(
            subject,
            format!(
                "headless Chromium cannot load {}, so every run skips its screenshots, with a line only in the log",
                libraries.join(", ")
            ),
            format!(
                "run `sudo node {} install-deps chromium-headless-shell`",
                tools.playwright_cli().display()
            ),
        );
    }
    Line::missing(
        subject,
        format!(
            "the preview tools lack {}, so every run skips its screenshots, with a line only in the log",
            missing.join(", ")
        ),
        "run `shep kelpie tools install`",
    )
}
