//! Gateways: one endpoint in front of a host's model servers, such as paddock
//!
//! Kelpie's own settings name each one, `[gateways.<name>]`, with its `url`
//! and the variable in kelpie's environment that holds its key. An agent
//! file names a gateway in place of a server's `url`. The gateway queues
//! its calls, so kelpie takes no lease around them.
//! The key is read when a call needs it and sent only from kelpie's own
//! process: every sandboxed call has that variable unset.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use super::local::EndpointUrl;
use super::reviewers::lowercase_name;
use super::{AgentName, SettingsError, TriageSpam};
use crate::agents::Agents;
use crate::forwarder::Upstream;

/// A gateway's name, as `[gateways.<name>]` and an agent's `gateway` give it
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct GatewayName(String);

impl GatewayName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for GatewayName {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match lowercase_name(&value) {
            true => Ok(Self(value)),
            false => Err("must be lowercase letters, digits and `-`"),
        }
    }
}

impl fmt::Display for GatewayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a gateway is, which says how kelpie talks to it beyond chat
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayKind {
    /// Paddock, a shep dog, which checks keys and leases models by its own routes
    #[default]
    Paddock,
    /// Any OpenAI-compatible gateway, such as LiteLLM, OpenRouter or a bare
    /// llama.cpp server: it knows nothing of paddock's routes, so it offers no
    /// lease and a model it runs a turn on may be evicted
    Openai,
}

/// One gateway in kelpie's own settings
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gateway {
    /// What it is, `paddock` when absent
    #[serde(default)]
    pub kind: GatewayKind,
    /// Its address, such as `http://gpu-box:8700`, over plain `http://`.
    /// Its OpenAI routes are under `/v1`.
    pub url: EndpointUrl,
    /// The variable in kelpie's environment that holds the key it gave
    /// kelpie, such as `PADDOCK_KEY`. No agent call sees that variable.
    pub key_env: KeyVar,
}

impl Gateway {
    /// Its OpenAI base, `<url>/v1`
    pub fn base(&self) -> EndpointUrl {
        EndpointUrl::try_from(format!("{}/v1", self.url.as_str()))
            .expect("a URL with a path added is still one")
    }
}

/// The name of an environment variable: capitals, digits and `_`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct KeyVar(String);

impl KeyVar {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Variables the children kelpie starts need, which hiding a key would strip from them
const RESERVED: [&str; 4] = ["HOME", "PATH", "GH_TOKEN", "GITHUB_TOKEN"];

impl TryFrom<String> for KeyVar {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_';
        if !(value.starts_with(|c: char| !c.is_ascii_digit()) && value.chars().all(allowed)) {
            return Err("must be an environment variable's name, such as PADDOCK_KEY".to_owned());
        }
        match RESERVED.contains(&value.as_str()) {
            true => Err(format!(
                "{value} is a variable kelpie's children need, which a key's variable is \
                 hidden from: use a name of its own, such as PADDOCK_KEY"
            )),
            false => Ok(Self(value)),
        }
    }
}

/// A gateway's key
///
/// `Debug` does not leak the key, and nothing but a request to its gateway carries it.
#[derive(Clone, PartialEq, Eq)]
pub struct GatewayKey(String);

impl GatewayKey {
    /// A key a test makes up
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(key: &str) -> Self {
        Self(key.to_owned())
    }

    /// The key, for the request to its gateway and nothing else
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GatewayKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GatewayKey(..)")
    }
}

/// Where a local model's server is: its own URL, or a gateway in front of it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelHost {
    /// An OpenAI-compatible server's base URL, up to and including its `/v1`
    Url(EndpointUrl),
    /// A gateway in kelpie's own settings
    Gateway(GatewayName),
}

/// A model host made concrete: the base its requests go to, and the key a gateway takes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The base URL, up to and including its `/v1`
    pub base: EndpointUrl,
    /// The gateway's key, for a model behind one
    pub key: Option<GatewayKey>,
}

/// How long each step of talking to paddock may take
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long a request gets to connect, and one other than a take to answer
    pub answer: Duration,
    /// How long the release gets, since a turn's end waits on it
    pub release: Duration,
    /// How often a held lease is renewed: a quarter of its ttl
    pub renew: Duration,
    /// How long a take paddock turned away as busy waits to ask again
    pub retry: Duration,
    /// How long after its last renewal paddock took the lease counts as lost, if none since was taken: its ttl
    pub lapse: Duration,
}

impl Timing {
    /// What a runner uses
    pub const RUNNER: Self = Self {
        answer: Duration::from_secs(10),
        release: Duration::from_secs(3),
        renew: Duration::from_secs(30),
        retry: Duration::from_secs(30),
        lapse: Duration::from_secs(120),
    };
}

