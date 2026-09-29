//! Letting workers and reviewers see the UI a work item builds
//!
//! A project opts in with `enabled = true` under `[preview]` in its settings,
//! and carries `.claude/launch.json` on `main`, the file Claude Desktop's
//! preview reads. Kelpie starts the named configuration's dev server itself
//! for its shots, and gives each worker the Playwright MCP server. The table
//! also names the configuration, the default routes, and the domains the app
//! really calls. Without both, nothing here runs.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::settings::NonBlank;

/// Where a work item's launch file sits, relative to its worktree
pub const LAUNCH_FILE: &str = ".claude/launch.json";

/// The hosts a preview always reaches: the dev server itself
pub const LOCAL_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

/// The `[preview]` table of a project's settings. Every key is optional.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    /// Whether the project takes shots and gives its workers the preview's
    /// tools. A launch file on `main` alone turns nothing on.
    #[serde(default)]
    pub enabled: bool,
    /// The launch configuration kelpie starts, by name. The file's first
    /// when absent.
    #[serde(default)]
    pub configuration: Option<NonBlank>,
    /// Routes every shots run captures, beside the ones the worker names.
    /// `/` when absent.
    #[serde(default = "default_routes")]
    pub routes: Vec<Route>,
    /// Domains the app calls, open to its dev server and its browser alike
    #[serde(default)]
    pub domains: Vec<NonBlank>,
}

// A settings file with no `[preview]` table at all takes this, not a derived
// default, so it captures `/` as an empty table does.
impl Default for Preview {
    fn default() -> Self {
        Self {
            enabled: false,
            configuration: None,
            routes: default_routes(),
            domains: Vec::new(),
        }
    }
}

fn default_routes() -> Vec<Route> {
    vec![Route("/".into())]
}

/// A path on the dev server, starting with `/`
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Route(String);

impl Route {
    /// The route as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Route {
    type Error = RouteError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let fits = value.starts_with('/')
            && !value.starts_with("//")
            && !value.chars().any(|c| c.is_whitespace() || c.is_control());
        if !fits {
            return Err(RouteError(value));
        }
        Ok(Self(value))
    }
}

impl From<Route> for String {
    fn from(route: Route) -> Self {
        route.0
    }
}

/// A route that does not start with a single `/`, or holds a space
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteError(pub String);

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a route: start it with one `/`, with no spaces",
            self.0
        )
    }
}

impl core::error::Error for RouteError {}

/// One configuration from `.claude/launch.json`
// wire format: changing this is a breaking change to the job file
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    /// Its name
    pub name: String,
    /// The program that starts the dev server
    pub runtime_executable: String,
    /// Its arguments
    #[serde(default)]
    pub runtime_args: Vec<String>,
    /// The local port the dev server answers on
    pub port: u16,
}

/// Why a launch configuration could not be read
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The file exists but could not be read, with the OS's reason
    Read(String),
    /// The file is not the JSON Claude Desktop reads, with the parser's reason
    Parse(String),
    /// No configuration has the settings' name, or the file lists none
    Missing(Option<String>),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "cannot read {LAUNCH_FILE}: {e}"),
            Self::Parse(e) => write!(f, "cannot parse {LAUNCH_FILE}: {e}"),
            Self::Missing(Some(name)) => write!(f, "{LAUNCH_FILE} has no configuration {name:?}"),
            Self::Missing(None) => write!(f, "{LAUNCH_FILE} lists no configuration"),
        }
    }
}

impl core::error::Error for LaunchError {}

/// Whether `repo`'s `origin/main` carries a launch file, which the preview needs
///
/// Read from `main`, not the work item's branch, so a worker cannot widen
/// its own sandbox by adding the file.
pub fn launch_file_on_main(repo: &Path) -> bool {
    let spec = format!("origin/{}:{LAUNCH_FILE}", crate::worktree::BASE);
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &spec])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The configuration named `name` in the launch file on `repo`'s
/// `origin/main`, or its first
///
/// Never the work item's branch: the worker writes that, and the dev server's
/// command would be its own to choose.
///
/// # Errors
///
/// [`LaunchError`] when the file cannot be read or parsed, or lacks the configuration.
pub fn launch(repo: &Path, name: Option<&str>) -> Result<Launch, LaunchError> {
    let spec = format!("origin/{}:{LAUNCH_FILE}", crate::worktree::BASE);
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &spec])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| LaunchError::Read(e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(LaunchError::Read(stderr.trim().to_owned()));
    }
    parse_launch(&String::from_utf8_lossy(&output.stdout), name)
}

