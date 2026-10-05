//! The checks that hold for one project, from its settings

use serde_json::{Map, Value};

use super::{Here, Line, Probes, rulings};
use crate::agents::Agents;
use crate::flock::add::LABELS;
use crate::ports::{NewLabel, Visibility};
use crate::runner::{ProjectName, ProjectPaths, SUMMON_LABEL, check_instructions, check_repo};
use crate::settings::{Account, Limit, Settings};
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
        Ok(name) => ProjectPaths::under(here.kelpie_home, here.shep_home, &name),
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
    let (book, line) = implementers(at("implementers"), &settings, &paths);
    lines.push(line);

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
    lines.push(reviewers(at("reviewers"), &settings, (&book, here), probes));
    if let Some(kelpie) = kelpie.filter(|_| spends_codex(&settings, &book, here)) {
        lines.push(match kelpie.codex_home(here.home, here.kelpie_home) {
            Ok(codex_home) => super::machine::codex(
                at("codex usage"),
                &*(probes.codex_meter)(&codex_home),
                probes.clock,
            ),
            Err(e) => Line::missing(at("codex usage"), e.to_string(), "set `codex_home` in kelpie's settings to a folder of kelpie's own, absolute or under `~/`"),
        });
    }
    if let Some(kelpie) = kelpie {
        lines.push(rulings::channel(at("rulings"), kelpie));
    }
    lines
}

// The agent files, and whether the project's implementers can be used.
// Files that cannot be read leave kelpie's own agents for the other lines.
fn implementers(subject: String, settings: &Settings, paths: &ProjectPaths) -> (Agents, Line) {
    let book = match Agents::load(&paths.agents) {
        Ok(book) => book,
        Err(e) => {
            let fix = "correct the file it names, or move it out of kelpie's `agents` folder";
            return (
                Agents::embedded(),
                Line::missing(subject, e.to_string(), fix),
            );
        }
    };
    let skipped: Vec<String> = book.skipped().collect();
    let line = match settings.role_agents(&book) {
        Ok(_) if !skipped.is_empty() => Line::unsure(
            subject,
            skipped.join("; "),
            "rename it to the agent's name, or move it out of kelpie's `agents` folder",
        ),
        Ok(roles) => Line::ok(
            subject,
            format!(
                "an issue with no `agent:` label runs on {}",
                roles.default_implementer.name
            ),
        ),
        Err(e) => Line::missing(
            subject,
            e.to_string(),
            "write the agent file it names, or correct `agents.implementers`",
        ),
    };
    (book, line)
}

// Whether an implementer or a listed reviewer runs on an agent that spends
// Codex. Settings that do not resolve are another line's to report.
fn spends_codex(settings: &Settings, book: &Agents, here: Here<'_>) -> bool {
    let codex = |limit: &Limit| *limit == Limit::Account(Account::Codex);
    let roles = settings.role_agents(book).ok();
    let lineup = settings.lineup(book, here.home).unwrap_or_default();
    let sessions = lineup
        .iter()
        .filter_map(|r| r.runs.session())
        .any(|(_, limit)| codex(limit));
    sessions || roles.is_some_and(|roles| roles.implementers.iter().any(|i| codex(&i.limit)))
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

// Every local reviewer the project lists can run.
fn reviewers(
    subject: String,
    settings: &Settings,
    (book, here): (&Agents, Here<'_>),
    probes: Probes<'_>,
) -> Line {
    let lineup = match settings.lineup(book, here.home) {
        Ok(lineup) => lineup,
        Err(e) => {
            return Line::missing(
                subject,
                e.to_string(),
                "write the reviewer's file in kelpie's `agents` folder, or take it off \
                 `agents.reviewers`",
            );
        }
    };
    for reviewer in &lineup {
        let Some(round) = reviewer.runs.local() else {
            continue;
        };
        if let Err(reason) = probes.reviewer.check(&round) {
            let fix = "install it, or point its agent file at one that exists";
            return Line::missing(subject, format!("{}: {reason}", reviewer.name), fix);
        }
    }
    let names: Vec<&str> = lineup.iter().map(|r| r.name.as_str()).collect();
    match names.is_empty() {
        true => Line::ok(
            subject,
            "none listed, so a pull request goes straight to CI",
        ),
        false => Line::ok(
            subject,
            format!("each pull request is read by {}", names.join(", then ")),
        ),
    }
}
