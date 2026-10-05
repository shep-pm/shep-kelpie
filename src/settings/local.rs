//! A reviewer that runs on its own: a command, or kelpie's reviewer over an
//! OpenAI-compatible server
//!
//! A reviewer agent file on the `command` or `endpoint` harness defines one.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::NonBlank;
use super::reviewers::LeaseName;

// The smallest context the endpoint reviewer accepts, in tokens. At 4096 a
// reply keeps 1,024 and the prompt about 500, leaving about 2,600 for a diff.
const MIN_CONTEXT: u32 = 4096;

/// What runs a local round
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRound {
    /// Kelpie's own reviewer, over an OpenAI-compatible server
    Endpoint(Endpoint),
    /// A command that keeps the README's contract
    Command(LocalCommand),
}

impl LocalRound {
    /// The lease kelpie holds around each round, if any
    pub fn lease(&self) -> Option<LeaseName> {
        match self {
            Self::Endpoint(endpoint) => endpoint.lease.clone(),
            Self::Command(command) => command.lease.clone(),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The server's base URL, up to and including its `/v1`
    pub url: EndpointUrl,
    /// The model's name as the server knows it
    pub model: NonBlank,
    /// The context size the server gives the model, in tokens
    pub context: ContextSize,
    /// The lease kelpie holds around each round: `gpu` is this machine's
    /// GPU lock, and any other name a lock of its own. None when absent.
    pub lease: Option<LeaseName>,
}

/// A command run for each local round
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCommand {
    /// Its path. A leading `~/` is the home folder.
    pub command: PathBuf,
    /// The lease kelpie holds around each round: `gpu` is this machine's
    /// GPU lock, and any other name a lock of its own. None when absent, as
    /// for a command that takes the lock itself.
    pub lease: Option<LeaseName>,
    /// The Ollama host the command's model runs on, such as
    /// `http://localhost:11434`. Kelpie reads its `/api/ps` before each round.
    /// Off when absent, since a command does not say where its model is.
    /// Needs a lease, so kelpie reads it only while it holds one.
    pub ollama: Option<EndpointUrl>,
    /// The one model on that host the command uses, as Ollama names it.
    /// Without it every model the host has loaded is checked, and one that
    /// is spilled for another reason fails the round too.
    pub ollama_model: Option<NonBlank>,
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
    use super::*;

    fn url(text: &str) -> EndpointUrl {
        EndpointUrl::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn an_endpoint_url_does_not_print_itself() {
        assert_eq!(
            format!("{:?}", url("http://gpu-box:9835/metrics")),
            "EndpointUrl(..)"
        );
    }

    #[test]
    fn the_ollama_host_is_an_endpoints_url_or_a_commands_own_setting_and_only_under_a_lease() {
        let mut endpoint = Endpoint {
            url: url("http://gpu-box:11434/v1/"),
            model: "coder".to_owned().try_into().unwrap(),
            context: ContextSize::try_from(8192).unwrap(),
            lease: None,
        };
        assert_eq!(LocalRound::Endpoint(endpoint.clone()).ollama(), None);
        endpoint.lease = Some(LeaseName::gpu());
        assert_eq!(
            LocalRound::Endpoint(endpoint).ollama(),
            Some(("http://gpu-box:11434".to_owned(), Some("coder")))
        );
        let mut command = LocalCommand {
            command: PathBuf::from("/opt/review"),
            lease: Some(LeaseName::gpu()),
            ollama: None,
            ollama_model: None,
        };
        assert_eq!(LocalRound::Command(command.clone()).ollama(), None);
        command.ollama = Some(url("http://gpu-box:11434/"));
        assert_eq!(
            LocalRound::Command(command.clone()).ollama(),
            Some(("http://gpu-box:11434".to_owned(), None))
        );
        command.ollama_model = Some("coder:14b".to_owned().try_into().unwrap());
        assert_eq!(
            LocalRound::Command(command).ollama(),
            Some(("http://gpu-box:11434".to_owned(), Some("coder:14b")))
        );
    }

    #[test]
    fn a_malformed_url_or_context_is_refused() {
        for text in ["localhost:11434", "ftp://x", "http://", "http://a b"] {
            let err = EndpointUrl::try_from(text.to_owned()).unwrap_err();
            assert_eq!(err, "must be an `http://` or `https://` URL", "{text}");
        }
        for context in [0, 4095, -1] {
            let err = ContextSize::try_from(context).unwrap_err();
            assert_eq!(err, "must be at least 4096 tokens", "{context}");
        }
        assert!(
            ContextSize::try_from(5_000_000_000)
                .unwrap_err()
                .contains("too large")
        );
    }
}
