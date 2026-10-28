//! Whether kelpie's GitHub App covers a project's repo: an App for its
//! owner, installed on it, that mints a token

use super::Line;
use crate::adapters::host::Host;
use crate::github::{GithubApp, TokenError};
use crate::ports::{Forge, InstallationToken};
use crate::settings::{ForgeSlug, Settings};

/// The line for who a ruling mentions, for a repo the App covers (`covered`,
/// as [`app`] says) and a project with no `git.maintainer`: the login `gh`
/// acts as, or that a ruling mentions no one and notifies no one when it
/// cannot be read
pub(super) fn maintainer(
    subject: String,
    covered: bool,
    settings: &Settings,
    forge: &dyn Forge,
) -> Option<Line> {
    if !covered || settings.git.maintainer.is_some() {
        return None;
    }
    Some(match forge.viewer() {
        Ok(login) => Line::ok(subject, format!("a ruling mentions @{login} (from gh)")),
        Err(e) => Line::missing(
            subject,
            format!(
                "a ruling mentions no one and notifies no one: the login gh acts as cannot be read: {e}"
            ),
            "set `git.maintainer` to your GitHub login in the project's `[app.dogs.kelpie.git]` table",
        ),
    })
}

/// The line for `repo`'s App, under `subject`, and whether the App covers
/// the repo: it minted a token, asked once so that one answer is the line's
/// and the covering's
pub(super) fn app(subject: String, repo: &ForgeSlug, github: &dyn GithubApp) -> (Line, bool) {
    let minted = github.token(repo);
    let covered = minted.is_ok();
    (line(subject, repo, minted), covered)
}

/// What each read permission is called in an App's settings
const READ_PERMISSIONS: [(&str, &str); 4] = [
    ("issues", "Issues"),
    ("pull_requests", "Pull requests"),
    ("checks", "Checks"),
    ("statuses", "Commit statuses"),
];

/// The line for who reads `repo`: kelpie's App, which has a rate budget of
/// its own, where it covers the repo and holds the read permissions, and the
/// maintainer's own `gh` login for whatever it does not
pub(super) fn reads(subject: String, repo: &ForgeSlug, github: &dyn GithubApp) -> Line {
    let slug = repo.as_str();
    match github.reader(repo) {
        Ok(issued) => {
            let lacking: Vec<&str> = (READ_PERMISSIONS.iter())
                .filter(|(name, _)| !issued.permissions.contains_key(*name))
                .map(|(_, shown)| *shown)
                .collect();
            if lacking.is_empty() {
                return Line::ok(
                    subject,
                    format!(
                        "the board, pull request, review and check reads of {slug} go through \
                         kelpie's App, which has a rate budget of its own"
                    ),
                );
            }
            let lacking = lacking.join(" and ");
            Line::unsure(
                subject,
                format!(
                    "the reads of {slug} go through kelpie's App except those that need \
                     {lacking}, which go through your own gh login"
                ),
                format!(
                    "in the App's settings on GitHub, set Repository permissions, {lacking}, to \
                     Read-only, then accept the new permission on the installation"
                ),
            )
        }
        // Not covered: the reads run as the maintainer, as before the App.
        Err(TokenError::Owner(_) | TokenError::NoApp(_) | TokenError::NotInstalled { .. }) => {
            Line::ok(
                subject,
                format!(
                    "kelpie's App does not cover {slug}, so every read goes through your own gh login"
                ),
            )
        }
        Err(TokenError::Api(e)) if e.passes() => {
            Line::unsure(subject, e.to_string(), "run doctor again")
        }
        Err(e) => Line::unsure(
            subject,
            format!(
                "every read of {slug} goes through your own gh login: no reading token mints: {e}"
            ),
            "put right what the github app line names",
        ),
    }
}

/// The line for what a worker on `repo` holds: a token that pushes its branch
/// where the App covers the repo, else the maintainer's own login, which a
/// Windows worker gets as `GH_TOKEN` and any other reads from `gh`'s config
pub(super) fn workers(
    subject: String,
    repo: &ForgeSlug,
    github: &dyn GithubApp,
    host: &dyn Host,
) -> Line {
    let slug = repo.as_str();
    match github.worker(repo) {
        Ok(worker) => {
            let workflows = match worker.workflows {
                true => {
                    "it can change a workflow file, and CI runs with the repo's secrets, so \
                     review a change to one"
                }
                false => {
                    "it cannot change a workflow file: to let it, give the App the `workflows` \
                     permission (read and write) in its settings and accept it on the installation"
                }
            };
            Line::ok(
                subject,
                format!(
                    "a worker pushes with a token for {slug} alone, which cannot comment, review, \
                     approve or merge; it can push any branch, so protect `main` on GitHub; \
                     {workflows}"
                ),
            )
        }
        // Not covered: the worker runs as the maintainer, as before the App.
        Err(TokenError::Owner(_) | TokenError::NoApp(_) | TokenError::NotInstalled { .. }) => {
            let login = match host.os() == "windows" {
                true => "a worker gets your gh login as `GH_TOKEN`",
                false => "a worker holds your own gh login",
            };
            Line::unsure(
                subject,
                format!(
                    "{login}: it can comment, approve and merge as you, and kelpie's guard only \
                     parses its commands"
                ),
                format!(
                    "install kelpie's App on {slug} (`shep kelpie github setup` registers it), \
                     so a worker holds a token for the repo that pushes and nothing more"
                ),
            )
        }
        Err(e @ TokenError::NoContents(_)) => Line::missing(
            subject,
            format!("no token mints for a worker: {e}"),
            "in the App's settings on GitHub, set Repository permissions, Contents, to Read and \
             write, then accept the new permission on the installation; or register the App \
             again with `shep kelpie github setup --replace`",
        ),
        Err(TokenError::Api(e)) if e.passes() => {
            Line::unsure(subject, e.to_string(), "run doctor again")
        }
        Err(e) => Line::missing(
            subject,
            format!("no token mints for a worker: {e}"),
            "put right what it names",
        ),
    }
}

fn line(subject: String, repo: &ForgeSlug, minted: Result<InstallationToken, TokenError>) -> Line {
    let slug = repo.as_str();
    match minted {
        Ok(_) => Line::ok(
            subject,
            format!("kelpie's App is installed on {slug} and mints a token"),
        ),
        // No App is unsure, not missing: the App is optional, and without one kelpie
        // falls back to posting from the user's own `gh` login.
        Err(TokenError::NoApp(owner)) => Line::unsure(
            subject,
            format!("kelpie has no GitHub App for {owner}"),
            format!(
                "run `shep kelpie github setup`, with `--org {owner}` when {owner} is an \
                 organization, then install the App on {slug} from the page it opens; until \
                 then kelpie posts as your own login, which GitHub never notifies you of"
            ),
        ),
        Err(TokenError::NotInstalled { app, .. }) => Line::missing(
            subject,
            format!("{} is not installed on {slug}", app.slug),
            format!("install it there from {}", app.install_url()),
        ),
        Err(TokenError::Api(e)) if e.passes() => {
            Line::unsure(subject, e.to_string(), "run doctor again")
        }
        Err(e) => Line::missing(
            subject,
            format!("no token mints: {e}"),
            "put right what it names, or register the App again with `shep kelpie github setup --replace`",
        ),
    }
}
