//! The checks that hold for one project, from its settings

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Map, Value};

use super::{Here, Line, Probes, rulings};
use crate::agents::Agents;
use crate::coderabbit::CodeRabbit;
use crate::codex::Codex;
use crate::cubic::Cubic;
use crate::flock::add::LABELS;
use crate::home::{ProjectName, ProjectPaths};
use crate::local_tickets::LocalTickets;
use crate::ports::{Forge, NewLabel, Timestamp, Visibility};
use crate::review_bot::{Bot, Login, Profile};
use crate::runner::{NO_ORIGIN, check_checkout, check_instructions, source_of};
use crate::settings::{
    Account, AgentName, DoctorProbe, ForgeSlug, GatewayKeys, Gateways, Harness, Limit, PM_DECIDES,
    Pick, RoleModel, Settings, SettingsError, TicketSource,
};
use crate::webhook::KelpieSettings;

/// A line for each project folder in kelpie's home whose old settings file
/// is still there while its `[app.dogs.kelpie]` table is unset
///
/// Kelpie no longer reads the file, so the runner would stop on it.
pub(super) fn old_files(
    tables: &BTreeMap<String, Map<String, Value>>,
    kelpie_home: &Path,
    only: Option<&ProjectName>,
) -> Vec<Line> {
    let Ok(entries) = std::fs::read_dir(kelpie_home) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !tables.contains_key(name))
        .filter(|name| only.is_none_or(|n| n.as_str() == name))
        .collect();
    names.sort();
    let mut lines = Vec::new();
    for name in names {
        let file = kelpie_home.join(&name).join("settings.toml");
        if !file.is_file() {
            continue;
        }
        let what = SettingsError::Unset {
            table: format!("[app.dogs.kelpie] table on the {name} sheep"),
            old_file: Some(file),
        };
        lines.push(Line::missing(
            format!("{name}: settings"),
            what.to_string(),
            "move the file's keys into the runner's `[app.dogs.kelpie]` table, then delete the file",
        ));
    }
    lines
}

/// A line for each project whose runner would refuse to start while the old
/// home still holds its files
pub(super) fn old_home(
    tables: &BTreeMap<String, Map<String, Value>>,
    here: Here<'_>,
    only: Option<&ProjectName>,
) -> Vec<Line> {
    let Some(old) = here.old_home else {
        return Vec::new();
    };
    let named = |sheep: &&String| only.is_none_or(|n| n.as_str() == sheep.as_str());
    let mut lines = Vec::new();
    for sheep in tables.keys().filter(named) {
        let Ok(name) = ProjectName::try_from(sheep.as_str()) else {
            continue;
        };
        if let Err(what) = crate::home::runner_may_start(old, here.kelpie_home, &name) {
            lines.push(Line::missing(
                format!("{sheep}: old home"),
                what,
                format!("do what it says, then `shep kelpie start {sheep}`"),
            ));
        }
    }
    lines
}

/// The reviewers the projects in `tables` list, which a table that does not load or a
/// lineup that does not resolve adds none to: their own lines say why
pub(super) fn listed_reviewers(
    tables: &BTreeMap<String, Map<String, Value>>,
    here: Here<'_>,
    (book, gateways): (&Agents, &Gateways),
) -> BTreeSet<AgentName> {
    let mut names = BTreeSet::new();
    for (sheep, table) in tables {
        let Ok(project) = ProjectName::try_from(sheep.as_str()) else {
            continue;
        };
        let paths = ProjectPaths::under(here.kelpie_home, here.shep_home, &project);
        let Ok(settings) = Settings::from_table(table, sheep, here.home, &paths.folder) else {
            continue;
        };
        names.extend(
            settings
                .lineup(book, gateways, here.home)
                .into_iter()
                .map(|r| r.name),
        );
    }
    names
}

