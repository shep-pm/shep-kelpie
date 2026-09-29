//! Kelpie's own settings: the webhook every ruling also goes to
//!
//! Kelpie's `[kelpie]` section of `dogs.toml`, or the file under kelpie's
//! home it had before one, shared by every project. It holds one `webhook`
//! table, and both its keys are required. The URL is a credential, so no
//! error, log line or status carries it. `kelpie-settings.example.toml`
//! beside this crate shows the section.

use std::fmt;
use std::path::Path;

use schemars::JsonSchema;
use serde::Deserialize;
use shep_client::dogs::dog_config;

use crate::settings::SettingsError;

/// What every project shares
#[dog_config]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KelpieSettings {
    /// Where every ruling is posted, so the maintainer hears of it away
    /// from the terminal
    pub webhook: Webhook,
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
const SHAPE: &str = "it needs a `[webhook]` table with `kind` (`discord` or `ntfy`) \
                     and an `https://` `url`, and nothing else";

impl KelpieSettings {
    /// Reads and checks kelpie's settings file
    ///
    /// # Errors
    ///
    /// - [`SettingsError::Read`] when the file cannot be read.
    /// - [`SettingsError::Parse`] naming the line that is wrong, never its text.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = std::fs::read_to_string(path).map_err(|e| SettingsError::Read {
            path: path.to_owned(),
            kind: e.kind(),
        })?;
        Self::parse(&text).map_err(|message| SettingsError::Parse {
            path: path.to_owned(),
            message,
        })
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
        toml::from_str(text).map_err(|e: toml::de::Error| {
            let line = e
                .span()
                .map(|span| text[..span.start].matches('\n').count() + 1);
            match line {
                Some(line) => format!("line {line} is not right: {SHAPE}"),
                None => SHAPE.to_owned(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The example's section as shep hands it over: re-rooted, so `[webhook]`.
    fn example() -> String {
        include_str!("../kelpie-settings.example.toml").replace("[kelpie.webhook]", "[webhook]")
    }

    // A URL no test would print by chance, so finding it anywhere is a leak
    const SECRET: &str = "https://ntfy.example.invalid/kelpie-s3cr3t-topic";

    fn parse_err(text: &str) -> String {
        KelpieSettings::parse(text).expect_err("the settings should be refused")
    }

    #[test]
    fn the_example_reads() {
        let s = KelpieSettings::parse(&example()).unwrap();
        assert_eq!(s.webhook.kind, WebhookKind::Ntfy);
        assert!(s.webhook.url.expose().starts_with("https://ntfy.sh/"));
    }

    // A derived Debug would print the URL wherever settings are debugged.
    #[test]
    fn debug_does_not_leak_the_url() {
        assert!(example().contains("https://ntfy.sh/your-private-topic"));
        let text = example().replace("https://ntfy.sh/your-private-topic", SECRET);
        let s = KelpieSettings::parse(&text).unwrap();
        assert_eq!(format!("{:?}", s.webhook.url), "WebhookUrl(..)");
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
    fn a_missing_file_names_its_path() {
        let err = KelpieSettings::load(Path::new("/nonexistent/settings.toml")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot read settings file /nonexistent/settings.toml: entity not found"
        );
    }
}
