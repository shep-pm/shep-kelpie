//! Kelpie's own settings: webhook, lease capacities, GPU metrics page, Codex login, model gateways
//!
//! Kelpie's `[kelpie]` section of `dogs.toml`, or the file under kelpie's
//! home it had before one, shared by every project. Every part is
//! optional: the `webhook` table, whose `kind` and `url` are required, is the
//! one way a ruling is posted, and with none rulings reach the maintainer only
//! in the log, `status` and `shep kelpie rule`. The URL and the `token` are
//! credentials, and the `ca_cert` path is the maintainer's, so no error, log
//! line or status carries any of them.
//! `kelpie-settings.example.toml` beside this crate shows the section.

use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;
use shep_client::dogs::dog_config;

use crate::forwarder::Upstream;
use crate::lease::counted::CARGO_TEST_CAPACITY;
use crate::settings::{EndpointUrl, Gateway, GatewayName, Gateways, SettingsError};

/// What every project shares
#[dog_config]
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KelpieSettings {
    /// Where rulings are posted, so the maintainer hears of them away from
    /// the terminal
    #[serde(default)]
    pub webhook: Option<Webhook>,
    /// How many commands may hold each counted lease at once
    #[serde(default)]
    pub leases: Leases,
    /// The GPU's Prometheus metrics page, such as `nvidia_gpu_exporter`'s
    /// `/metrics`, which `status` reads. No GPU figures when absent.
    #[serde(default)]
    pub gpu_metrics_url: Option<EndpointUrl>,
    /// The folder holding kelpie's own Codex login, `<kelpie home>/codex`
    /// when absent. A leading `~/` is the home folder.
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    /// Model gateways such as paddock, by name, which an agent file
    /// names as `gateway` in place of its server's `url`. A runner reads
    /// them when it starts.
    #[serde(default)]
    pub gateways: BTreeMap<GatewayName, Gateway>,
}

/// The counted leases' capacities
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Leases {
    /// How many commands may hold `cargo-test` at once, as set: `None`
    /// when absent, which [`Leases::cargo_test_capacity`] reads as 3
    #[serde(default, rename = "cargo-test")]
    pub cargo_test: Option<NonZeroU32>,
}

impl Leases {
    /// How many commands may hold `cargo-test` at once
    pub fn cargo_test_capacity(&self) -> NonZeroU32 {
        self.cargo_test.unwrap_or(CARGO_TEST_CAPACITY)
    }
}

/// The maintainer's webhook
#[dog_config]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    /// What kind of service it is, which decides the post's shape
    pub kind: WebhookKind,
    /// Where to post
    #[shep(secret)]
    pub url: WebhookUrl,
    /// A private CA's certificate to trust the server by, for a self-hosted
    /// ntfy behind its own certificate
    #[serde(default)]
    #[shep(secret)]
    pub ca_cert: Option<CaCert>,
    /// An ntfy access token, sent as a bearer token on the post and the poll
    /// of an ntfy webhook and never to a Discord one
    #[serde(default)]
    #[shep(secret)]
    pub token: Option<WebhookToken>,
    /// The maintainer's word that only their `token` can post to this ntfy
    /// topic, so a reply on it is theirs without a code. Needs a `token`. A
    /// yes on a merge ruling still takes the code.
    #[serde(default)]
    pub trusted: bool,
}

/// The path of a CA certificate file: it names where the maintainer keeps
/// things, so `Debug` and every error leave it out
#[derive(Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "PathBuf")]
pub struct CaCert(PathBuf);

impl CaCert {
    /// The path, for the adapter that gives it to curl and the check that reads it
    #[inline]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for CaCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CaCert(..)")
    }
}

impl TryFrom<PathBuf> for CaCert {
    type Error = &'static str;

    // Curl would read a relative path from wherever the runner stands, and
    // the dog's check from wherever the dog does.
    fn try_from(value: PathBuf) -> Result<Self, Self::Error> {
        if !value.is_absolute() {
            return Err("must be an absolute path");
        }
        Ok(Self(value))
    }
}

