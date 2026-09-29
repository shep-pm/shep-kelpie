//! The review loop's local round, `[app.dogs.kelpie.review.local]`
//!
//! The local round alternates with the Claude round, local first. A project
//! turns it off, points kelpie's own reviewer at an OpenAI-compatible
//! server, or names a command that keeps the README's contract. A file
//! without the table runs the maintainer's qwen-review script.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::NonBlank;

/// The maintainer's qwen-review script, which a file without `[app.dogs.kelpie.review.local]` runs
const QWEN_REVIEW: &str = "~/.claude/scripts/qwen-review.sh";

// The smallest context the endpoint reviewer accepts, in tokens. At 4096 a
// reply keeps 1,024 and the prompt about 500, leaving about 2,600 for a diff.
const MIN_CONTEXT: u32 = 4096;

/// Which local round a project runs
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LocalRound {
    /// No local round: every round is the Claude round
    Off {},
    /// Kelpie's own reviewer, over an OpenAI-compatible server
    Endpoint(Endpoint),
    /// A command that keeps the README's contract
    Command(LocalCommand),
}

impl Default for LocalRound {
    fn default() -> Self {
        Self::Command(LocalCommand {
            command: PathBuf::from(QWEN_REVIEW),
            gpu_lease: false,
        })
    }
}

impl LocalRound {
    /// Whether the project runs a local round at all
    pub fn is_on(&self) -> bool {
        !matches!(self, Self::Off {})
    }

    /// Whether kelpie holds the GPU lock around each round
    pub fn gpu_lease(&self) -> bool {
        match self {
            Self::Off {} => false,
            Self::Endpoint(endpoint) => endpoint.gpu_lease,
            Self::Command(command) => command.gpu_lease,
        }
    }
}

/// An OpenAI-compatible server and the model kelpie's reviewer asks
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    /// The server's base URL, up to and including its `/v1`
    pub url: EndpointUrl,
    /// The model's name as the server knows it
    pub model: NonBlank,
    /// The context size the server gives the model, in tokens
    pub context: ContextSize,
    /// Whether kelpie holds the GPU lock around each round. Off when absent.
    #[serde(default)]
    pub gpu_lease: bool,
}

/// A command run for each local round
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalCommand {
    /// Its path. A leading `~/` is the home folder, and a relative path is
    /// taken from the settings file's folder.
    pub command: PathBuf,
    /// Whether kelpie holds the GPU lock around each round. Off when absent,
    /// and off for a command that takes the lock itself.
    #[serde(default)]
    pub gpu_lease: bool,
}

/// An `http://` or `https://` URL, kept without a trailing `/`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("pattern" = "^https?://[^\\s/]"))]
pub struct EndpointUrl(String);

impl EndpointUrl {
    /// The URL, without a trailing `/`
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EndpointUrl {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let url = value.trim_end_matches('/');
        let host = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"));
        match host {
            Some(host) if !host.is_empty() && !host.contains(char::is_whitespace) => {
                Ok(Self(url.to_owned()))
            }
            _ => Err("must be an `http://` or `https://` URL"),
        }
    }
}

/// A model's context size in tokens, at least 4096
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "i64")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("minimum" = MIN_CONTEXT, "maximum" = u32::MAX))]
pub struct ContextSize(u32);

