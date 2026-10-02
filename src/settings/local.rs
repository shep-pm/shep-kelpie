//! A local model's round: an OpenAI-compatible server, or a command
//!
//! Kelpie's `[local_reviewers]` define these by name, and a project lists
//! them in `review.reviewers`. The older form is one project's
//! `[app.dogs.kelpie.review.local]`, alternating with the Claude round,
//! local first. A table with neither runs the maintainer's qwen-review
//! script, then the Claude round.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use super::NonBlank;
use super::reviewers::LeaseName;

/// The maintainer's qwen-review script, which a table without a local round runs
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

impl LocalRound {
    /// The maintainer's qwen-review script under `home`, which a table
    /// without either form of the local round runs
    pub fn default_at(home: &Path) -> Self {
        let script = QWEN_REVIEW.strip_prefix("~/").unwrap_or(QWEN_REVIEW);
        Self::Command(LocalCommand {
            command: home.join(script),
            lease: None,
            gpu_lease: false,
            ollama: None,
            ollama_model: None,
            paths: Vec::new(),
        })
    }

    /// Whether the project runs a local round at all
    pub fn is_on(&self) -> bool {
        !matches!(self, Self::Off {})
    }

    /// The lease kelpie holds around each round, if any
    ///
    /// `gpu_lease = true` is the older spelling of `lease = "gpu"`.
    pub fn lease(&self) -> Option<LeaseName> {
        let (lease, gpu_lease) = match self {
            Self::Off {} => return None,
            Self::Endpoint(endpoint) => (&endpoint.lease, endpoint.gpu_lease),
            Self::Command(command) => (&command.lease, command.gpu_lease),
        };
        lease.clone().or_else(|| gpu_lease.then(LeaseName::gpu))
    }

    /// The globs a pull request must change a file under for this round to run
    pub fn paths(&self) -> &[NonBlank] {
        match self {
            Self::Off {} => &[],
            Self::Endpoint(endpoint) => &endpoint.paths,
            Self::Command(command) => &command.paths,
        }
    }

    /// Why the settings are refused, when one cannot work as written
    ///
    /// `at` is the setting's dotted name, for the message.
    ///
    /// # Errors
    ///
    /// The message, naming the setting.
    pub fn check(&self, at: &str) -> Result<(), String> {
        let (lease, gpu_lease) = match self {
            Self::Off {} => return Ok(()),
            Self::Endpoint(endpoint) => (&endpoint.lease, endpoint.gpu_lease),
            Self::Command(command) => (&command.lease, command.gpu_lease),
        };
        if lease.is_some() && gpu_lease {
            return Err(format!(
                "`{at}` sets both `lease` and `gpu_lease`: keep `lease`"
            ));
        }
        let Self::Command(command) = self else {
            return Ok(());
        };
        match (&command.ollama, &command.ollama_model) {
            (Some(_), _) if self.lease().is_none() => Err(format!(
                "`{at}.ollama` needs a lease, such as `lease = \"gpu\"`: without \
                 it kelpie reads `/api/ps` while another round may hold the model, \
                 and would rule on that round's model"
            )),
            (None, Some(_)) => Err(format!("`{at}.ollama_model` needs `ollama`")),
            _ => Ok(()),
        }
    }