fn parse_launch(text: &str, name: Option<&str>) -> Result<Launch, LaunchError> {
    #[derive(Deserialize)]
    struct File {
        configurations: Vec<Launch>,
    }
    let file: File = serde_json::from_str(text).map_err(|e| LaunchError::Parse(e.to_string()))?;
    let found = match name {
        Some(name) => file.configurations.into_iter().find(|c| c.name == name),
        None => file.configurations.into_iter().next(),
    };
    found.ok_or_else(|| LaunchError::Missing(name.map(str::to_owned)))
}

/// Chromium's `--host-resolver-rules`: every host fails to resolve but the
/// dev server and `domains`
///
/// A leading `*.` in a domain covers its subdomains, as in the sandbox's list.
pub fn resolver_rules<'a>(domains: impl IntoIterator<Item = &'a str>) -> String {
    let mut rules = vec!["MAP * ~NOTFOUND".to_owned()];
    for host in LOCAL_HOSTS.into_iter().chain(domains) {
        rules.push(format!("EXCLUDE {host}"));
    }
    rules.join(", ")
}

/// Kelpie's own copies of Playwright, its MCP server, the sandbox runtime
/// and a headless Chromium, under `<kelpie home>/tools`
///
/// `kelpie tools install` fills it, at the versions [`PACKAGE`] pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools(PathBuf);

/// The `package.json` kelpie installs its tools from
pub const PACKAGE: &str = include_str!("preview/package.json");

impl Tools {
    /// The tools under `kelpie_home`
    pub fn under(kelpie_home: &Path) -> Self {
        Self::at(kelpie_home.join("tools"))
    }

    /// The tools in `dir` itself
    pub fn at(dir: PathBuf) -> Self {
        Self(dir)
    }

    /// Installs the pinned packages with `npm`, then Playwright's headless Chromium
    ///
    /// # Errors
    ///
    /// The step that failed, and why.
    pub fn install(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.0)
            .and_then(|()| std::fs::write(self.0.join("package.json"), PACKAGE))
            .map_err(|e| format!("cannot write {}: {e}", self.0.display()))?;
        let npm = ["install", "--no-audit", "--no-fund"];
        run(Command::new("npm").args(npm).current_dir(&self.0))?;
        run(Command::new("node")
            .arg(self.playwright_cli())
            .args(["install", "chromium-headless-shell"])
            .env("PLAYWRIGHT_BROWSERS_PATH", self.browsers()))
    }

    /// The folder itself
    #[inline]
    pub fn dir(&self) -> &Path {
        &self.0
    }

    /// Where Playwright keeps its browsers, its `PLAYWRIGHT_BROWSERS_PATH`
    pub fn browsers(&self) -> PathBuf {
        self.0.join("browsers")
    }

    /// The Playwright MCP server's entry point
    pub fn playwright_mcp(&self) -> PathBuf {
        self.package("@playwright/mcp/cli.js")
    }

    /// Playwright's own command line, which installs browsers
    pub fn playwright_cli(&self) -> PathBuf {
        self.package("playwright-core/cli.js")
    }

    /// The sandbox runtime's command line, `srt`
    pub fn sandbox(&self) -> PathBuf {
        self.package("@anthropic-ai/sandbox-runtime/dist/cli.js")
    }

    fn package(&self, path: &str) -> PathBuf {
        self.0.join("node_modules").join(path)
    }
}

// Runs `command` with its output passed through, as an install shows progress.
fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !status.success() {
        return Err(format!("{program} failed: {status}"));
    }
    Ok(())
}

/// Appended to kelpie's instructions for a worker whose worktree has a launch file
pub const WORKER_INSTRUCTIONS: &str = include_str!("preview/worker-instructions.md");