/// Kelpie's gateways, as a runner read them when it started
#[derive(Clone)]
pub struct Gateways {
    table: BTreeMap<GatewayName, Gateway>,
    env: fn(&str) -> Option<String>,
    paddock: Timing,
}

impl fmt::Debug for Gateways {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateways")
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

impl Default for Gateways {
    fn default() -> Self {
        Self::new(BTreeMap::new())
    }
}

impl Gateways {
    /// `table`, with each key read from kelpie's environment
    pub fn new(table: BTreeMap<GatewayName, Gateway>) -> Self {
        Self {
            table,
            env: |name| std::env::var(name).ok().filter(|key| !key.is_empty()),
            paddock: Timing::RUNNER,
        }
    }

    /// `table`, with each key read through `env`, as a test sets them
    #[cfg(any(test, feature = "test-support"))]
    pub fn reading(table: BTreeMap<GatewayName, Gateway>, env: fn(&str) -> Option<String>) -> Self {
        Self {
            table,
            env,
            paddock: Timing::RUNNER,
        }
    }

    /// These gateways, with a paddock's leases on `paddock`'s schedule
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_paddock_timing(mut self, paddock: Timing) -> Self {
        self.paddock = paddock;
        self
    }

    /// The schedule a paddock's leases keep
    pub fn paddock_timing(&self) -> Timing {
        self.paddock
    }

    /// The gateway `name`
    ///
    /// # Errors
    ///
    /// The reason when kelpie's settings lack a gateway of that name.
    pub fn named(&self, name: &GatewayName) -> Result<&Gateway, String> {
        self.table.get(name).ok_or_else(|| {
            format!(
                "gateway {name} is not in kelpie's settings: add `[kelpie.gateways.{name}]` \
                 with its `url` and `key_env`"
            )
        })
    }

    /// Each gateway, by name
    pub fn iter(&self) -> impl Iterator<Item = (&GatewayName, &Gateway)> {
        self.table.iter()
    }

    /// The gateway `name`, if kelpie's settings hold one
    pub fn get(&self, name: &GatewayName) -> Option<&Gateway> {
        self.table.get(name)
    }

    /// The variables holding the keys, which no agent call may see
    pub fn key_vars(&self) -> Vec<String> {
        let names = self.table.values().map(|g| g.key_env.as_str().to_owned());
        names.collect()
    }

    /// The key of `gateway`, from kelpie's environment
    ///
    /// # Errors
    ///
    /// The reason, naming the variable, when it is unset or empty, or holds
    /// a line break, which would start a header of its own in a request.
    pub fn key(&self, gateway: &Gateway) -> Result<GatewayKey, String> {
        let var = gateway.key_env.as_str();
        let key = (self.env)(var).ok_or_else(|| {
            format!("{var}, which holds the gateway's key, is not set in kelpie's environment")
        })?;
        match key.contains(['\r', '\n']) {
            true => Err(format!(
                "{var}, which holds the gateway's key, holds a line break"
            )),
            false => Ok(GatewayKey(key)),
        }
    }

    /// Where `host`'s requests go, and with what key
    ///
    /// # Errors
    ///
    /// The reason when `host` names a gateway kelpie's settings lack, or
    /// one whose key is not set.
    pub fn route(&self, host: &ModelHost) -> Result<Route, String> {
        let name = match host {
            ModelHost::Url(url) => {
                return Ok(Route {
                    base: url.clone(),
                    key: None,
                });
            }
            ModelHost::Gateway(name) => name,
        };
        let gateway = self.named(name)?;
        Ok(Route {
            base: gateway.base(),
            key: Some(self.key(gateway)?),
        })
    }

    /// Refuses a start or a change whose `listed` agents name a gateway that
    /// cannot be routed to or dialed, as an endpoint that does not answer is refused
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming the agent and the reason, which for an
    /// `https://` gateway is that the forwarder speaks plain `http://`.
    pub fn check_listed<'a>(
        &self,
        book: &Agents,
        listed: impl IntoIterator<Item = &'a AgentName>,
    ) -> Result<(), SettingsError> {
        self.check_each(book, listed, |gateway| self.key(gateway).map(|_| ()))
    }