/// An ntfy access token: a credential, since it reads and posts to the topics it covers
///
/// `Debug` does not leak the token, and nothing else prints it.
#[derive(Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct WebhookToken(String);

impl WebhookToken {
    /// The token, for the adapter that sends it and nothing else
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WebhookToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookToken(..)")
    }
}

impl TryFrom<String> for WebhookToken {
    type Error = &'static str;

    // A token is one word of printable characters, so it cannot end the
    // header line it goes in.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("must be one word of printable characters");
        }
        Ok(Self(value))
    }
}

/// A service kelpie can post an alert to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WebhookKind {
    /// A Discord channel's webhook, which takes JSON
    Discord,
    /// An ntfy topic, which takes the text as the body
    Ntfy,
}

/// A webhook's URL: a credential, since anyone holding it can post
///
/// `Debug` does not leak the URL, and nothing else prints it.
#[derive(Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct WebhookUrl(String);

impl WebhookUrl {
    /// The URL, for the adapter that posts to it and nothing else
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WebhookUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebhookUrl(..)")
    }
}

impl TryFrom<String> for WebhookUrl {
    type Error = &'static str;

    // Plain `http` is only for a stand-in on this machine or a host on a
    // tailnet, whose own tunnel encrypts it: anything else would send the
    // question, and the URL's secret, in the clear. The whole host and port
    // are checked, since `127.0.0.1@elsewhere` names elsewhere.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let allowed = match value.split_once("://") {
            Some(("https", rest)) => has_host(authority(rest)),
            Some(("http", rest)) => private_host(authority(rest)),
            _ => false,
        };
        if !allowed || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("must be an `https://` URL with no spaces");
        }
        Ok(Self(value))
    }
}

// The host and port: what comes before the path, query or fragment.
fn authority(rest: &str) -> &str {
    rest.split(['/', '?', '#']).next().unwrap_or_default()
}

// Whether `authority` names a host, after any userinfo and before any port: `:8700` does not.
fn has_host(authority: &str) -> bool {
    let host_and_port = authority.rsplit('@').next().unwrap_or_default();
    !host_and_port.is_empty() && !host_and_port.starts_with(':')
}

// Whether `authority` is this machine or a tailnet host, on a port that is
// absent or all digits. Userinfo (`@`) is refused outright.
fn private_host(authority: &str) -> bool {
    if authority.contains('@') {
        return false;
    }
    if authority == "[::1]" || authority.strip_prefix("[::1]:").is_some_and(is_port) {
        return true;
    }
    let (host, port_ok) = match authority.split_once(':') {
        Some((host, port)) => (host, is_port(port)),
        None => (authority, true),
    };
    port_ok && (host == "localhost" || is_local_or_tailnet_address(host) || is_tailnet_name(host))
}

fn is_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
}

// The loopback address, or one in Tailscale's block, the CGNAT range
// 100.64.0.0/10.
fn is_local_or_tailnet_address(host: &str) -> bool {
    host.parse::<Ipv4Addr>().is_ok_and(|ip| {
        let [a, b, ..] = ip.octets();
        host == "127.0.0.1" || (a == 100 && b & 0b1100_0000 == 0b0100_0000)
    })
}

