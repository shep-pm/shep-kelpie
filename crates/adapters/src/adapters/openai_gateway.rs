//! A bare OpenAI-compatible gateway, for a gateway of kind `openai`
//!
//! LiteLLM, OpenRouter or a llama.cpp server know nothing of paddock's
//! routes, so [`OpenAi`] asks `/v1/models` alone, for the model list and for
//! the key, and offers no lease. A call on it starts at once, and the gateway
//! may evict the model it runs on. Requests are paddock's: one HTTP/1.0
//! exchange over plain `http://`.

use std::time::Duration;

use crate::adapters::paddock;
use crate::forwarder::Upstream;
use crate::ports::{KeyCheck, Metered, ModelGateway, Taking, Unheld};

// How long the key check gets to connect and to answer.
const ANSWER: Duration = Duration::from_secs(10);

/// The gateway port for a bare OpenAI-compatible gateway
#[derive(Debug, Clone, Copy)]
pub(crate) struct OpenAi;

impl ModelGateway for OpenAi {
    // Many such gateways list their models to a key alone, so it is sent when
    // `gateway` carries one.
    fn models(&self, gateway: &Upstream) -> Result<Vec<String>, String> {
        paddock::list(gateway, gateway.key().is_some())
    }

    fn key(&self, gateway: &Upstream) -> KeyCheck {
        let asked = ("GET", "/v1/models");
        match paddock::send(gateway, asked, None, true, ANSWER) {
            Ok((200, _)) => KeyCheck::Taken,
            Ok((401 | 403, _)) => KeyCheck::Refused,
            Ok((status, _)) => KeyCheck::Unsure(format!("`GET /v1/models` answered HTTP {status}")),
            Err(why) => KeyCheck::Unsure(why),
        }
    }

    fn lease(
        &self,
        _: &Upstream,
        _: &Taking<'_>,
        _: &mut dyn FnMut(&str),
    ) -> Option<Result<Option<Metered>, Unheld>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{EndpointUrl, GatewayKey};
    use crate::test::{Answer, StandInEndpoint};

    const KEY: &str = "sk-right";

    fn gateway(server: &StandInEndpoint, key: Option<&str>) -> Upstream {
        let base = EndpointUrl::try_from(server.url().to_owned()).unwrap();
        let upstream = Upstream::new(&base).unwrap();
        match key {
            Some(key) => upstream.with_key(GatewayKey::for_test(key)),
            None => upstream,
        }
    }

    #[test]
    fn its_models_are_the_ids_its_models_route_lists_to_the_key() {
        let server = StandInEndpoint::start([]).like_openai(KEY, &["qwen", "coder"]);
        let listed = OpenAi.models(&gateway(&server, Some(KEY)));
        assert_eq!(listed, Ok(vec!["qwen".to_owned(), "coder".to_owned()]));
        assert_eq!(server.seen(), ["GET /v1/models HTTP/1.0"]);
    }

    #[test]
    fn a_key_it_takes_is_taken_and_one_it_refuses_is_refused_without_naming_it() {
        let server = StandInEndpoint::start([]).like_openai(KEY, &[]);
        assert_eq!(OpenAi.key(&gateway(&server, Some(KEY))), KeyCheck::Taken);
        assert_eq!(
            OpenAi.key(&gateway(&server, Some("sk-wrong"))),
            KeyCheck::Refused
        );
        let auths = server.authorizations();
        assert_eq!(auths[0], Some(format!("Bearer {KEY}")));
    }

    #[test]
    fn a_403_is_refused_too() {
        let server = StandInEndpoint::start([]).with_models_status(403);
        assert_eq!(OpenAi.key(&gateway(&server, Some(KEY))), KeyCheck::Refused);
    }

    #[test]
    fn an_openai_gateways_unexpected_status_is_unsure_naming_it() {
        let server = StandInEndpoint::start([]).with_models_status(503);
        let KeyCheck::Unsure(why) = OpenAi.key(&gateway(&server, Some(KEY))) else {
            panic!("a 503 is neither a yes nor a no");
        };
        assert!(why.contains("503"), "{why}");
        assert!(!why.contains(KEY), "{why}");
    }

    #[test]
    fn it_offers_no_lease_so_a_call_runs_at_once_and_nothing_is_asked() {
        let server = StandInEndpoint::start([Answer::Says("unused")]).like_openai(KEY, &[]);
        let port: &dyn ModelGateway = &OpenAi;
        let ending = crate::ports::Ending::default();
        let taking = Taking {
            model: "qwen",
            note: "n",
            give_up: &|| true,
            tell: &|_| panic!("a kind with no lease tells of no take"),
            ending: &ending,
        };
        let ran = port.around(&gateway(&server, Some(KEY)), &taking, || 7);
        assert_eq!(ran, Ok(7));
        assert!(server.seen().is_empty(), "{:?}", server.seen());
    }
}