    /// As [`Gateways::check_listed`], for a shell that holds no gateway key, such as the one
    /// that runs `shep kelpie set`. The key is left to the runner that holds it, and to
    /// `doctor`, which says what this shell cannot know
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming the agent and the reason, which here is
    /// only that kelpie's settings name no such gateway or that its URL is not
    /// one the forwarder can dial.
    pub fn check_listed_without_keys<'a>(
        &self,
        book: &Agents,
        listed: impl IntoIterator<Item = &'a AgentName>,
    ) -> Result<(), SettingsError> {
        self.check_each(book, listed, |_| Ok(()))
    }

    // Refuses the first `listed` agent whose gateway cannot be dialed, or whose
    // key `needs` cannot find.
    fn check_each<'a>(
        &self,
        book: &Agents,
        listed: impl IntoIterator<Item = &'a AgentName>,
        needs: impl Fn(&Gateway) -> Result<(), String>,
    ) -> Result<(), SettingsError> {
        for name in listed {
            let Some((gateway, _)) = book.get(name).and_then(|a| a.runs.gateway()) else {
                continue;
            };
            let reason = self.dialable(gateway).and_then(&needs).err();
            if let Some(reason) = reason {
                return Err(SettingsError::Invalid {
                    setting: "agents",
                    reason: format!("{name}: {reason}"),
                });
            }
        }
        Ok(())
    }

    // The gateway `name` in kelpie's settings, when its URL is one the forwarder can dial.
    fn dialable(&self, name: &GatewayName) -> Result<&Gateway, String> {
        let gateway = self.named(name)?;
        Upstream::new(&gateway.base()).map_err(|e| e.to_string())?;
        Ok(gateway)
    }

    /// What a forwarder passes `host`'s chat calls to, with a gateway's key
    ///
    /// # Errors
    ///
    /// The reason, as for [`Gateways::route`] or when the forwarder cannot dial it.
    pub fn upstream(&self, host: &ModelHost) -> Result<Upstream, String> {
        let route = self.route(host)?;
        let upstream = Upstream::new(&route.base).map_err(|e| e.to_string())?;
        Ok(match route.key {
            Some(key) => upstream.with_key(key),
            None => upstream,
        })
    }

    /// Refuses a start or change whose triage spam check names a gateway kelpie's settings lack or cannot dial
    ///
    /// The key is left to the check itself: a gateway whose key is unset is one
    /// kelpie cannot ask, and triage goes on without the check rather than the
    /// runner refusing to start.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `triage.spam.gateway` and the reason.
    pub fn check_triage_spam(&self, spam: &TriageSpam) -> Result<(), SettingsError> {
        let Some(name) = &spam.gateway else {
            return Ok(());
        };
        match self.dialable(name).err() {
            None => Ok(()),
            Some(reason) => Err(SettingsError::Invalid {
                setting: "triage.spam.gateway",
                reason: format!("{name}: {reason}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Probability;
    use crate::settings::ClassifierPath;

    fn paddock(url: &str) -> BTreeMap<GatewayName, Gateway> {
        let gateway = Gateway {
            kind: GatewayKind::Paddock,
            url: EndpointUrl::try_from(url.to_owned()).unwrap(),
            key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
        };
        BTreeMap::from([(
            GatewayName::try_from("paddock".to_owned()).unwrap(),
            gateway,
        )])
    }

    // A lazy `derive(Debug)` would print the key into a log or a ruling.
    #[test]
    fn neither_a_key_nor_the_gateways_debug_show_it() {
        let gateways = Gateways::reading(paddock("http://gpu:8700"), |_| Some("pk-1".into()));
        let host = ModelHost::Gateway(GatewayName::try_from("paddock".to_owned()).unwrap());
        let route = gateways.route(&host).unwrap();
        assert_eq!(route.base.as_str(), "http://gpu:8700/v1");
        assert_eq!(format!("{:?}", route.key), "Some(GatewayKey(..))");
        assert!(!format!("{gateways:?}{route:?}").contains("pk-1"));
    }

    #[test]
    fn a_missing_gateway_or_key_is_named() {
        let gateways = Gateways::reading(paddock("http://gpu:8700"), |_| None);
        let named =
            |name: &str| ModelHost::Gateway(GatewayName::try_from(name.to_owned()).unwrap());
        let err = gateways.route(&named("other")).unwrap_err();
        assert!(err.contains("`[kelpie.gateways.other]`"), "{err}");
        let err = gateways.route(&named("paddock")).unwrap_err();
        assert!(err.starts_with("PADDOCK_KEY, which holds"), "{err}");
        for bad in ["paddock_key", "1KEY", "KEY-1", ""] {
            assert!(KeyVar::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_key_with_a_line_break_is_refused_where_it_is_read_without_showing_it() {
        let host = ModelHost::Gateway(GatewayName::try_from("paddock".to_owned()).unwrap());
        let reads: [fn(&str) -> Option<String>; 3] = [
            |_| Some("pk-1\r\nx-evil: 1".into()),
            |_| Some("pk-1\nx-evil: 1".into()),
            |_| Some("pk-1\rx-evil: 1".into()),
        ];
        for env in reads {
            let gateways = Gateways::reading(paddock("http://gpu:8700"), env);
            let err = gateways.route(&host).unwrap_err();
            assert!(err.starts_with("PADDOCK_KEY, which holds"), "{err}");
            assert!(err.contains("line break") && !err.contains("pk-1"), "{err}");
        }
    }

    #[test]
    fn a_name_the_children_need_is_refused_as_a_key_variable() {
        for reserved in ["HOME", "PATH", "GH_TOKEN", "GITHUB_TOKEN"] {
            let err = KeyVar::try_from(reserved.to_owned()).unwrap_err();
            assert!(err.contains(reserved), "{reserved}: {err}");
        }
        let kept = KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap();
        assert_eq!(kept.as_str(), "PADDOCK_KEY");
    }

    #[test]
    fn a_listed_agent_on_an_https_gateway_is_refused_saying_the_forwarder_speaks_http() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("local.md"),
            "---\nrole: implementer\nharness: pi\nmodel: m\neffort: low\n\
             gateway: paddock\ncontext: 65536\n---\n",
        )
        .unwrap();
        let book = Agents::load(dir.path()).unwrap();
        let listed = [AgentName::try_from("local".to_owned()).unwrap()];
        let check = |url: &str| {
            let gateways = Gateways::reading(paddock(url), |_| Some("pk-1".into()));
            gateways.check_listed(&book, &listed)
        };
        check("http://gpu:8700").unwrap();
        let err = check("https://gpu:8700").unwrap_err().to_string();
        assert!(err.contains("local"), "{err}");
        assert!(err.contains("plain `http://`"), "{err}");
    }

    #[test]
    fn a_check_without_keys_holds_the_shape_and_names_no_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("local.md"),
            "---\nrole: implementer\nharness: pi\nmodel: m\neffort: low\n\
             gateway: paddock\ncontext: 65536\n---\n",
        )
        .unwrap();
        let book = Agents::load(dir.path()).unwrap();
        let listed = [AgentName::try_from("local".to_owned()).unwrap()];
        let check = |table| {
            let gateways = Gateways::reading(table, |_| None);
            gateways.check_listed_without_keys(&book, &listed)
        };
        check(paddock("http://gpu:8700")).unwrap();
        let err = check(paddock("https://gpu:8700")).unwrap_err().to_string();
        assert!(err.contains("plain `http://`"), "{err}");
        let err = check(BTreeMap::new()).unwrap_err().to_string();
        assert!(
            err.contains("local: gateway paddock is not in kelpie's settings")
                && !err.contains("PADDOCK_KEY"),
            "{err}"
        );
    }

    // `triage.spam.gateway`, which is a gateway name and not an agent's
    fn spam_on(name: &str) -> TriageSpam {
        TriageSpam {
            gateway: Some(GatewayName::try_from(name.to_owned()).unwrap()),
            path: Some(ClassifierPath::try_from("/classify".to_owned()).unwrap()),
            threshold: Probability::millionths(800_000),
        }
    }

    #[test]
    fn a_triage_spam_gateway_kelpie_does_not_have_is_refused_naming_it() {
        let gateways = Gateways::reading(paddock("http://gpu:8700"), |_| Some("pk-1".into()));
        gateways.check_triage_spam(&TriageSpam::default()).unwrap();
        gateways.check_triage_spam(&spam_on("paddock")).unwrap();
        let err = gateways
            .check_triage_spam(&spam_on("other"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("triage.spam.gateway") && err.contains("other"),
            "{err}"
        );
        assert!(err.contains("[kelpie.gateways.other]"), "{err}");
    }

    #[test]
    fn a_triage_spam_gateway_the_forwarder_cannot_dial_is_refused_saying_it_speaks_http() {
        let mut table = paddock("https://gpu:8700");
        table.insert(
            GatewayName::try_from("secure".to_owned()).unwrap(),
            Gateway {
                kind: GatewayKind::Paddock,
                url: EndpointUrl::try_from("https://gpu:8700".to_owned()).unwrap(),
                key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
            },
        );
        let gateways = Gateways::reading(table, |_| Some("pk-1".into()));
        let err = gateways
            .check_triage_spam(&spam_on("secure"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("triage.spam.gateway") && err.contains("plain `http://`"),
            "{err}"
        );
    }
}
