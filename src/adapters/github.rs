//! The GitHub App's calls over `curl`, and its signer over `openssl`
//!
//! Each call reaches curl as a config on its stdin, as the webhook's do, so
//! no process listing shows a JWT. `openssl` reads the key from its file and
//! the JWT's header and claims from its stdin; the key never passes through
//! kelpie's memory or any variable.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use super::curl;
use crate::github::api::{self, ApiError, Conversion, GithubApi, IssuedToken, Signer};
use crate::github::tokens::Jwt;
use crate::ports::AlertError;
use crate::settings::ForgeSlug;

const API: &str = "https://api.github.com";

/// Seconds curl gets to connect, and to finish the whole call
const CONNECT_TIMEOUT: u32 = 5;
const MAX_TIME: u32 = 30;

/// The App's calls, made with the system's `curl`
#[derive(Debug, Clone, Copy, Default)]
pub struct CurlGithub;

impl GithubApi for CurlGithub {
    fn convert(&self, code: &str) -> Result<Conversion, ApiError> {
        let url = format!("{API}/app-manifests/{code}/conversions");
        match call(&url, None, true)? {
            (body, 201) => api::parse_conversion(&body),
            (_, status) => Err(ApiError::Refused(status)),
        }
    }

    fn installation(&self, jwt: &Jwt, repo: &ForgeSlug) -> Result<Option<u64>, ApiError> {
        let url = format!("{API}/repos/{}/installation", repo.as_str());
        match call(&url, Some(jwt), false)? {
            (body, 200) => api::parse_installation(&body).map(Some),
            (_, 404) => Ok(None),
            (_, status) => Err(ApiError::Refused(status)),
        }
    }

    fn access_token(&self, jwt: &Jwt, installation: u64) -> Result<IssuedToken, ApiError> {
        let url = format!("{API}/app/installations/{installation}/access_tokens");
        match call(&url, Some(jwt), true)? {
            (body, 201) => api::parse_token(&body),
            (_, status) => Err(ApiError::Refused(status)),
        }
    }
}

// One call to `url`, a `POST` with an empty body when `post`, and what it
// answered with its status.
fn call(url: &str, jwt: Option<&Jwt>, post: bool) -> Result<(String, u16), ApiError> {
    let mut lines = vec![
        ("url", url.to_owned()),
        ("proto", "=https".to_owned()),
        ("connect-timeout", CONNECT_TIMEOUT.to_string()),
        ("max-time", MAX_TIME.to_string()),
        ("header", "Accept: application/vnd.github+json".to_owned()),
        ("header", "X-GitHub-Api-Version: 2022-11-28".to_owned()),
        ("user-agent", "shep-kelpie".to_owned()),
        ("write-out", "\n%{http_code}".to_owned()),
    ];
    if let Some(jwt) = jwt {
        lines.push(("header", format!("Authorization: Bearer {}", jwt.expose())));
    }
    if post {
        lines.push(("data-raw", String::new()));
    }
    curl::run(&curl::render(&lines)).map_err(|e| match e {
        AlertError::Unreachable(code) => ApiError::Unreachable(format!("curl exited {code}")),
        e => ApiError::Unreachable(e.to_string()),
    })
}

/// Signs with the system's `openssl`, which reads the key from its file
#[derive(Debug, Clone, Copy, Default)]
pub struct Openssl;

impl Signer for Openssl {
    fn sign(&self, key: &Path, input: &[u8]) -> Result<Vec<u8>, String> {
        let mut child = crate::spawn::command("openssl")
            .args(["dgst", "-sha256", "-sign"])
            .arg(key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run openssl: {e}"))?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let written = stdin.write_all(input);
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|e| format!("cannot run openssl: {e}"))?;
        match written {
            Ok(()) if output.status.success() && !output.stdout.is_empty() => Ok(output.stdout),
            _ => Err(format!(
                "openssl could not sign with {}: {}",
                key.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        }
    }
}
