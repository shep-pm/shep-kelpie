//! Gateways: one endpoint in front of a host's model servers, such as paddock
//!
//! Kelpie's own settings name each one, `[gateways.<name>]`, with its `url`
//! and the variable in kelpie's environment that holds its key. An agent
//! file names a gateway in place of a server's `url`. The gateway queues
//! its calls, so kelpie takes no lease around them and reads no `/api/ps`.
//! The key is read when a call needs it and sent only from kelpie's own
//! process: every sandboxed call has that variable unset.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;

use super::local::EndpointUrl;
use super::reviewers::lowercase_name;
use super::{AgentName, SettingsError};
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

/// One gateway in kelpie's own settings
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gateway {
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

impl TryFrom<String> for KeyVar {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_';
        match value.starts_with(|c: char| !c.is_ascii_digit()) && value.chars().all(allowed) {
            true => Ok(Self(value)),
            false => Err("must be an environment variable's name, such as PADDOCK_KEY"),
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
    #[cfg(test)]
    pub(crate) fn for_test(key: &str) -> Self {
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

/// Kelpie's gateways, as a runner read them when it started
#[derive(Clone)]
pub struct Gateways {
    table: BTreeMap<GatewayName, Gateway>,
    env: fn(&str) -> Option<String>,
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
        }
    }

    /// `table`, with each key read through `env`, as a test sets them
    #[cfg(test)]
    pub(crate) fn reading(
        table: BTreeMap<GatewayName, Gateway>,
        env: fn(&str) -> Option<String>,
    ) -> Self {
        Self { table, env }
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
    /// The reason, naming the variable, when it is unset or empty.
    pub fn key(&self, gateway: &Gateway) -> Result<GatewayKey, String> {
        let var = gateway.key_env.as_str();
        (self.env)(var).map(GatewayKey).ok_or_else(|| {
            format!("{var}, which holds the gateway's key, is not set in kelpie's environment")
        })
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
        let Some(gateway) = self.table.get(name) else {
            return Err(format!(
                "gateway {name} is not in kelpie's settings: add `[kelpie.gateways.{name}]` \
                 with its `url` and `key_env`"
            ));
        };
        Ok(Route {
            base: gateway.base(),
            key: Some(self.key(gateway)?),
        })
    }

    /// Refuses a start or a change whose `listed` agents name a gateway that
    /// cannot be routed to, as an endpoint that does not answer is refused
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming the agent and the reason.
    pub fn check_listed<'a>(
        &self,
        book: &Agents,
        listed: impl IntoIterator<Item = &'a AgentName>,
    ) -> Result<(), SettingsError> {
        for name in listed {
            let Some((gateway, _)) = book.get(name).and_then(|a| a.runs.gateway()) else {
                continue;
            };
            let host = ModelHost::Gateway(gateway.clone());
            self.route(&host).map_err(|reason| SettingsError::Invalid {
                setting: "agents",
                reason: format!("{name}: {reason}"),
            })?;
        }
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paddock(url: &str) -> BTreeMap<GatewayName, Gateway> {
        let gateway = Gateway {
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
}
