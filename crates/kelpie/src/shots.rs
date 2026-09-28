//! Kelpie's shots: a work item's routes in headless Chromium, at a phone and
//! a desktop width, light and dark
//!
//! A shots run starts the launch file's dev server in a sandbox, captures
//! each route four ways, and stops the server. Whatever goes wrong is kept in
//! the run and reported, never raised: a failed run does not hold a gate.
//! The worker runs one through kelpie's MCP tool, and kelpie runs one before
//! each Claude review round and before the merge ruling.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::preview::Route;

pub mod mcp;
pub mod publish;

/// What one shots run captures, and where it may write
// wire format: changing this is a breaking change to the job file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShotsJob {
    /// The work item's worktree, where the dev server runs
    pub worktree: PathBuf,
    /// The work item's build folder, which the dev server may write
    pub build: PathBuf,
    /// The folder the shots and the dev server's log go in
    pub out: PathBuf,
    /// The launch configuration to start, or the file's first
    pub configuration: Option<String>,
    /// The routes to capture
    pub routes: Vec<Route>,
    /// Domains the app calls, besides the dev server itself
    pub domains: Vec<String>,
    /// Variables the dev server starts with, such as a tool cache in the build folder
    pub env: BTreeMap<String, PathBuf>,
}

/// A browser window's size
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Viewport {
    /// 390 by 844 at twice the density, a phone
    Mobile,
    /// 1280 by 800
    Desktop,
}

/// The colour scheme the page is told the system prefers
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    /// `prefers-color-scheme: light`
    Light,
    /// `prefers-color-scheme: dark`
    Dark,
}

/// Every viewport and scheme a route is captured in, in the order shown
pub const VARIANTS: [(Viewport, Scheme); 4] = [
    (Viewport::Mobile, Scheme::Light),
    (Viewport::Mobile, Scheme::Dark),
    (Viewport::Desktop, Scheme::Light),
    (Viewport::Desktop, Scheme::Dark),
];

/// One route captured one way
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    /// The route
    pub route: Route,
    /// The window's size
    pub viewport: Viewport,
    /// The colour scheme
    pub scheme: Scheme,
    /// The PNG, when one was taken
    pub file: Option<PathBuf>,
    /// The page's HTTP status, when it answered
    pub status: Option<u16>,
    /// What went wrong on the page: a blocked or failed request, an error
    #[serde(default)]
    pub problems: Vec<String>,
}

/// What a shots run captured, and what went wrong
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShotsRun {
    /// Each route's shots, in the job's order
    pub shots: Vec<Shot>,
    /// What went wrong outside any one page, such as a host the dev server
    /// was refused
    #[serde(default)]
    pub problems: Vec<String>,
    /// Why no page could be captured at all, when none could
    #[serde(default)]
    pub failed: Option<String>,
}

impl ShotsRun {
    /// A run that captured nothing, for `reason`
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            failed: Some(reason.into()),
            ..Self::default()
        }
    }

    /// Every PNG taken, in order
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.shots.iter().filter_map(|s| s.file.as_deref())
    }

    /// Every problem, each naming its page and window
    pub fn all_problems(&self) -> Vec<String> {
        let pages = self.shots.iter().flat_map(|shot| {
            let at = shot.label();
            shot.problems.iter().map(move |p| format!("{at}: {p}"))
        });
        self.failed
            .iter()
            .cloned()
            .chain(self.problems.iter().cloned())
            .chain(pages)
            .collect()
    }

    /// The run as the Claude review round and the worker read it
    pub fn text(&self) -> String {
        let mut text = String::new();
        if let Some(reason) = &self.failed {
            let _ = writeln!(text, "The shots run failed: {reason}");
            return text;
        }
        for shot in &self.shots {
            match &shot.file {
                Some(file) => {
                    let _ = writeln!(text, "- {}: {}", shot.label(), file.display());
                }
                None => {
                    let _ = writeln!(text, "- {}: no screenshot", shot.label());
                }
            }
        }
        let problems = self.all_problems();
        if !problems.is_empty() {
            text.push_str("\nWhat went wrong:\n");
            for problem in problems {
                let _ = writeln!(text, "- {problem}");
            }
        }
        text
    }
}

impl Shot {
    /// `/events at mobile, dark`, with its status when it was not a 200
    pub fn label(&self) -> String {
        let viewport = match self.viewport {
            Viewport::Mobile => "mobile",
            Viewport::Desktop => "desktop",
        };
        let scheme = match self.scheme {
            Scheme::Light => "light",
            Scheme::Dark => "dark",
        };
        let status = match self.status {
            Some(200) | None => String::new(),
            Some(code) => format!(" (HTTP {code})"),
        };
        format!("{} at {viewport}, {scheme}{status}", self.route.as_str())
    }
}