/// Every check for the project on `sheep`, whose table is `table` and whose
/// entry backs the names `env` with secrets in its `env`
///
/// A table that does not load is the one line: nothing else can be read from it.
pub(super) fn checks(
    sheep: &str,
    table: &Map<String, Value>,
    env: &[String],
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
    let settings = match Settings::from_table(table, sheep, here.home, &paths.folder) {
        Ok(settings) => settings,
        Err(e) => return wrong(e.to_string()),
    };
    let (line, remote) = checkout(at("checkout"), &settings);
    let base = base(at("base"), &settings, remote.as_ref(), probes.forge);
    let mut lines = vec![line, base, tickets(at("tickets"), &settings, &paths)];
    if settings.tickets.source == TicketSource::Local {
        lines.push(Line::ok(
            at("repo writes"),
            "the branch, the pull request and its comments, and nothing else",
        ));
    }
    lines.extend(super::ci::drafts(at("ci on drafts"), &settings));
    lines.extend(super::ci::pull_requests(
        at("ci on pull requests"),
        &settings,
    ));
    if settings.worker.instructions_file.is_some() {
        lines.push(instructions(at("instructions"), &settings));
    }
    lines.extend(super::e2e::line(at("e2e"), &settings, probes.host));
    if settings.code_graph.enabled {
        lines.push(code_graph(at("code graph"), &paths));
    }
    if settings.browser.enabled {
        lines.push(browser(at("browser"), &paths));
    }
    lines.extend(repo_plugins(at("repo plugins"), &settings, here));
    lines.extend(super::mcp::hosts(at("repo mcp servers"), &settings, here));
    lines.extend(super::registries::line(at("package registries"), &settings));
    let gateways = kelpie.map(KelpieSettings::gateways).unwrap_or_default();
    let (book, line) = implementers(at("implementers"), &settings, &paths, &gateways);
    lines.push(line);
    lines.extend(left_out(&at, &settings, &book, &gateways));
    lines.extend(gone_keys(at("old keys"), &settings));
    // The forge lines need the repo, and the checkout line says why when it is unknown.
    if let Some(repo) = &remote {
        let bots = bots_listed(&settings, (&book, &gateways), here.home);
        lines.extend(forge_lines(&at, repo, &settings, &bots, probes));
        let (line, covered) = super::github::app(at("github app"), repo, probes.github);
        lines.push(line);
        lines.push(super::github::workers(
            at("workers"),
            repo,
            probes.github,
            probes.host,
        ));
        lines.push(super::github::reads(at("reads"), repo, probes.github));
        lines.extend(super::github::maintainer(
            at("maintainer"),
            covered,
            &settings,
            probes.forge,
        ));
    }
    if spends_on(&settings, (&book, &gateways), here, Harness::ClaudeCode) {
        lines.push(super::machine::login(at("claude login"), env, probes.host));
    }
    lines.push(reviewers(
        at("reviewers"),
        &settings,
        (&book, &gateways, here),
        probes,
    ));
    if let Some(kelpie) = kelpie.filter(|_| spends_codex(&settings, (&book, &gateways), here)) {
        lines.push(match kelpie.codex_home(here.home, here.kelpie_home) {
            Ok(codex_home) => super::machine::codex(
                at("codex usage"),
                &*(probes.codex_meter)(&codex_home),
                probes.clock,
            ),
            Err(e) => Line::missing(at("codex usage"), e.to_string(), "set `codex_home` in kelpie's settings to a folder of kelpie's own, absolute or under `~/`"),
        });
    }
    for harness in Harness::ALL {
        let Some(probe) = harness.facts().doctor_probe else {
            continue;
        };
        if !spends_on(&settings, (&book, &gateways), here, harness) {
            continue;
        }
        match probe {
            DoctorProbe::Pi => {
                lines.push(super::machine::pi(at("pi"), probes.host));
                lines.push(super::machine::pi_tools(at("pi tools"), probes.host));
            }
            DoctorProbe::Goose => lines.push(super::machine::goose(at("goose"), probes.host)),
        }
    }
    if let Some(kelpie) = kelpie {
        lines.push(rulings::channel(at("rulings"), kelpie));
    }
    lines.extend(rulings::unnamed_kinds(at(PM_DECIDES), &settings));
    lines
}