    /// The Ollama host to read `/api/ps` from before a round, and the model
    /// to look for there
    ///
    /// Only where kelpie holds a lease around the round: a read without it
    /// races whoever holds the model, who may reload it spilled. An
    /// endpoint's host is its URL without the `/v1`, and its model is the one
    /// it asks. A command names its host in `ollama` and may name its model
    /// in `ollama_model`, else every model the host has loaded is looked at.
    pub fn ollama(&self) -> Option<(String, Option<&str>)> {
        self.lease()?;
        match self {
            Self::Off {} => None,
            Self::Endpoint(endpoint) => {
                let url = endpoint.url.as_str();
                let host = url.strip_suffix("/v1").unwrap_or(url);
                Some((host.to_owned(), Some(endpoint.model.as_str())))
            }
            Self::Command(command) => {
                let host = command.ollama.as_ref()?;
                let model = command.ollama_model.as_ref().map(NonBlank::as_str);
                Some((host.as_str().to_owned(), model))
            }
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
    /// The lease kelpie holds around each round: `gpu` is this machine's
    /// GPU lock, and any other name a lock of its own. None when absent.
    #[serde(default)]
    pub lease: Option<LeaseName>,
    /// The older spelling of `lease = "gpu"`. Off when absent.
    #[serde(default)]
    pub gpu_lease: bool,
    /// Globs of the files a pull request must change for this reviewer to
    /// run. Every pull request when absent.
    #[serde(default)]
    pub paths: Vec<NonBlank>,
}

/// A command run for each local round
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalCommand {
    /// Its path. A leading `~/` is the home folder, and a relative path is
    /// taken from the settings file's folder.
    pub command: PathBuf,
    /// The lease kelpie holds around each round: `gpu` is this machine's
    /// GPU lock, and any other name a lock of its own. None when absent, as
    /// for a command that takes the lock itself.
    #[serde(default)]
    pub lease: Option<LeaseName>,
    /// The older spelling of `lease = "gpu"`. Off when absent.
    #[serde(default)]
    pub gpu_lease: bool,
    /// The Ollama host the command's model runs on, such as
    /// `http://localhost:11434`. Kelpie reads its `/api/ps` before each round.
    /// Off when absent, since a command does not say where its model is.
    /// Needs a lease, so kelpie reads it only while it holds one.
    #[serde(default)]
    pub ollama: Option<EndpointUrl>,
    /// The one model on that host the command uses, as Ollama names it.
    /// Without it every model the host has loaded is checked, and one that
    /// is spilled for another reason fails the round too.
    #[serde(default)]
    pub ollama_model: Option<NonBlank>,
    /// Globs of the files a pull request must change for this reviewer to
    /// run. Every pull request when absent.
    #[serde(default)]
    pub paths: Vec<NonBlank>,
}

/// An `http://` or `https://` URL, kept without a trailing `/`
#[derive(Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("pattern" = "^https?://[^\\s/]"))]
pub struct EndpointUrl(String);

impl std::fmt::Debug for EndpointUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EndpointUrl(..)")
    }
}

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
    use crate::webhook::KelpieSettings;
    use std::path::Path;

    use super::*;
    use crate::settings::{LoopReviewer, Runs, Settings};

    const EXAMPLE: &str = include_str!("../../settings.example.toml");

    #[test]
    fn an_endpoint_url_does_not_print_itself() {
        let url = EndpointUrl::try_from("http://gpu-box:9835/metrics".to_owned()).unwrap();
        assert_eq!(format!("{url:?}"), "EndpointUrl(..)");
    }

    const TABLE: &str = "[app.dogs.kelpie.review.local]\n\
                         kind = \"command\"\n\
                         command = \"~/.claude/scripts/qwen-review.sh\"\n";

    // The example's runner entry with `table` as its older local round.
    fn with_table(table: &str) -> Result<Settings, String> {
        let entry = crate::test::project_table(&crate::test::with_tables(EXAMPLE, table));
        let (home, folder) = (Path::new("/home/me"), Path::new("/p"));
        Settings::from_table(&entry, "shep", home, folder).map_err(|e| e.to_string())
    }

    fn local(table: &str) -> LocalRound {
        with_table(table)
            .unwrap()
            .review
            .local
            .expect("the table sets it")
    }

    // The loop `table` runs, with nothing defined in kelpie's settings.
    fn lineup(table: &str) -> Vec<LoopReviewer> {
        let settings = with_table(table).unwrap();
        settings
            .lineup(&KelpieSettings::default(), Path::new("/home/me"))
            .unwrap()
    }

    fn command(path: &str) -> LocalRound {
        LocalRound::Command(LocalCommand {
            command: PathBuf::from(path),
            lease: None,
            gpu_lease: false,
            ollama: None,
            ollama_model: None,
            paths: Vec::new(),
        })
    }

    #[test]
    fn the_ollama_host_is_an_endpoints_url_or_a_commands_own_setting() {
        let table = "[app.dogs.kelpie.review.local]\nkind = \"endpoint\"\n\
                     url = \"http://gpu-box:11434/v1/\"\nmodel = \"coder\"\ncontext = 8192\n";
        assert_eq!(
            local(table).ollama(),
            None,
            "not read where kelpie holds no lease"
        );
        let leased = format!("{table}gpu_lease = true\n");
        assert_eq!(
            local(&leased).ollama(),
            Some(("http://gpu-box:11434".to_owned(), Some("coder")))
        );
        let named = format!("{table}lease = \"gpu-box\"\n");
        assert_eq!(
            local(&named).ollama(),
            Some(("http://gpu-box:11434".to_owned(), Some("coder")))
        );
        let table = "[app.dogs.kelpie.review.local]\nkind = \"command\"\n\
                     command = \"/opt/review\"\ngpu_lease = true\n";
        assert_eq!(local(table).ollama(), None);
        let table = format!("{table}ollama = \"http://gpu-box:11434/\"\n");
        assert_eq!(
            local(&table).ollama(),
            Some(("http://gpu-box:11434".to_owned(), None))
        );
        let table = format!("{table}ollama_model = \"coder:14b\"\n");
        assert_eq!(
            local(&table).ollama(),
            Some(("http://gpu-box:11434".to_owned(), Some("coder:14b")))
        );
        assert_eq!(LocalRound::Off {}.ollama(), None);
    }

    #[test]
    fn an_ollama_host_without_a_lease_or_a_model_without_a_host_is_refused() {
        let table = "[app.dogs.kelpie.review.local]\nkind = \"command\"\n\
                     command = \"/opt/review\"\n";
        let err = with_table(&format!("{table}ollama = \"http://h:1\"\n")).unwrap_err();
        assert!(err.contains("`review.local.ollama` needs a lease"), "{err}");
        let err = with_table(&format!("{table}ollama_model = \"m\"\ngpu_lease = true\n"));
        assert!(
            err.unwrap_err()
                .contains("`review.local.ollama_model` needs `ollama`")
        );
        let err = with_table(&format!("{table}lease = \"gpu\"\ngpu_lease = true\n"));
        assert!(
            err.unwrap_err()
                .contains("sets both `lease` and `gpu_lease`")
        );
    }

    #[test]
    fn a_table_with_neither_form_runs_the_qwen_review_script_from_the_home_folder_then_deep() {
        let [qwen, deep] = lineup("").try_into().unwrap();
        assert_eq!(qwen.name.as_str(), "qwen");
        assert_eq!(
            qwen.runs,
            Runs::Local(command("/home/me/.claude/scripts/qwen-review.sh"))
        );
        assert_eq!(deep.name.as_str(), "deep");
        assert_eq!(deep.runs, Runs::Deep);
    }

    #[test]
    fn a_table_with_the_older_form_keeps_the_older_loop_of_that_round_and_claude() {
        let [qwen, claude] = lineup(TABLE).try_into().unwrap();
        assert_eq!(qwen.name.as_str(), "qwen");
        assert_eq!(
            qwen.runs,
            Runs::Local(command("/home/me/.claude/scripts/qwen-review.sh"))
        );
        assert_eq!(claude.name.as_str(), "claude");
    }

    #[test]
    fn the_local_round_can_be_off() {
        let table = "[app.dogs.kelpie.review.local]\nkind = \"off\"\n";
        assert_eq!(local(table), LocalRound::Off {});
        assert!(!local(table).is_on());
        let [claude] = lineup(table).try_into().unwrap();
        assert_eq!(claude.name.as_str(), "claude");
    }

    #[test]
    fn an_endpoint_names_its_server_model_and_context() {
        let table = "[app.dogs.kelpie.review.local]\nkind = \"endpoint\"\n\
                     url = \"http://localhost:11434/v1/\"\n\
                     model = \"qwen2.5-coder:14b\"\ncontext = 32768\ngpu_lease = true\n";
        let LocalRound::Endpoint(e) = local(table) else {
            panic!("not an endpoint");
        };
        assert_eq!(e.url.as_str(), "http://localhost:11434/v1");
        assert_eq!(e.model.as_str(), "qwen2.5-coder:14b");
        assert_eq!(e.context.get(), 32768);
        assert!(e.gpu_lease);
    }

    #[test]
    fn the_lease_is_none_unless_asked_for_and_gpu_lease_is_the_gpu() {
        let table =
            "[app.dogs.kelpie.review.local]\nkind = \"command\"\ncommand = \"/opt/review\"\n";
        assert_eq!(local(table).lease(), None);
        let gpu = format!("{table}gpu_lease = true\n");
        assert_eq!(local(&gpu).lease(), Some(LeaseName::gpu()));
        let named = format!("{table}lease = \"gpu-box\"\n");
        assert_eq!(local(&named).lease().unwrap().as_str(), "gpu-box");
        assert_eq!(LocalRound::Off {}.lease(), None);
        let err = with_table(&format!("{table}lease = \"GPU box\"\n")).unwrap_err();
        assert!(err.contains("must be lowercase letters"), "{err}");
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
        let entry = crate::test::with_tables(EXAMPLE, table);
        let old_file = crate::test::project_table(&entry);
        std::fs::write(&file, toml::to_string(&old_file).unwrap()).unwrap();
        let s = Settings::load(&file, Path::new("/home/me")).unwrap();
        assert_eq!(
            s.review.local,
            Some(command(&dir.path().join("review.sh").display().to_string()))
        );
    }
}