// A name under `.ts.net`, with at least one label before it.
fn is_tailnet_name(host: &str) -> bool {
    host.len() > ".ts.net".len()
        && host.is_ascii()
        && host.to_ascii_lowercase().ends_with(".ts.net")
        && host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// What a malformed file is told, since the parser's own message could quote the URL
const SHAPE: &str = "it takes a `[webhook]` table with `kind` (`discord` or `ntfy`) \
                     and an `https://` `url` (`http://` on a tailnet), an optional absolute \
                     `ca_cert` path, an optional one-word `token` and `trusted = true` with one, \
                     a `[leases]` table with a \
                     `cargo-test` count, a `gpu_metrics_url`, a `codex_home` path, \
                     `[gateways.<name>]` tables with `url` and `key_env`, and nothing else";

/// Why `trusted` is refused without a `token`
const NEEDS_TOKEN: &str = "needs a `token`: without one anyone who learns the topic's name \
                           can post to it, so a reply on it proves nothing";

impl KelpieSettings {
    /// Checks the webhook's `ca_cert` in the section's text: an absolute path
    /// to a file with a PEM certificate in it
    ///
    /// It reads the text and not the parsed settings, so a mistake elsewhere
    /// in the section cannot hide a certificate that curl would then fail on.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `webhook.ca_cert` and never the path.
    pub fn check_ca_cert(section: &str) -> Result<(), SettingsError> {
        let invalid = |reason: &str| SettingsError::Invalid {
            setting: "webhook.ca_cert",
            reason: reason.into(),
        };
        let Some(set) =
            section
                .parse::<toml::Table>()
                .ok()
                .and_then(|table| match table.get("webhook") {
                    Some(toml::Value::Table(webhook)) => webhook.get("ca_cert").cloned(),
                    _ => None,
                })
        else {
            return Ok(());
        };
        let toml::Value::String(path) = set else {
            return Err(invalid("must be a path"));
        };
        let ca = CaCert::try_from(PathBuf::from(path)).map_err(invalid)?;
        let bytes = std::fs::read(ca.path())
            .map_err(|e| invalid(&format!("cannot be read ({})", e.kind())))?;
        if !String::from_utf8_lossy(&bytes).contains("-----BEGIN CERTIFICATE-----") {
            return Err(invalid("must be a PEM file with a certificate in it"));
        }
        Ok(())
    }

    /// Checks in the section's text that a webhook with `trusted = true` has a valid `token`
    ///
    /// A valid token is a non-empty word of printable characters. Kelpie cannot
    /// see the server's access rules, so `trusted` is the maintainer's word that
    /// the topic is locked to the token. It reads the text and not the parsed
    /// settings, so a mistake elsewhere in the section cannot hide it.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `webhook.trusted`.
    pub fn check_trusted(section: &str) -> Result<(), SettingsError> {
        let trusted =
            section
                .parse::<toml::Table>()
                .ok()
                .and_then(|table| match table.get("webhook") {
                    Some(toml::Value::Table(webhook)) => Some((
                        webhook.get("trusted") == Some(&toml::Value::Boolean(true)),
                        matches!(
                            webhook.get("token"),
                            Some(toml::Value::String(token))
                                if WebhookToken::try_from(token.clone()).is_ok()
                        ),
                    )),
                    _ => None,
                });
        match trusted {
            Some((true, false)) => Err(SettingsError::Invalid {
                setting: "webhook.trusted",
                reason: NEEDS_TOKEN.into(),
            }),
            _ => Ok(()),
        }
    }

    /// The gateways, with each key read from kelpie's environment
    pub fn gateways(&self) -> Gateways {
        Gateways::new(self.gateways.clone())
    }

    /// The folder holding kelpie's own Codex login, which every Codex call
    /// signs in from and no call reads otherwise
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] when `codex_home` is a relative path, the
    /// home folder or a folder above it, which every call would then be
    /// denied, or the maintainer's own `~/.codex` or a folder in it.
    pub fn codex_home(&self, home: &Path, kelpie_home: &Path) -> Result<PathBuf, SettingsError> {
        let Some(set) = &self.codex_home else {
            return Ok(kelpie_home.join("codex"));
        };
        let path = match set.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => set.clone(),
        };
        let invalid = |reason: &str| SettingsError::Invalid {
            setting: "codex_home",
            reason: reason.into(),
        };
        if path.is_relative() {
            return Err(invalid("must be an absolute path or start with `~/`"));
        }
        if home.starts_with(&path) {
            return Err(invalid(
                "must be a folder of its own, not the home folder or one above it",
            ));
        }
        if path.starts_with(home.join(".codex")) {
            return Err(invalid(
                "must not be your own `~/.codex`: kelpie signs in to Codex apart from you",
            ));
        }
        Ok(path)
    }

    /// Reads and checks kelpie's `[kelpie]` section, as shep hands it over
    ///
    /// # Errors
    ///
    /// [`SettingsError::Section`] naming the line that is wrong, never its text.
    pub fn from_section(text: &str) -> Result<Self, SettingsError> {
        Self::parse(text).map_err(|message| SettingsError::Section { message })
    }

    fn parse(text: &str) -> Result<Self, String> {
        let removed = [
            crate::settings::Removed {
                key: "ruling_channels",
                because: "the relay is gone",
                fix: crate::settings::DELETE,
            },
            crate::settings::Removed {
                key: "agents",
                because: "agents are files in kelpie's home's `agents` folder",
                fix: "write each `[agents.<name>]` table as `agents/<name>.md`, its keys \
                      as YAML frontmatter with `role: implementer`, and delete the tables",
            },
            crate::settings::Removed {
                key: "local_reviewers",
                because: "reviewers are agent files in kelpie's home's `agents` folder",
                fix: "write each `[local_reviewers.<name>]` table as `agents/<name>.md` in \
                      kelpie's home, with `role: reviewer`, its `kind` as `harness`; a `kind = \"claude\"` or \
                      `\"session\"` table is a session reviewer on its model and effort \
                      whose body is its prompt. Then delete the tables",
            },
            crate::settings::Removed {
                key: "reviewers",
                because: "review bots are reviewer agent files on the `bot` harness, which \
                          hold each bot's window",
                fix: "write each `[reviewers.<bot>]` table's `reviews` and `hours`, and \
                      Codex's `reviews_on_ready`, into `agents/<bot>.md` in kelpie's home, \
                      which `shep kelpie add` writes out, and delete the tables",
            },
        ];
        crate::settings::refuse_removed(text, &removed, &[])?;
        let read: Self = toml::from_str(text).map_err(|e: toml::de::Error| {
            let line = e
                .span()
                .map(|span| text[..span.start].matches('\n').count() + 1);
            match line {
                Some(line) => format!("line {line} is not right: {SHAPE}"),
                None => SHAPE.to_owned(),
            }
        })?;
        if read
            .webhook
            .as_ref()
            .is_some_and(|w| w.trusted && w.token.is_none())
        {
            return Err(format!("`webhook.trusted` {NEEDS_TOKEN}"));
        }
        // Every call to a gateway goes through kelpie's forwarder or comes from kelpie itself.
        for (name, gateway) in &read.gateways {
            Upstream::new(&gateway.base())
                .map_err(|e| format!("`gateways.{name}.url` cannot be used: {e}"))?;
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The example's section as shep hands it over: re-rooted, so `[webhook]`.
    fn example() -> String {
        include_str!("../../../kelpie-settings.example.toml")
            .replace("[kelpie]\n", "")
            .replace("[kelpie.webhook]", "[webhook]")
    }

    // A URL no test would print by chance, so finding it anywhere is a leak
    const SECRET: &str = "https://ntfy.example.invalid/kelpie-s3cr3t-topic";

    fn parse_err(text: &str) -> String {
        KelpieSettings::parse(text).expect_err("the settings should be refused")
    }

    #[test]
    fn the_example_reads() {
        let s = KelpieSettings::parse(&example()).unwrap();
        let webhook = s.webhook.expect("the example names a webhook");
        assert_eq!(webhook.kind, WebhookKind::Ntfy);
        assert!(webhook.url.expose().starts_with("https://ntfy.sh/"));
    }

    #[test]
    fn the_webhook_is_optional() {
        let s = KelpieSettings::parse("").unwrap();
        assert_eq!(s, KelpieSettings::default());
        assert_eq!(s.webhook, None);
    }

    #[test]
    fn ruling_channels_is_refused_by_name_without_quoting_the_file() {
        let text =
            format!("ruling_channels = []\n[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\n");
        let err = parse_err(&text);
        assert!(
            err.starts_with("`ruling_channels` is no longer a setting"),
            "{err}"
        );
        assert!(err.ends_with("delete it"), "{err}");
        assert!(!err.contains("s3cr3t"), "{err}");
    }

    #[test]
    fn local_reviewers_are_refused_saying_they_are_agent_files_now() {
        let text = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\n\
             [local_reviewers.qwen]\nkind = \"command\"\ncommand = \"~/q.sh\"\n"
        );
        let err = parse_err(&text);
        assert!(
            err.starts_with(
                "`local_reviewers` is no longer a setting, because reviewers are agent files \
                 in kelpie's home's `agents` folder: write each"
            ),
            "{err}"
        );
        assert!(err.contains("`role: reviewer`"), "{err}");
        assert!(!err.contains("lease: gpu"), "{err}");
        assert!(err.contains("whose body is its prompt"), "{err}");
        assert!(!err.contains("s3cr3t"), "{err}");
    }

    #[test]
    fn kelpies_reviewer_windows_are_refused_saying_they_are_bot_files_now() {
        let text = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\n\
             [reviewers.cubic]\nreviews = 20\nhours = 720\n"
        );
        assert_eq!(
            parse_err(&text),
            "`reviewers` is no longer a setting, because review bots are reviewer agent \
             files on the `bot` harness, which hold each bot's window: write each \
             `[reviewers.<bot>]` table's `reviews` and `hours`, and Codex's \
             `reviews_on_ready`, into `agents/<bot>.md` in kelpie's home, which \
             `shep kelpie add` writes out, and delete the tables"
        );
    }

    #[test]
    fn kelpies_agents_tables_are_refused_saying_they_are_files_now() {
        let text = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\n\
             [agents.qwen]\nharness = \"pi\"\nmodel = \"m\"\neffort = \"low\"\n"
        );
        assert_eq!(
            parse_err(&text),
            "`agents` is no longer a setting, because agents are files in kelpie's home's \
             `agents` folder: write each `[agents.<name>]` table as `agents/<name>.md`, its \
             keys as YAML frontmatter with `role: implementer`, and delete the tables"
        );
    }

    // A derived Debug would print the URL wherever settings are debugged.
    #[test]
    fn debug_does_not_leak_the_url() {
        assert!(example().contains("https://ntfy.sh/your-private-topic"));
        let text = example().replace("https://ntfy.sh/your-private-topic", SECRET);
        let s = KelpieSettings::parse(&text).unwrap();
        let url = s.webhook.as_ref().unwrap().url.clone();
        assert_eq!(format!("{url:?}"), "WebhookUrl(..)");
        assert!(!format!("{s:?}").contains("s3cr3t"), "{s:?}");
    }

    #[test]
    fn a_malformed_file_names_the_line_and_never_quotes_it() {
        let cases = [
            format!("[webhook]\nkind = \"slack\"\nurl = \"{SECRET}\"\n"),
            format!("[webhook]\nkind = \"{SECRET}\"\nurl = \"{SECRET}\"\n"),
            format!("[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET} x\"\n"),
            format!("[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\ntoken = 7\n"),
            format!("[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\nca_cert = 7\n"),
            format!("[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\ntoken = \"a b\"\n"),
            format!("[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\nbearer = \"x\"\n"),
            format!(
                "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
                SECRET.replace("https", "http")
            ),
            format!("url = \"{SECRET}\"\n"),
        ];
        for text in cases {
            let err = parse_err(&text);
            assert!(!err.contains("s3cr3t"), "{err}");
            assert!(err.contains(SHAPE), "{err}");
        }
        let err = parse_err(&format!(
            "[webhook]\nkind = \"slack\"\nurl = \"{SECRET}\"\n"
        ));
        assert!(err.starts_with("line 2 is not right"), "{err}");
        let err = parse_err(&format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\nbearer = \"x\"\n"
        ));
        assert!(err.starts_with("line 4 is not right"), "{err}");
    }

    // A self-hosted ntfy: http on a tailnet, a private CA and an access token.
    const SELF_HOSTED: &str = "[webhook]\nkind = \"ntfy\"\n\
                               url = \"http://ntfy.tail1234.ts.net/kelpie-s3cr3t-topic\"\n\
                               ca_cert = \"/etc/kelpie/s3cr3t-ca.pem\"\n\
                               token = \"tk_s3cr3t0123456789\"\n";

    #[test]
    fn a_self_hosted_ntfys_ca_and_token_are_read() {
        let webhook = KelpieSettings::parse(SELF_HOSTED).unwrap().webhook.unwrap();
        assert_eq!(
            webhook.ca_cert.as_ref().map(CaCert::path),
            Some(Path::new("/etc/kelpie/s3cr3t-ca.pem"))
        );
        assert_eq!(
            webhook.token.as_ref().map(WebhookToken::expose),
            Some("tk_s3cr3t0123456789")
        );
        let bare = KelpieSettings::parse(&example()).unwrap().webhook.unwrap();
        assert_eq!((bare.ca_cert, bare.token), (None, None));
    }

    #[test]
    fn neither_the_token_nor_the_ca_path_shows_in_debug_or_an_error() {
        let s = KelpieSettings::parse(SELF_HOSTED).unwrap();
        let shown = format!("{s:?} {:#?}", s.webhook);
        assert!(!shown.contains("s3cr3t"), "{shown}");

        let bad_token = SELF_HOSTED.replace("tk_s3cr3t0123456789", "tk_s3cr3t 01");
        let bad_ca = SELF_HOSTED.replace("/etc/kelpie/s3cr3t-ca.pem", "s3cr3t-ca.pem");
        for text in [bad_token, bad_ca] {
            let err = parse_err(&text);
            assert!(!err.contains("s3cr3t"), "{err}");
        }
        let err = KelpieSettings::from_section(&SELF_HOSTED.replace("tk_s3cr3t0123456789", "a b"))
            .unwrap_err()
            .to_string();
        assert!(!err.contains("s3cr3t"), "{err}");

        // The file is not there, and the error says which key, not where
        let err = KelpieSettings::check_ca_cert(SELF_HOSTED)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("s3cr3t"), "{err}");
    }

    #[test]
    fn a_ca_cert_that_is_not_a_readable_certificate_is_refused_naming_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        let with = |path: &Path| {
            SELF_HOSTED.replace("/etc/kelpie/s3cr3t-ca.pem", &path.display().to_string())
        };
        let refused = |text: &str| {
            let err = KelpieSettings::check_ca_cert(text).unwrap_err().to_string();
            assert!(err.contains("`webhook.ca_cert`"), "{err}");
            assert!(!err.contains(&dir.path().display().to_string()), "{err}");
        };

        refused(&with(&ca));
        // A folder, which curl cannot read as a certificate
        refused(&with(dir.path()));
        // Empty, and something else
        std::fs::write(&ca, "").unwrap();
        refused(&with(&ca));
        std::fs::write(&ca, "-----BEGIN PRIVATE KEY-----\n").unwrap();
        refused(&with(&ca));
        // A path that is not absolute or not a string
        refused(&SELF_HOSTED.replace("/etc/kelpie/s3cr3t-ca.pem", "ca.pem"));
        refused(&SELF_HOSTED.replace("\"/etc/kelpie/s3cr3t-ca.pem\"", "7"));

        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
        KelpieSettings::check_ca_cert(&with(&ca)).unwrap();

        // No certificate named is nothing to check
        KelpieSettings::check_ca_cert(&example()).unwrap();
        KelpieSettings::check_ca_cert("").unwrap();
    }

    // A mistake elsewhere in the section must not hide the certificate.
    #[test]
    fn a_ca_cert_is_checked_though_the_rest_of_the_section_is_malformed() {
        let text = format!("{SELF_HOSTED}[leases]\ncargo-test = 0\n");
        assert!(KelpieSettings::from_section(&text).is_err());
        let err = KelpieSettings::check_ca_cert(&text)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`webhook.ca_cert`"), "{err}");
    }

    #[test]
    fn trusted_without_a_token_is_refused_naming_the_key() {
        let trusted = SELF_HOSTED.replace("token =", "trusted = true\ntoken =");
        let webhook = KelpieSettings::parse(&trusted).unwrap().webhook.unwrap();
        assert!(webhook.trusted);
        let bare = KelpieSettings::parse(&example()).unwrap().webhook.unwrap();
        assert!(
            !bare.trusted,
            "a topic is trusted only when the maintainer says so"
        );

        let no_token = format!("{}trusted = true\n", example());
        for text in [
            no_token.clone(),
            format!("{no_token}[leases]\ncargo-test = 0\n"),
        ] {
            let err = KelpieSettings::check_trusted(&text)
                .unwrap_err()
                .to_string();
            assert!(err.contains("`webhook.trusted`"), "{err}");
            assert!(err.contains("`token`"), "{err}");
        }
        let err = KelpieSettings::from_section(&no_token)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`webhook.trusted`"), "{err}");

        // A token that is empty, not a string or not one word is no token.
        for bad in ["\"\"", "7", "\"a b\""] {
            let text = format!("{}token = {bad}\ntrusted = true\n", example());
            let err = KelpieSettings::check_trusted(&text)
                .unwrap_err()
                .to_string();
            assert!(err.contains("`webhook.trusted`"), "{bad}: {err}");
            assert!(KelpieSettings::from_section(&text).is_err(), "{bad}");
        }

        KelpieSettings::check_trusted(&trusted).unwrap();
        KelpieSettings::check_trusted(&example()).unwrap();
        KelpieSettings::check_trusted(&format!("{}trusted = false\n", example())).unwrap();
        KelpieSettings::check_trusted("").unwrap();
    }

    #[test]
    fn a_relative_ca_cert_path_is_refused() {
        let err = parse_err(&SELF_HOSTED.replace("/etc/kelpie/s3cr3t-ca.pem", "ca.pem"));
        assert!(err.contains(SHAPE), "{err}");
    }

    #[test]
    fn a_malformed_section_is_named_and_never_quoted() {
        let err = KelpieSettings::from_section(&format!(
            "[webhook]\nkind = \"slack\"\nurl = \"{SECRET}\"\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(
            err.starts_with("the [kelpie] section of dogs.toml: line 2"),
            "{err}"
        );
        assert!(!err.contains("s3cr3t"), "{err}");
    }

    #[test]
    fn a_missing_key_is_refused() {
        let err = parse_err("[webhook]\nkind = \"discord\"\n");
        assert!(err.contains(SHAPE), "{err}");
    }

    #[test]
    fn plain_http_is_only_for_this_machine() {
        let url = |u: &str| WebhookUrl::try_from(u.to_owned()).map(|u| u.expose().to_owned());
        for ok in [
            "https://discord.com/api/webhooks/1/abc",
            "http://127.0.0.1:8080/hook",
            "http://localhost/hook",
            "http://[::1]:9/x",
        ] {
            assert_eq!(url(ok).as_deref(), Ok(ok));
        }
        for bad in [
            "http://ntfy.sh/topic",
            "http://localhost.example.com/x",
            "http://127.0.0.10/x",
            "http://127.0.0.1:80@attacker.example/x",
            "http://localhost@attacker.example/x",
            "http://127.0.0.1:/x",
            "https:///x",
            "http://:8700",
            "http://:8700/x",
            "https://:8700",
            "https://:443/x",
            "ftp://x",
            "https://ntfy.sh/a b",
            "https://ntfy.sh/a\nb",
            "",
        ] {
            assert!(url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn plain_http_is_also_for_a_tailnet() {
        let url = |u: &str| WebhookUrl::try_from(u.to_owned()).map(|u| u.expose().to_owned());
        for ok in [
            "http://100.64.0.1/topic",
            "http://100.127.255.254:2586/topic",
            "http://100.101.102.103/topic",
            "http://ntfy.tail1234.ts.net/topic",
            "http://ntfy.tail1234.ts.net:8080/topic",
            "http://NTFY.Tail1234.TS.NET/topic",
        ] {
            assert_eq!(url(ok).as_deref(), Ok(ok));
        }
        for bad in [
            // A private LAN address is not a tailnet's
            "http://192.168.1.5/topic",
            "http://10.0.0.1/topic",
            // Just either side of the CGNAT block, 100.64.0.0/10
            "http://100.63.255.255/topic",
            "http://100.128.0.0/topic",
            // A tailnet address as userinfo before another host
            "http://100.64.0.1@attacker.example/topic",
            "http://100.64.0.1:80@attacker.example/topic",
            "http://ntfy.tail1234.ts.net@attacker.example/topic",
            // Names that only look like a tailnet's
            "http://ts.net.attacker.example/topic",
            "http://ts.net/topic",
            "http://.ts.net/topic",
            "http://evilts.net/topic",
            "http://ntfy.ts.net.attacker.example/topic",
            "http://ntfy.ts.net./topic",
            "http://ntfy.ts.net:/topic",
            "http://ntfy.ts.net:80a/topic",
            "http://100.64.0.1:/topic",
            "http://100.064.0.1/topic",
        ] {
            assert!(url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cargo_test_holds_three_unless_the_section_says() {
        let absent = KelpieSettings::from_section("").unwrap();
        assert_eq!(absent.leases.cargo_test_capacity().get(), 3);
        let set = KelpieSettings::from_section("[leases]\ncargo-test = 5\n").unwrap();
        assert_eq!(set.leases.cargo_test_capacity().get(), 5);
        for bad in ["0", "-1", "4294967296", "\"3\""] {
            let text = format!("[leases]\ncargo-test = {bad}\n");
            assert!(KelpieSettings::from_section(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn kelpies_codex_login_is_its_own_folder_unless_set() {
        let (home, kelpie) = (Path::new("/Users/me"), Path::new("/Users/me/.kelpie"));
        let codex_home = |text: &str| {
            KelpieSettings::parse(text)
                .unwrap()
                .codex_home(home, kelpie)
        };
        assert_eq!(codex_home("").unwrap(), kelpie.join("codex"));
        assert_eq!(
            codex_home("codex_home = \"~/logins/codex\"").unwrap(),
            home.join("logins/codex")
        );
        assert_eq!(
            codex_home("codex_home = \"/srv/codex\"").unwrap(),
            Path::new("/srv/codex")
        );
        for refused in ["codex", "~", "/Users", "~/.codex", "~/.codex/kelpie", "/"] {
            let err = codex_home(&format!("codex_home = {refused:?}"))
                .unwrap_err()
                .to_string();
            assert!(err.contains("`codex_home`"), "{refused}: {err}");
        }
    }

    #[test]
    fn a_gateway_reads_and_one_the_forwarder_cannot_dial_is_refused() {
        let gateway = "[gateways.paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"PADDOCK_KEY\"\n";
        let read = KelpieSettings::parse(gateway).unwrap();
        assert_eq!(read.gateways().key_vars(), ["PADDOCK_KEY"]);
        let err = parse_err(&gateway.replace("http://", "https://"));
        assert!(
            err.starts_with("`gateways.paddock.url` cannot be used"),
            "{err}"
        );
    }
}
