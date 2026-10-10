//! `shep kelpie github setup`: registers kelpie's App through GitHub's app manifest flow
//!
//! Kelpie listens on a free port of 127.0.0.1 and serves one page, which
//! posts the App's manifest to GitHub's new-App page with a random `state`.
//! The maintainer creates the App there, and GitHub sends the browser back
//! to kelpie with a code and that `state`. Kelpie refuses a code that comes
//! with any other `state`, converts the right one into the App and its key,
//! keeps them in [`Apps`], and opens the page that installs the App.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use super::api::{ApiError, GithubApi};
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

/// Why setup ended without an App
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    /// The local listener failed, with the OS's reason
    Io(io::ErrorKind),
    /// GitHub would not convert the code
    Api(ApiError),
    /// The App could not be kept
    Store(StoreError),
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "kelpie's local page failed: {kind}"),
            Self::Api(e) => write!(f, "GitHub did not hand the App over: {e}"),
            Self::Store(e) => write!(f, "the App could not be kept: {e}"),
        }
    }
}

impl core::error::Error for SetupError {}

/// One run of the flow
#[derive(Debug)]
pub struct Flow<'a> {
    /// The App's name, which GitHub refuses when another App has it
    pub name: &'a str,
    /// The organization to register it under, or the maintainer's own account
    pub org: Option<&'a Owner>,
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
            Some(org) => format!("organizations/{org}/settings"),
            None => "settings".to_owned(),
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
}

/// Serves the flow on `listener` until GitHub sends back a code with the
/// flow's `state`, then converts it with `api` and keeps the App in `apps`
///
/// A code that comes with another `state`, or none, is refused and the
/// flow waits on.
///
/// # Errors
///
/// [`SetupError`] when the listener fails, GitHub will not convert the
/// code, or the App cannot be kept.
pub fn serve(
    listener: &TcpListener,
    flow: &Flow<'_>,
    api: &dyn GithubApi,
    apps: &Apps,
) -> Result<App, SetupError> {
    let port = listener
        .local_addr()
        .map_err(|e| SetupError::Io(e.kind()))?
        .port();
    loop {
        let (mut stream, _) = listener.accept().map_err(|e| SetupError::Io(e.kind()))?;
        let Some(target) = request_target(&mut stream) else {
            continue;
        };
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
        let kept = api
            .convert(code)
            .map_err(SetupError::Api)
            .and_then(|conversion| apps.save(&conversion).map_err(SetupError::Store));
        match &kept {
            Ok(app) => {
                let url = app.install_url();
                let body = format!(
                    "Kelpie keeps {}. Next, <a href=\"{}\">install it</a> on the repos kelpie works.",
                    html(&app.slug),
                    html(&url)
                );
                respond(&mut stream, 200, &body);
            }
            Err(e) => respond(&mut stream, 502, &html(&e.to_string())),
        }
        return kept;
    }
}

// The request's target, such as `/callback?code=..`, from a `GET` line.
fn request_target(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < REQUEST_MAX {
        let n = stream.read(&mut buf).ok().filter(|&n| n > 0)?;
        head.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut parts = head.lines().next()?.split(' ');
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => Some(target.to_owned()),
        _ => None,
    }
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

/// Refuses a setup for `owner` while kelpie keeps an App for it, unless `replace`
///
/// # Errors
///
/// A message naming the App kept and `--replace`, or why the Apps cannot be read.
pub fn may_register(apps: &Apps, owner: &Owner, replace: bool) -> Result<(), String> {
    match apps.get(owner) {
        Ok(Some(app)) if !replace => Err(format!(
            "kelpie already has a GitHub App for {owner}, {}. It installs from {}. \
             `--replace` registers a new one in its place",
            app.slug,
            app.install_url()
        )),
        Ok(_) => Ok(()),
        Err(_) if replace => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Runs `shep kelpie github setup [--org <org>] [--name <name>] [--replace]`
pub fn main(args: &[String]) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) if why == USAGE => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(why) => {
            eprintln!("shep kelpie github setup: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let (org, name, replace) = read_args(args)?;
    let owner = match &org {
        Some(org) => org.clone(),
        None => Owner::try_from(
            Gh.viewer()
                .map_err(|e| format!("`gh` cannot say who you are: {e}"))?
                .as_str(),
        )?,
    };
    let apps = Apps::under(&crate::home::kelpie_home()?);
    may_register(&apps, &owner, replace)?;
    let name = name.unwrap_or_else(|| format!("kelpie-{owner}"));
    let state = draw_state().map_err(|e| format!("cannot draw a random state: {e}"))?;
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let flow = Flow {
        name: &name,
        org: org.as_ref(),
        state: &state,
    };
    let start = format!("http://127.0.0.1:{port}/");
    println!("Create the App {name} on GitHub from {start}");
    println!(
        "If GitHub says the name is taken, App names being shared by all of GitHub, stop with \
         Ctrl-C and run again with `--name <another name>`."
    );
    open(&start);
    let app = serve(&listener, &flow, &CurlGithub, &apps).map_err(|e| e.to_string())?;
    println!(
        "kelpie keeps {} for {}. Install it on the repos kelpie works from {}",
        app.slug,
        app.owner,
        app.install_url()
    );
    open(&app.install_url());
    Ok(())
}

fn read_args(args: &[String]) -> Result<(Option<Owner>, Option<String>, bool), String> {
    let [sub, rest @ ..] = args else {
        return Err(USAGE.to_owned());
    };
    if sub != "setup" {
        return Err(USAGE.to_owned());
    }
    let (mut org, mut name, mut replace) = (None, None, false);
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().filter(|v| !v.trim().is_empty()).ok_or(USAGE);
        match flag.as_str() {
            "--replace" if !replace => replace = true,
            "--org" if org.is_none() => org = Some(Owner::try_from(value()?.as_str())?),
            "--name" if name.is_none() => name = Some(value()?.clone()),
            _ => return Err(USAGE.to_owned()),
        }
    }
    Ok((org, name, replace))
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