/// Where one worker's MCP servers find what they run
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpFiles<'a> {
    /// Kelpie's tools
    pub tools: &'a Tools,
    /// The kelpie binary, which serves the shots tool
    pub kelpie: &'a Path,
    /// The shots job kelpie wrote for this work item
    pub job: &'a Path,
    /// Playwright's config file, from [`browser_config`]
    pub browser: &'a Path,
}

/// The worker's `--mcp-config`: Playwright's server, and kelpie's shots tool
pub fn mcp_config(files: McpFiles<'_>) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "playwright": {
                "command": "node",
                "args": [files.tools.playwright_mcp(), "--config", files.browser],
                "env": { "PLAYWRIGHT_BROWSERS_PATH": files.tools.browsers() },
            },
            "kelpie": {
                "command": files.kelpie,
                "args": ["shots-mcp", files.tools.dir(), files.job],
            },
        }
    })
}

/// The Playwright MCP server's config: a headless, throwaway Chromium that
/// resolves only the dev server and `domains`, writing its files to `out`
///
/// The server lets a tool reach `out` and its own working folder, resolving
/// symlinks, so `out` must be a folder the worker cannot write or replace.
pub fn browser_config(out: &Path, domains: &[NonBlank]) -> serde_json::Value {
    let rules = resolver_rules(domains.iter().map(NonBlank::as_str));
    serde_json::json!({
        "browser": {
            "browserName": "chromium",
            "isolated": true,
            "launchOptions": {
                "headless": true,
                "args": [format!("--host-resolver-rules={rules}")],
            },
        },
        "outputDir": out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_starts_with_one_slash_and_holds_no_space() {
        for good in ["/", "/events", "/events?type=raid", "/a/b#c"] {
            assert!(Route::try_from(good.to_owned()).is_ok(), "{good}");
        }
        for bad in ["", "events", "//evil.example", "/a b", "/a\nb"] {
            assert!(Route::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_absent_table_captures_the_root_and_opens_no_domain() {
        let preview: Preview = toml::from_str("").unwrap();
        assert_eq!(preview.routes, [Route("/".into())]);
        assert_eq!(preview.domains, []);
        assert_eq!(preview.configuration, None);
        assert!(!preview.enabled);
    }

    // The playground's own file, as Claude Desktop's preview reads it
    const PLAYGROUND: &str = r#"{
        "version": "0.0.1",
        "configurations": [
            { "name": "dev", "runtimeExecutable": "bun", "runtimeArgs": ["run", "dev"], "port": 3000 },
            { "name": "preview", "runtimeExecutable": "bun", "runtimeArgs": ["run", "preview"], "port": 4173 }
        ]
    }"#;

    fn worktree_with(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join(LAUNCH_FILE), text).unwrap();
        dir
    }

    #[test]
    fn the_named_configuration_or_the_first_is_read() {
        let first = parse_launch(PLAYGROUND, None).unwrap();
        assert_eq!(
            first,
            Launch {
                name: "dev".into(),
                runtime_executable: "bun".into(),
                runtime_args: vec!["run".into(), "dev".into()],
                port: 3000,
            }
        );
        assert_eq!(
            parse_launch(PLAYGROUND, Some("preview")).unwrap().port,
            4173
        );
        assert_eq!(
            parse_launch(PLAYGROUND, Some("storybook"))
                .unwrap_err()
                .to_string(),
            ".claude/launch.json has no configuration \"storybook\""
        );
    }

    #[test]
    fn the_preview_instructions_never_mention_money_or_limits() {
        let text = WORKER_INSTRUCTIONS.to_lowercase();
        for word in ["budget", "cost", "spend", "token", "usage", "$", "limit"] {
            assert!(!text.contains(word), "the instructions mention {word:?}");
        }
    }

    #[test]
    fn a_folder_that_is_not_a_repo_has_no_preview() {
        let dir = worktree_with(PLAYGROUND);
        assert!(!launch_file_on_main(dir.path()));
    }

    #[test]
    fn every_host_fails_to_resolve_but_the_dev_server_and_the_domains() {
        assert_eq!(
            resolver_rules(["leekduck.com", "*.leekduck.com"]),
            "MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1, \
             EXCLUDE leekduck.com, EXCLUDE *.leekduck.com"
        );
    }
}
