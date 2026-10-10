//! `shep kelpie github setup`: registers kelpie's App through GitHub's app manifest flow
//!
//! Kelpie listens on a free port of 127.0.0.1 and serves one page, which
//! posts the App's manifest to GitHub's new-App page with a random `state`.
//! The maintainer creates the App there, and GitHub sends the browser back
//! to kelpie with a code and that `state`. Kelpie refuses a request for any
//! other host, and a code that comes with any other `state`; it converts the
//! right one into the App and its key, keeps them in [`Apps`] when the App
//! is the asked owner's, and opens the page that installs the App.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use super::api::GithubApi;
use super::{App, Apps, Owner, StoreError};
use crate::adapters::{CurlGithub, Gh};
use crate::ports::Forge;

/// The App's homepage
const HOMEPAGE: &str = "https://github.com/shep-pm/shep-kelpie";

/// The path GitHub sends the browser back to
const CALLBACK: &str = "/callback";

/// The most of a request kelpie reads
const REQUEST_MAX: usize = 8 << 10;

/// How long a connection gets to send its request
const READ_TIMEOUT: Duration = Duration::from_secs(10);

const USAGE: &str = "usage: shep kelpie github setup [--org <org>] [--name <app name>] [--replace]";

const HELP: &str = "\
Registers kelpie's GitHub App for one repo owner, and opens the page that installs it.

  --org <org>         register it under this organization, not your own account
  --name <app name>   the App's name, `kelpie-<owner>` when left out; names are
                      unique across GitHub
  --replace           register a new App in place of the one kelpie keeps for
                      the owner; delete the old one on GitHub first, or give
                      `--name`";

/// One run of the flow
#[derive(Debug)]
pub struct Flow<'a> {
    /// The App's name, which GitHub refuses when another App has it
    pub name: &'a str,
    /// The account the App is for
    pub owner: &'a Owner,
    /// Whether the owner is an organization, named with `--org`
    pub org: bool,
    /// The value GitHub must send back, which no one else knows
    pub state: &'a str,
}

impl Flow<'_> {
    /// The manifest the page posts, sending the browser back to `port`
    pub fn manifest(&self, port: u16) -> String {
        serde_json::json!({
            "name": self.name,
            "url": HOMEPAGE,
            "redirect_url": format!("http://127.0.0.1:{port}{CALLBACK}"),
            "public": false,
            "hook_attributes": { "url": HOMEPAGE, "active": false },
            "default_permissions": {
                "issues": "write",
                "pull_requests": "write",
                "metadata": "read",
            },
        })
        .to_string()
    }

    /// GitHub's new-App page the manifest is posted to
    pub fn new_app_url(&self) -> String {
        let at = match self.org {
            true => format!("organizations/{}/settings", self.owner),
            false => "settings".to_owned(),
        };
        format!("https://github.com/{at}/apps/new?state={}", self.state)
    }

    fn page(&self, port: u16) -> String {
        format!(
            "<!doctype html><title>kelpie</title><form id=\"f\" method=\"post\" action=\"{}\">\
             <input type=\"hidden\" name=\"manifest\" value=\"{}\">\
             <button>Create kelpie's GitHub App</button></form>\
             <script>document.getElementById('f').submit()</script>",
            html(&self.new_app_url()),
            html(&self.manifest(port))
        )
    }

    // Converts `code` and keeps the App, when it is the asked owner's.
    fn keep(&self, code: &str, api: &dyn GithubApi, apps: &Apps) -> Result<App, String> {
        let conversion = api
            .convert(code)
            .map_err(|e| format!("GitHub did not hand the App over: {e}"))?;
        if !conversion.owner.eq_ignore_ascii_case(self.owner.as_str()) {
            return Err(format!(
                "GitHub registered {} for {}, not {}, so kelpie kept nothing: delete it from \
                 {}'s settings on GitHub",
                conversion.slug, conversion.owner, self.owner, conversion.owner
            ));
        }
        apps.save(&conversion)
            .map_err(|e| format!("the App could not be kept: {e}"))
    }
}

/// Serves the flow on `listener` until GitHub sends back a code with the
/// flow's `state` and the App it converts to is kept
///
/// A request for another host, a code with another `state`, and a code
/// that does not end in a kept App are each refused, and the flow waits on.
///
/// # Errors
///
/// The listener's error when it fails.
pub fn serve(
    listener: &TcpListener,
    flow: &Flow<'_>,
    api: &dyn GithubApi,
    apps: &Apps,
) -> io::Result<App> {
    let port = listener.local_addr()?.port();
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    loop {
        let (mut stream, _) = listener.accept()?;
        let Some((target, host)) = request(&mut stream) else {
            continue;
        };
        if !hosts.contains(&host) {
            respond(
                &mut stream,
                400,
                "refused: kelpie answers only on 127.0.0.1",
            );
            continue;
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path == "/" {
            respond(&mut stream, 200, &flow.page(port));
            continue;
        }
        if path != CALLBACK {
            respond(&mut stream, 404, "not found");
            continue;
        }
        let param = |name: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        };
        let plain =
            |code: &&str| !code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric());
        let code = match (param("state"), param("code").filter(plain)) {
            (Some(state), Some(code)) if state == flow.state => code,
            _ => {
                let why = "refused: this is not the answer to the setup kelpie started";
                respond(&mut stream, 400, why);
                continue;
            }
        };
        match flow.keep(code, api, apps) {
            Ok(app) => {
                let body = format!(
                    "Kelpie keeps {}. Next, <a href=\"{}\">install it</a> on the repos kelpie works.",
                    html(&app.slug),
                    html(&app.install_url())
                );
                respond(&mut stream, 200, &body);
                return Ok(app);
            }
            Err(why) => {
                eprintln!("{why}. Open http://127.0.0.1:{port}/ to try again, or Ctrl-C to stop");
                let again = "<a href=\"/\">Try again</a>";
                respond(&mut stream, 502, &format!("{}. {again}", html(&why)));
            }
        }
    }
}