// The branch the project's work is cut from and merged into, and whether the
// forge has it: with no repo known there is nothing to ask, and it is only named.
fn base(subject: String, settings: &Settings, repo: Option<&ForgeSlug>, forge: &dyn Forge) -> Line {
    let base = settings.git.base();
    let works = format!(
        "work items are cut from `origin/{base}`, and their pull requests open against and merge into `{base}`"
    );
    let Some(repo) = repo else {
        return Line::ok(subject, works);
    };
    match forge.branch_exists(repo, base) {
        Ok(true) => Line::ok(subject, works),
        Ok(false) => Line::missing(
            subject,
            format!("`{base}` is not a branch of {}", repo.as_str()),
            "push the branch, or set `git.base` to one the repo has",
        ),
        Err(e) => Line::unsure(
            subject,
            format!("cannot read {}'s branch `{base}`: {e}", repo.as_str()),
            format!(
                "run `gh api repos/{}/branches/{base}` to see why",
                repo.as_str()
            ),
        ),
    }
}

// The plugins the repo turns on, as a runner starting now would load them
// from the marketplaces it has fetched: nothing where the repo turns on none.
// A plugin left out never fails the run, since the runner goes on without it.
fn repo_plugins(subject: String, settings: &Settings, here: Here<'_>) -> Option<Line> {
    let plugins = crate::skills::inspect(
        &settings.git.checkout,
        settings.git.base(),
        here.kelpie_home,
        &settings.repo_setup,
    );
    if plugins.is_empty() {
        return None;
    }
    // Every plugin, so the line shows what a runner would load as well as what it would not.
    let parts: Vec<String> = (plugins.iter())
        .map(|p| match &p.skipped {
            Some(why) => format!("{} is skipped: {why}", p.plugin),
            None => format!("{} ({})", p.plugin, p.skills.join(", ")),
        })
        .collect();
    if plugins.iter().all(|p| p.skipped.is_none()) {
        return Some(Line::ok(subject, format!("loads {}", parts.join("; "))));
    }
    Some(Line::unsure(
        subject,
        parts.join("; "),
        "correct the repo's .claude/settings.json on its default branch, or let a runner fetch the marketplace by starting it",
    ))
}

// Where the project's tickets live, and for a local store how many it holds
fn tickets(subject: String, settings: &Settings, paths: &ProjectPaths) -> Line {
    match settings.tickets.source {
        TicketSource::Github => Line::ok(subject, "github: the repo's issues"),
        TicketSource::Local => {
            let folder = paths.tickets();
            match LocalTickets::new(folder.clone()).all() {
                Ok(held) => Line::ok(
                    subject,
                    format!("local: {} in {}", count(held.len()), folder.display()),
                ),
                Err(e) => Line::missing(
                    subject,
                    format!("cannot read the local tickets: {e}"),
                    "repair or remove the file it names",
                ),
            }
        }
    }
}

fn count(tickets: usize) -> String {
    match tickets {
        1 => "1 ticket".to_owned(),
        n => format!("{n} tickets"),
    }
}

// Push access, the labels, and each review bot the project lists: each read
// from `repo` on the forge.
fn forge_lines(
    at: &dyn Fn(&str) -> String,
    repo: &ForgeSlug,
    settings: &Settings,
    bots: &[Bot],
    probes: Probes<'_>,
) -> Vec<Line> {
    let slug = repo.as_str();
    let mut lines = Vec::new();
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

    // parity: review-bot.summon-by-label
    let wanted: Vec<NewLabel> = (LABELS.into_iter())
        .filter(|l| bots.contains(&Bot::Coderabbit) || l.name != crate::coderabbit::LABEL)
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
                        settings.git.checkout.display()
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

    lines.push(
        match source_of(&settings.git.checkout, probes.forge, repo.owner()) {
            Ok(source) => Line::ok(at("pull request template"), source.to_string()),
            Err(e) => Line::unsure(
                at("pull request template"),
                format!("cannot read {}'s .github repository: {e}", repo.owner()),
                format!(
                    "run `gh api repos/{}/.github/contents` to see why",
                    repo.owner()
                ),
            ),
        },
    );

    for &bot in bots {
        lines.push(review_bot(&at(bot.as_str()), repo, bot, probes));
    }
    lines
}

