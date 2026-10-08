//! Each gateway in kelpie's own settings: that it answers, that it takes
//! kelpie's key, and that it lists every agent's model on it
//!
//! The model list needs no key. The key is read from this shell's
//! environment, which may not be the runner's, so a key not set here is
//! unsure rather than missing.

use super::Line;
use crate::adapters::paddock;
use crate::agents::Agents;
use crate::forwarder::Upstream;
use crate::settings::Gateways;

/// A line for each gateway's reach and key, and for each agent's model on one
pub(super) fn checks(gateways: &Gateways, agents: &Agents) -> Vec<Line> {
    let mut lines = Vec::new();
    for (name, gateway) in gateways.iter() {
        let subject = format!("gateway {name}");
        let reached = Upstream::new(&gateway.base())
            .map_err(|e| e.to_string())
            .and_then(|upstream| Ok((paddock::models(&upstream)?, upstream)));
        let (listed, upstream) = match reached {
            Ok(reached) => reached,
            Err(why) => {
                let fix = "start the gateway, or put `url` right in kelpie's settings";
                lines.push(Line::missing(subject, why, fix));
                continue;
            }
        };
        lines.push(Line::ok(&subject, format!("lists {} models", listed.len())));
        let key = format!("{subject}: key");
        lines.push(match gateways.key(gateway) {
            Err(why) => {
                let var = gateway.key_env.as_str();
                let next = format!(
                    "the runner reads it from its sheep's `env`. To check the key, give \
                     doctor it for one run: ` {var}=<key> shep kelpie doctor`, the leading \
                     space keeping it out of the shell's history"
                );
                Line::unsure(key, why, next)
            }
            Ok(found) => match paddock::key_taken(&upstream.with_key(found)) {
                Ok(true) => Line::ok(key, "taken"),
                Ok(false) => Line::missing(
                    key,
                    format!(
                        "refused: {} holds no key it takes",
                        gateway.key_env.as_str()
                    ),
                    "give kelpie a client key from the gateway's own settings",
                ),
                Err(why) => Line::unsure(key, why, "run doctor again"),
            },
        });
        for (agent, model) in on(agents, name.as_str()) {
            let line = format!("{subject}: {agent}");
            lines.push(match listed.iter().any(|m| m == model) {
                true => Line::ok(line, format!("{model} is listed")),
                false => Line::missing(
                    line,
                    format!("{model} is not among its models"),
                    "name a model the gateway serves, or add this one to it",
                ),
            });
        }
    }
    for (agent, agent_def) in agents.iter() {
        if let Some((gateway, _)) = agent_def.runs.gateway()
            && gateways.get(gateway).is_none()
        {
            lines.push(Line::missing(
                format!("agent {agent}"),
                format!("names gateway {gateway}, which kelpie's settings lack"),
                format!("add `[kelpie.gateways.{gateway}]` with its `url` and `key_env`"),
            ));
        }
    }
    lines
}

// Each agent whose model is behind `gateway`, with the model's name there.
fn on<'a>(agents: &'a Agents, gateway: &'a str) -> impl Iterator<Item = (String, &'a str)> {
    agents.iter().filter_map(move |(name, agent)| {
        let (on, model) = agent.runs.gateway()?;
        (on.as_str() == gateway).then(|| (name.to_string(), model))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::doctor::Verdict;
    use crate::settings::{EndpointUrl, Gateway, GatewayName, KeyVar};
    use crate::test::{Answer, StandInEndpoint, unreachable_url};

    fn gateways(url: &str, env: fn(&str) -> Option<String>) -> Gateways {
        let gateway = Gateway {
            url: EndpointUrl::try_from(url.to_owned()).unwrap(),
            key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
        };
        let name = GatewayName::try_from("paddock".to_owned()).unwrap();
        Gateways::reading(BTreeMap::from([(name, gateway)]), env)
    }

    fn agents(files: &[(&str, &str)]) -> Agents {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        Agents::load(dir.path()).unwrap()
    }

    const ON_PADDOCK: &str = "---\nrole: implementer\nharness: pi\nmodel: qwen3.8:27b\n\
                              effort: medium\ngateway: paddock\ncontext: 65536\n---\n";
    const REVIEWS_ON_PADDOCK: &str = "---\nrole: reviewer\nharness: endpoint\n\
                                      gateway: paddock\nmodel: coder\ncontext: 32768\n---\n";

    fn verdicts(lines: &[Line]) -> Vec<(String, &'static str)> {
        let kind = |v: &Verdict| match v {
            Verdict::Ok(_) => "ok",
            Verdict::Missing { .. } => "missing",
            Verdict::Unsure { .. } => "unsure",
        };
        lines
            .iter()
            .map(|l| (l.subject.clone(), kind(&l.verdict)))
            .collect()
    }

    #[test]
    fn a_gateway_is_reached_its_key_taken_and_each_agents_model_looked_for() {
        let server = StandInEndpoint::start([Answer::Says("unused")])
            .like_paddock("pk-right", &["qwen3.8:27b"]);
        let book = agents(&[("local.md", ON_PADDOCK), ("box.md", REVIEWS_ON_PADDOCK)]);
        let lines = checks(&gateways(server.host(), |_| Some("pk-right".into())), &book);
        assert_eq!(
            verdicts(&lines),
            [
                ("gateway paddock".to_owned(), "ok"),
                ("gateway paddock: key".to_owned(), "ok"),
                ("gateway paddock: box".to_owned(), "missing"),
                ("gateway paddock: local".to_owned(), "ok"),
            ]
        );
        assert!(
            lines[2]
                .to_string()
                .contains("coder is not among its models")
        );
        let auths = server.authorizations();
        assert_eq!(
            auths,
            [None, Some("Bearer pk-right".to_owned())],
            "only status"
        );
    }

    #[test]
    fn a_refused_key_is_missing_and_one_not_set_here_is_unsure() {
        let server = StandInEndpoint::start([]).like_paddock("pk-right", &[]);
        let book = agents(&[]);
        let lines = checks(&gateways(server.host(), |_| Some("pk-wrong".into())), &book);
        assert_eq!(
            verdicts(&lines)[1],
            ("gateway paddock: key".into(), "missing")
        );
        assert!(!lines[1].to_string().contains("pk-wrong"), "{}", lines[1]);
        let lines = checks(&gateways(server.host(), |_| None), &book);
        assert_eq!(
            verdicts(&lines)[1],
            ("gateway paddock: key".into(), "unsure")
        );
    }

    #[test]
    fn a_gateway_that_does_not_answer_or_an_agent_naming_none_is_missing() {
        let down = unreachable_url();
        let book = agents(&[("local.md", &ON_PADDOCK.replace("paddock", "elsewhere"))]);
        let lines = checks(&gateways(down.trim_end_matches("/v1"), |_| None), &book);
        assert_eq!(
            verdicts(&lines),
            [
                ("gateway paddock".to_owned(), "missing"),
                ("agent local".to_owned(), "missing"),
            ]
        );
        assert!(
            lines[1]
                .to_string()
                .contains("`[kelpie.gateways.elsewhere]`")
        );
    }
}
