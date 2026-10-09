//! Kelpie's own settings: the webhook, the counted leases' capacities, the
//! GPU's metrics page, kelpie's own Codex login and its model gateways
//!
//! Kelpie's `[kelpie]` section of `dogs.toml`, or the file under kelpie's
//! home it had before one, shared by every project. Every part is
//! optional: the `webhook` table, whose keys are both required, is the one way
//! a ruling is posted, and with none rulings reach the maintainer only in the
//! log, `status` and `shep kelpie rule`. The URL is a
//! credential, so no error, log line or status carries it.
//! `kelpie-settings.example.toml` beside this crate shows the section.

use std::collections::BTreeMap;
use std::fmt;
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

    // Plain `http` is only for a stand-in on this machine: anything else
    // would send the question, and the URL's secret, in the clear. The whole
    // host and port are checked, since `127.0.0.1@elsewhere` names elsewhere.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let local = |authority: &str| {
            ["127.0.0.1", "localhost", "[::1]"].iter().any(|host| {
                authority.strip_prefix(host).is_some_and(|port| {
                    port.is_empty()
                        || port
                            .strip_prefix(':')
                            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                })
            })
        };
        let allowed = match value.split_once("://") {
            Some(("https", rest)) => !authority(rest).is_empty(),
            Some(("http", rest)) => local(authority(rest)),
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

/// What a malformed file is told, since the parser's own message could quote the URL
const SHAPE: &str = "it takes a `[webhook]` table with `kind` (`discord` or `ntfy`) \
                     and an `https://` `url`, a `[leases]` table with a \
                     `cargo-test` count, a `gpu_metrics_url`, a `codex_home` path, \
                     `[gateways.<name>]` tables with `url` and `key_env`, and nothing else";

impl KelpieSettings {
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
                because: "reviewers are agent files listed in a project's `agents.reviewers`",
                fix: "write each `[local_reviewers.<name>]` table as `agents/<name>.md` in \
                      kelpie's home, with `role: reviewer`, its `kind` as `harness` and \
                      `gpu_lease = true` as `lease: gpu`; a `kind = \"claude\"` or \
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
        include_str!("../kelpie-settings.example.toml")
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
                 listed in a project's `agents.reviewers`: write each"
            ),
            "{err}"
        );
        assert!(err.contains("`role: reviewer`"), "{err}");
        assert!(err.contains("`gpu_lease = true` as `lease: gpu`"), "{err}");
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
            format!("[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\ntoken = \"{SECRET}\"\n"),
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
            "[webhook]\nkind = \"ntfy\"\nurl = \"{SECRET}\"\ntoken = \"x\"\n"
        ));
        assert!(err.starts_with("line 4 is not right"), "{err}");
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
            "ftp://x",
            "https://ntfy.sh/a b",
            "https://ntfy.sh/a\nb",
            "",
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