// The agent files, and whether the implementers the agent board may pick can be used.
// Files that cannot be read leave kelpie's own agents for the other lines.
fn implementers(
    subject: String,
    settings: &Settings,
    paths: &ProjectPaths,
    gateways: &Gateways,
) -> (Agents, Line) {
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
    let allowed = settings.allowed_implementers(&book);
    let refused = settings.refused_implementers(&book, gateways, GatewayKeys::Unheld);
    let usable: Vec<&AgentName> = (allowed.iter())
        .map(|i| &i.name)
        .filter(|name| !refused.iter().any(|(r, _)| r == *name))
        .collect();
    let line = match settings.role_agents(&book) {
        Err(e) => Line::missing(
            subject,
            e.to_string(),
            "write or correct the agent file it names, or correct the setting",
        ),
        Ok(_) if allowed.is_empty() => Line::missing(
            subject,
            "no agent file whose role is `implementer` is one `agents.allow` holds",
            "add an implementer to `agents.allow`, or write one with \
             `shep kelpie agents new implementer`",
        ),
        Ok(_) if usable.is_empty() => Line::missing(
            subject,
            "every implementer `agents.allow` holds is left out of the agent board's ask",
            "correct what each `implementer` line names",
        ),
        Ok(_) => {
            let described =
                |name: &AgentName| book.get(name).is_some_and(|a| a.fit.description.is_some());
            let bare: Vec<&AgentName> = (usable.iter().copied())
                .filter(|name| !described(name))
                .collect();
            let mut fixes = Vec::new();
            if !skipped.is_empty() {
                fixes.push(
                    "correct or rename the file it names, or move it out of kelpie's `agents` folder"
                        .to_owned(),
                );
            }
            let mut found = skipped;
            if !bare.is_empty() {
                found.extend(bare.iter().map(|name| format!("{name} has no description")));
                let files: Vec<String> = (bare.iter())
                    .map(|name| {
                        paths
                            .agents
                            .join(format!("{name}.md"))
                            .display()
                            .to_string()
                    })
                    .collect();
                fixes.push(format!("add a `description` key to {}", files.join(", ")));
            }
            if found.is_empty() {
                let names: Vec<&str> = usable.iter().map(|name| name.as_str()).collect();
                Line::ok(
                    subject,
                    format!("the agent board picks among {}", names.join(", ")),
                )
            } else {
                Line::unsure(subject, found.join("; "), fixes.join("; "))
            }
        }
    };
    (book, line)
}

// A line for each implementer the agent board is never asked about, saying why.
fn left_out(
    at: &impl Fn(&str) -> String,
    settings: &Settings,
    book: &Agents,
    gateways: &Gateways,
) -> Vec<Line> {
    let line = |(name, why): (AgentName, String)| {
        Line::unsure(
            at(&format!("implementer {name}")),
            format!("{name} is left out: {why}"),
            "correct what it names, or leave it out of `agents.allow`",
        )
    };
    let refused = settings.refused_implementers(book, gateways, GatewayKeys::Unheld);
    refused.into_iter().map(line).collect()
}

// The keys kelpie reads no longer that the project's table still holds, in one line.
fn gone_keys(subject: String, settings: &Settings) -> Option<Line> {
    let keys = settings.agents.gone_keys();
    (!keys.is_empty()).then(|| {
        let keys: Vec<String> = keys.iter().map(|key| format!("`{key}`")).collect();
        Line::unsure(
            subject,
            format!("{} read by nothing now", keys.join(", ")),
            "`shep kelpie upgrade` deletes them, as `shep kelpie settings migrate` does",
        )
    })
}