// The request's target, such as `/callback?code=..`, from a `GET` line, and
// its `Host` header.
fn request(stream: &mut TcpStream) -> Option<(String, String)> {
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < REQUEST_MAX {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        head.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.lines();
    let mut parts = lines.next()?.split(' ');
    let target = match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => target.to_owned(),
        _ => return None,
    };
    let host = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("host")
            .then(|| value.trim().to_owned())
    })?;
    Some((target, host))
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Bad Gateway",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

// `text` escaped for HTML, inside an attribute's double quotes too.
fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Refuses a setup for `owner` while kelpie keeps an App for it, unless
/// `replace`, and warns of a kept App that is exposed when replacing it
///
/// # Errors
///
/// A message naming the App kept and how to replace it, or why the Apps
/// cannot be read.
pub fn may_register(apps: &Apps, owner: &Owner, replace: bool) -> Result<Option<String>, String> {
    match apps.get(owner) {
        Ok(Some(app)) if !replace => Err(format!(
            "kelpie already has a GitHub App for {owner}, {}. It installs from {}. \
             `--replace` registers a new one in its place: App names are unique on GitHub, so \
             delete {} from {owner}'s settings on GitHub first, or give the new one `--name`",
            app.slug,
            app.install_url(),
            app.slug
        )),
        Ok(_) => Ok(None),
        Err(e @ StoreError::Exposed(_)) if replace => Ok(Some(format!("warning: {e}"))),
        Err(e) if replace => Ok(Some(format!("warning: the App kept for {owner}: {e}"))),
        Err(e) => Err(e.to_string()),
    }
}

/// What the command line asked for
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Setup {
        org: Option<Owner>,
        name: Option<String>,
        replace: bool,
    },
    Help,
}

/// Arguments that are not `setup`'s
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BadArgs;

/// Runs `shep kelpie github setup [--org <org>] [--name <name>] [--replace]`
pub fn main(args: &[String]) -> ExitCode {
    let (org, name, replace) = match read_args(args) {
        Ok(Asked::Setup { org, name, replace }) => (org, name, replace),
        Ok(Asked::Help) => {
            println!("{USAGE}\n\n{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(BadArgs) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(org, name, replace) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("shep kelpie github setup: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run(org: Option<Owner>, name: Option<String>, replace: bool) -> Result<(), String> {
    let owner = match &org {
        Some(org) => org.clone(),
        None => {
            let login = Gh
                .viewer()
                .map_err(|e| format!("`gh` cannot say who you are: {e}"))?;
            Owner::try_from(login.as_str())?
        }
    };
    let apps = Apps::under(&crate::home::kelpie_home()?);
    if let Some(warning) = may_register(&apps, &owner, replace)? {
        eprintln!("{warning}");
    }
    let name = name.unwrap_or_else(|| format!("kelpie-{owner}"));
    let state = draw_state().map_err(|e| format!("cannot draw a random state: {e}"))?;
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let flow = Flow {
        name: &name,
        owner: &owner,
        org: org.is_some(),
        state: &state,
    };
    let start = format!("http://127.0.0.1:{port}/");
    println!("Create the App {name} on GitHub from {start}");
    println!(
        "If GitHub says the name is taken, App names being shared by all of GitHub, stop with \
         Ctrl-C and run again with `--name <another name>`."
    );
    open(&start);
    let app = serve(&listener, &flow, &CurlGithub, &apps)
        .map_err(|e| format!("kelpie's local page failed: {e}"))?;
    println!(
        "kelpie keeps {} for {}. Install it on the repos kelpie works from {}",
        app.slug,
        app.owner,
        app.install_url()
    );
    open(&app.install_url());
    Ok(())
}

fn read_args(args: &[String]) -> Result<Asked, BadArgs> {
    let [sub, rest @ ..] = args else {
        return Err(BadArgs);
    };
    if matches!(sub.as_str(), "--help" | "-h") || rest.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Asked::Help);
    }
    if sub != "setup" {
        return Err(BadArgs);
    }
    let (mut org, mut name, mut replace) = (None, None, false);
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().filter(|v| !v.trim().is_empty()).ok_or(BadArgs);
        match flag.as_str() {
            "--replace" if !replace => replace = true,
            "--org" if org.is_none() => {
                org = Some(Owner::try_from(value()?.as_str()).map_err(|_| BadArgs)?);
            }
            "--name" if name.is_none() => name = Some(value()?.clone()),
            _ => return Err(BadArgs),
        }
    }
    Ok(Asked::Setup { org, name, replace })
}

// 128 random bits in hex.
fn draw_state() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// Opens `url` in the maintainer's browser; the URL is printed either way.
fn open(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = crate::spawn::command(program)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn help_is_asked_for_and_wrong_flags_are_not_setup() {
        assert_eq!(read_args(&args("--help")), Ok(Asked::Help));
        assert_eq!(read_args(&args("setup --org shep-pm -h")), Ok(Asked::Help));
        assert_eq!(
            read_args(&args("setup --replace --org Shep-PM --name k")),
            Ok(Asked::Setup {
                org: Some(Owner::try_from("shep-pm").unwrap()),
                name: Some("k".to_owned()),
                replace: true,
            })
        );
        for bad in [
            "setup --org",
            "setup --org ../x",
            "setup --replace --replace",
            "install",
        ] {
            assert_eq!(read_args(&args(bad)), Err(BadArgs), "{bad}");
        }
        assert!(HELP.contains("--org") && HELP.contains("--name") && HELP.contains("--replace"));
    }
}