/// Where each of `routes`' shots goes under `out`, in [`VARIANTS`] order
pub fn plan(routes: &[Route], out: &Path) -> Vec<Shot> {
    let mut used = Vec::new();
    let mut shots = Vec::new();
    for route in routes {
        let mut slug = slug(route);
        if used.contains(&slug) {
            slug = format!("{slug}-{}", used.len());
        }
        used.push(slug.clone());
        for (viewport, scheme) in VARIANTS {
            let name = format!(
                "{slug}-{}-{}.png",
                serde_plain(viewport),
                serde_plain(scheme)
            );
            shots.push(Shot {
                route: route.clone(),
                viewport,
                scheme,
                file: Some(out.join(name)),
                status: None,
                problems: Vec::new(),
            });
        }
    }
    shots
}

fn serde_plain(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

// A file name for `route`: its letters and digits, `-` between words, `root` for `/`
fn slug(route: &Route) -> String {
    let words: Vec<&str> = route
        .as_str()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return "root".into();
    }
    words.join("-").to_lowercase()
}

/// A shots run kelpie took of one head
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShotsRecord {
    /// The head the worktree held
    pub head: String,
    /// What it captured
    pub run: ShotsRun,
    /// Whether it is on the pull request's shots comment
    #[serde(default)]
    pub posted: bool,
}

/// Routes the worker named through the shots tool, kept beside its shots
pub const NAMED_ROUTES: &str = "routes.json";

/// The routes the worker has named for its work item so far, oldest first
pub fn named_routes(shots: &Path) -> Vec<Route> {
    std::fs::read_to_string(shots.join(NAMED_ROUTES))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Adds `routes` to the ones the worker has named, keeping each once
///
/// # Errors
///
/// The OS's reason when the file cannot be written.
pub fn name_routes(shots: &Path, routes: &[Route]) -> Result<(), String> {
    let mut named = named_routes(shots);
    for route in routes {
        if !named.contains(route) {
            named.push(route.clone());
        }
    }
    let text = serde_json::to_string_pretty(&named).expect("routes are JSON");
    std::fs::create_dir_all(shots)
        .and_then(|()| std::fs::write(shots.join(NAMED_ROUTES), text))
        .map_err(|e| format!("cannot write {NAMED_ROUTES}: {}", e.kind()))
}

/// `defaults` and then `named`, each route once
pub fn routes(defaults: &[Route], named: &[Route]) -> Vec<Route> {
    let mut all = defaults.to_vec();
    for route in named {
        if !all.contains(route) {
            all.push(route.clone());
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(r: &str) -> Route {
        Route::try_from(r.to_owned()).unwrap()
    }

    #[test]
    fn each_route_is_planned_four_ways_with_its_own_file_names() {
        let shots = plan(&[route("/"), route("/events?type=raid")], Path::new("/s"));
        let files: Vec<_> = shots.iter().map(|s| s.file.clone().unwrap()).collect();
        assert_eq!(
            files,
            [
                "/s/root-mobile-light.png",
                "/s/root-mobile-dark.png",
                "/s/root-desktop-light.png",
                "/s/root-desktop-dark.png",
                "/s/events-type-raid-mobile-light.png",
                "/s/events-type-raid-mobile-dark.png",
                "/s/events-type-raid-desktop-light.png",
                "/s/events-type-raid-desktop-dark.png",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn routes_that_slug_alike_keep_apart() {
        let shots = plan(&[route("/a-b"), route("/a/b")], Path::new("/s"));
        assert_eq!(shots[0].file, Some("/s/a-b-mobile-light.png".into()));
        assert_eq!(shots[4].file, Some("/s/a-b-1-mobile-light.png".into()));
    }

    #[test]
    fn named_routes_join_the_defaults_once_each() {
        let dir = tempfile::tempdir().unwrap();
        name_routes(dir.path(), &[route("/events"), route("/")]).unwrap();
        name_routes(dir.path(), &[route("/events"), route("/raids")]).unwrap();
        let named = named_routes(dir.path());
        assert_eq!(named, [route("/events"), route("/"), route("/raids")]);
        assert_eq!(
            routes(&[route("/")], &named),
            [route("/"), route("/events"), route("/raids")]
        );
    }

    #[test]
    fn a_failed_run_says_why_and_nothing_else() {
        let run = ShotsRun::failed("port 3000 is taken");
        assert_eq!(run.text(), "The shots run failed: port 3000 is taken\n");
        assert_eq!(run.all_problems(), ["port 3000 is taken"]);
    }

    #[test]
    fn the_text_names_every_shot_and_every_problem_with_its_page() {
        let mut shots = plan(&[route("/events")], Path::new("/s"));
        shots[1].status = Some(500);
        shots[1].problems = vec!["blocked https://cdn.example/a.png: not a preview domain".into()];
        shots[3].file = None;
        let run = ShotsRun {
            shots,
            problems: vec!["the dev server was refused registry.npmjs.org".into()],
            failed: None,
        };
        assert_eq!(
            run.text(),
            "- /events at mobile, light: /s/events-mobile-light.png\n\
             - /events at mobile, dark (HTTP 500): /s/events-mobile-dark.png\n\
             - /events at desktop, light: /s/events-desktop-light.png\n\
             - /events at desktop, dark: no screenshot\n\
             \n\
             What went wrong:\n\
             - the dev server was refused registry.npmjs.org\n\
             - /events at mobile, dark (HTTP 500): blocked https://cdn.example/a.png: not a preview domain\n"
        );
    }
}