// Whether an implementer or a listed reviewer runs on an agent that spends
// Codex. Settings that do not resolve are another line's to report.
fn spends_codex(
    settings: &Settings,
    (book, gateways): (&Agents, &Gateways),
    here: Here<'_>,
) -> bool {
    let codex = |limit: &Limit| *limit == Limit::Account(Account::Codex);
    let lineup = settings.lineup(book, gateways, here.home);
    let sessions = lineup
        .iter()
        .filter_map(|r| r.runs.session())
        .any(|(_, limit)| codex(limit));
    sessions || (settings.allowed_implementers(book).iter()).any(|i| codex(&i.limit))
}

// Whether an implementer, another role agent or a reviewer runs on `harness`: one among
// a pass's candidates, or, when contributions are picked up, one that reads a
// contribution. Settings that do not resolve are another line's to report.
fn spends_on(
    settings: &Settings,
    (book, gateways): (&Agents, &Gateways),
    here: Here<'_>,
    harness: Harness,
) -> bool {
    let on_it = |model: &RoleModel| model.harness.harness() == harness;
    let roles = settings.role_agents(book).ok();
    let mut reviewers = settings.lineup(book, gateways, here.home);
    let mut security = None;
    if settings.contributions.pick != Pick::Off {
        reviewers.extend(
            settings
                .contribution_lineup(book, gateways, here.home)
                .unwrap_or_default(),
        );
        security = settings.security_reviewer(book).ok();
    }
    let reviewers = reviewers
        .iter()
        .filter_map(|r| r.runs.session())
        .chain(security.and_then(|a| a.runs.session()))
        .any(|(model, _)| on_it(model));
    reviewers
        || roles.is_some_and(|roles| roles.pm.as_ref().is_some_and(|pm| on_it(&pm.model)))
        || (settings.allowed_implementers(book).iter()).any(|i| on_it(&i.model))
}

// The checkout's line, and the repo on the forge where it is known: from
// `git.remote`, or from `origin` when that is absent.
fn checkout(subject: String, settings: &Settings) -> (Line, Option<ForgeSlug>) {
    match check_checkout(settings) {
        Ok(remote) => {
            let found = format!(
                "{} is a git checkout with an origin, and the repo is {}",
                settings.git.checkout.display(),
                remote.as_str()
            );
            (Line::ok(subject, found), Some(remote))
        }
        Err(e) => {
            let fix = match &e {
                SettingsError::Invalid {
                    setting: "git.remote",
                    ..
                } => "set `git.remote` to the repo on GitHub as `owner/name`",
                SettingsError::Invalid { reason, .. } if reason.ends_with(NO_ORIGIN) => {
                    "add an `origin` remote to the checkout, naming the repo on GitHub"
                }
                _ => "set `git.checkout` to where the checkout is, or run `shep kelpie add` in it",
            };
            let line = Line::missing(subject, e.to_string(), fix);
            (line, settings.git.remote.clone())
        }
    }
}

// A runner whose server is missing goes on without a graph, but the project turned it on.
fn code_graph(subject: String, paths: &ProjectPaths) -> Line {
    let binary = paths.tools.code_graph();
    match paths.tools.has_code_graph() {
        true => Line::ok(
            subject,
            format!("the code graph server is at {}", binary.display()),
        ),
        false => Line::missing(
            subject,
            format!(
                "the code graph server is not at {}, so the project's calls run without a code graph",
                binary.display()
            ),
            "run `shep kelpie tools install`, or turn `code_graph.enabled` off",
        ),
    }
}