impl ContextSize {
    /// The size in tokens
    #[inline]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<i64> for ContextSize {
    type Error = &'static str;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match u32::try_from(value) {
            Ok(tokens) if tokens >= MIN_CONTEXT => Ok(Self(tokens)),
            Err(_) if value > 0 => Err("is too large: give the model's context in tokens"),
            _ => Err("must be at least 4096 tokens"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::settings::Settings;

    const EXAMPLE: &str = include_str!("../../settings.example.toml");

    const TABLE: &str = "[app.dogs.kelpie.review.local]\n\
                         kind = \"command\"\n\
                         command = \"~/.claude/scripts/qwen-review.sh\"\n";

    // The example's runner entry with `table` for its local round.
    fn with_table(table: &str) -> Result<Settings, String> {
        assert!(EXAMPLE.contains(TABLE), "the example's local round moved");
        let entry = crate::test::project_table(&EXAMPLE.replace(TABLE, table));
        let (home, folder) = (Path::new("/home/maintainer"), Path::new("/p"));
        Settings::from_table(&entry, "shep", home, folder).map_err(|e| e.to_string())
    }

    fn command(path: &str) -> LocalRound {
        LocalRound::Command(LocalCommand {
            command: PathBuf::from(path),
            gpu_lease: false,
        })
    }

    #[test]
    fn the_example_runs_the_qwen_review_script_from_the_home_folder() {
        assert_eq!(
            with_table(TABLE).unwrap().review.local,
            command("/home/maintainer/.claude/scripts/qwen-review.sh")
        );
    }

    #[test]
    fn a_file_without_the_table_runs_the_same_script() {
        assert_eq!(
            with_table("").unwrap().review,
            with_table(TABLE).unwrap().review
        );
    }

    #[test]
    fn the_local_round_can_be_off() {
        let s = with_table("[app.dogs.kelpie.review.local]\nkind = \"off\"\n").unwrap();
        assert_eq!(s.review.local, LocalRound::Off {});
        assert!(!s.review.local.is_on());
    }

    #[test]
    fn an_endpoint_names_its_server_model_and_context() {
        let table = "[app.dogs.kelpie.review.local]\nkind = \"endpoint\"\n\
                     url = \"http://localhost:11434/v1/\"\n\
                     model = \"qwen2.5-coder:14b\"\ncontext = 32768\ngpu_lease = true\n";
        let LocalRound::Endpoint(e) = with_table(table).unwrap().review.local else {
            panic!("not an endpoint");
        };
        assert_eq!(e.url.as_str(), "http://localhost:11434/v1");
        assert_eq!(e.model.as_str(), "qwen2.5-coder:14b");
        assert_eq!(e.context.get(), 32768);
        assert!(e.gpu_lease);
    }

    #[test]
    fn the_gpu_lease_is_off_unless_asked_for() {
        let table =
            "[app.dogs.kelpie.review.local]\nkind = \"command\"\ncommand = \"/opt/review\"\n";
        assert!(!with_table(table).unwrap().review.local.gpu_lease());
        let table = format!("{table}gpu_lease = true\n");
        assert!(with_table(&table).unwrap().review.local.gpu_lease());
        assert!(!LocalRound::Off {}.gpu_lease());
    }

    #[test]
    fn a_malformed_endpoint_is_named() {
        let endpoint = |url: &str, context: &str| {
            format!(
                "[app.dogs.kelpie.review.local]\nkind = \"endpoint\"\nurl = \"{url}\"\n\
                 model = \"m\"\ncontext = {context}\n"
            )
        };
        for url in ["localhost:11434", "ftp://x", "http://", "http://a b"] {
            let err = with_table(&endpoint(url, "8192")).unwrap_err();
            assert!(
                err.contains("must be an `http://` or `https://` URL"),
                "{url}: {err}"
            );
        }
        for context in ["0", "4095", "-1"] {
            let err = with_table(&endpoint("http://x", context)).unwrap_err();
            assert!(
                err.contains("must be at least 4096 tokens"),
                "{context}: {err}"
            );
        }
        let err = with_table(&endpoint("http://x", "5000000000")).unwrap_err();
        assert!(err.contains("is too large"), "{err}");
        let err =
            with_table("[app.dogs.kelpie.review.local]\nkind = \"endpoint\"\nurl = \"http://x\"\n");
        assert!(err.unwrap_err().contains("missing field `model`"));
    }

    #[test]
    fn an_unknown_kind_or_key_is_named() {
        let err = with_table("[app.dogs.kelpie.review.local]\nkind = \"qwen\"\n").unwrap_err();
        assert!(err.contains("unknown variant `qwen`"), "{err}");
        let err = with_table("[app.dogs.kelpie.review.local]\nkind = \"off\"\ncommand = \"x\"\n")
            .unwrap_err();
        assert!(err.contains("unknown field `command`"), "{err}");
    }

    #[test]
    fn a_relative_command_is_taken_from_the_settings_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        let table = "[app.dogs.kelpie.review.local]\nkind = \"command\"\ncommand = \"review.sh\"\n";
        let old_file = crate::test::project_table(&EXAMPLE.replace(TABLE, table));
        std::fs::write(&file, toml::to_string(&old_file).unwrap()).unwrap();
        let s = Settings::load(&file, Path::new("/home/maintainer")).unwrap();
        assert_eq!(
            s.review.local,
            command(&dir.path().join("review.sh").display().to_string())
        );
    }
}