// A runner whose browser is missing goes on without one, but the project turned it on.
fn browser(subject: String, paths: &ProjectPaths) -> Line {
    let tools = paths.tools.dir();
    match paths.tools.has_browser() {
        true => Line::ok(
            subject,
            format!(
                "the Playwright server and Chromium are under {}",
                tools.display()
            ),
        ),
        false => Line::missing(
            subject,
            format!(
                "the Playwright server or Chromium is not under {}, so the project's workers run \
                 without a browser",
                tools.display()
            ),
            "run `shep kelpie tools install`, or turn `browser.enabled` off",
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

// The review bots among the project's candidates, in their order.
fn bots_listed(
    settings: &Settings,
    (book, gateways): (&Agents, &Gateways),
    home: &std::path::Path,
) -> Vec<Bot> {
    let lineup = settings.lineup(book, gateways, home);
    let mut bots: Vec<Bot> = lineup
        .iter()
        .filter_map(|r| r.bot().map(|b| b.bot))
        .collect();
    bots.dedup();
    bots
}

// The runner refuses to start with CodeRabbit listed for a repo that is not
// public. Any bot that has never reviewed on the repo is likely not
// installed there, and the runner passes it over after ten minutes.
fn review_bot(subject: &str, repo: &ForgeSlug, bot: Bot, probes: Probes<'_>) -> Line {
    let name = bot.name();
    let slug = repo.as_str();
    // parity: review-bot.private-repos
    if bot == Bot::Coderabbit {
        match probes.forge.visibility(repo) {
            Ok(Visibility::Public) => {}
            Ok(_) => {
                return Line::missing(
                    subject,
                    format!(
                        "{slug} is not public, and {name}'s free plan reviews public repos only"
                    ),
                    "keep `coderabbit` out with this project's `agents.allow`, or make the repo public",
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
    }
    match probes.forge.last_bot_review(repo, login_of(bot)) {
        Ok(Some(at)) => Line::ok(
            subject,
            format!("{name} last reviewed on {slug} at {}", when(at)),
        ),
        Ok(None) => Line::missing(
            subject,
            format!(
                "{name} has never reviewed a pull request of {slug}, so it may not be installed there"
            ),
            format!(
                "install it on {slug}, or keep `{bot}` out with `agents.allow`; a repo with no reviewed pull request yet reads this way"
            ),
        ),
        Err(e) => Line::unsure(
            subject,
            format!("cannot read whether {name} is on {slug}: {e}"),
            format!("run `gh search prs --repo {slug}` to see why"),
        ),
    }
}

// The login `bot` posts under, which its profile holds.
fn login_of(bot: Bot) -> Login<'static> {
    match bot {
        Bot::Coderabbit => CodeRabbit.login(),
        Bot::Cubic => Cubic.login(),
        Bot::Codex => Codex.login(),
    }
}

// `at` as the maintainer reads a time, in UTC to the minute.
fn when(at: Timestamp) -> String {
    let seconds = i64::try_from(at.0).unwrap_or(i64::MAX);
    match jiff::Timestamp::from_second(seconds) {
        Ok(time) => time.strftime("%Y-%m-%d %H:%M UTC").to_string(),
        Err(_) => format!("{} seconds into 1970", at.0),
    }
}

// Every reviewer among the candidates can run, and the candidates a pass picks from.
fn reviewers(
    subject: String,
    settings: &Settings,
    (book, gateways, here): (&Agents, &Gateways, Here<'_>),
    probes: Probes<'_>,
) -> Line {
    // A candidate the project named in `agents.allow` names a gateway kelpie's
    // settings hold: the runner refuses a start without it, and the gateway
    // lines skip a shipped agent as shipped, so a named one is looked for here.
    let lineup = settings.lineup(book, gateways, here.home);
    if let Err(e) = gateways.check_listed_without_keys(book, lineup.iter().map(|r| &r.name)) {
        let fix = "add the gateway to kelpie's settings, or take the reviewer off `agents.allow`";
        return Line::missing(subject, e.to_string(), fix);
    }
    for reviewer in &lineup {
        // A round behind a gateway is checked on the gateway's own lines.
        let Some(round) = reviewer
            .runs
            .local()
            .filter(|_| reviewer.runs.gateway().is_none())
        else {
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
            "no reviewer file is a candidate, so a pull request goes straight to CI",
        ),
        false => Line::ok(
            subject,
            format!("each pull request is read by {}", names.join(", then ")),
        ),
    }
}
