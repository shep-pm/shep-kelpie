//! Each gateway in kelpie's own settings: that it answers, that it takes
//! kelpie's key, and that it lists every agent's model on it
//!
//! The model list needs no key. The key is read from this shell's
//! environment, which may not be the runner's, so a key not set here is
//! unsure rather than missing.

use std::collections::BTreeSet;

use super::Line;
use crate::agents::{Agent, Agents};
use crate::forwarder::Upstream;
use crate::ports::KeyCheck;
use crate::settings::{AgentName, Gateway, Gateways};

/// A line for each gateway's reach and key, and for each agent's model on one
///
/// A shipped agent as kelpie ships it is in use only where a project `listed` it, so an
/// unlisted one gets no line, whatever its gateway serves or lacks.
pub(super) fn checks(
    gateways: &Gateways,
    agents: &Agents,
    listed: &BTreeSet<AgentName>,
) -> Vec<Line> {
    let shipped = Agents::embedded();
    let idle = |name: &AgentName, agent: &Agent| {
        !listed.contains(name) && shipped.get(name) == Some(agent)
    };
    let mut lines = Vec::new();
    for (name, gateway) in gateways.iter() {
        let subject = format!("gateway {name}");
        let port = crate::adapters::port(gateways, gateway);
        let found = gateways.key(gateway);
        let fix = "start the gateway, or put `url` right in kelpie's settings";
        let upstream = match Upstream::new(&gateway.base()) {
            Ok(upstream) => upstream,
            Err(why) => {
                lines.push(Line::missing(subject, why.to_string(), fix));
                continue;
            }
        };
        // A gateway may list its models to a key alone, so the list is asked
        // with it when this shell has it. A kind that needs none ignores it.
        let upstream = match &found {
            Ok(key) => upstream.with_key(key.clone()),
            Err(_) => upstream,
        };
        let listed = match port.models(&upstream) {
            Ok(listed) => listed,
            Err(why) => {
                lines.push(Line::missing(&subject, &why, fix));
                // A gateway that lists to a key alone shows a refused key here,
                // or one this shell lacks.
                match &found {
                    Ok(_) if port.key(&upstream) == KeyCheck::Refused => {
                        lines.push(key_line(&subject, gateway, &KeyCheck::Refused));
                    }
                    Err(unset) if answered_no_key(&why) => {
                        lines.push(unset_key_line(&subject, gateway, unset));
                    }
                    _ => {}
                }
                continue;
            }
        };
        lines.push(Line::ok(&subject, format!("lists {} models", listed.len())));
        lines.push(match found {
            Err(why) => unset_key_line(&subject, gateway, &why),
            Ok(_) => key_line(&subject, gateway, &port.key(&upstream)),
        });
        for (agent, model) in on(agents, name.as_str(), &idle) {
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
            && !idle(agent, agent_def)
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

// The line for a key this shell does not have, which may not be the runner's.
fn unset_key_line(subject: &str, gateway: &Gateway, why: &str) -> Line {
    let var = gateway.key_env.as_str();
    let next = format!(
        "the runner reads it from its sheep's `env`. To check the key, give \
         doctor it for one run: ` {var}=<key> shep kelpie doctor`, the leading \
         space keeping it out of the shell's history"
    );
    Line::unsure(format!("{subject}: key"), why, next)
}

// Whether a failed list was the gateway turning the request away for want of a key.
fn answered_no_key(why: &str) -> bool {
    why.ends_with("HTTP 401") || why.ends_with("HTTP 403")
}

// The line for `gateway`'s answer to its key.
fn key_line(subject: &str, gateway: &Gateway, check: &KeyCheck) -> Line {
    let key = format!("{subject}: key");
    match check {
        KeyCheck::Taken => Line::ok(key, "taken"),
        KeyCheck::Refused => Line::missing(
            key,
            format!(
                "refused: {} holds no key it takes",
                gateway.key_env.as_str()
            ),
            "give kelpie a client key from the gateway's own settings",
        ),
        KeyCheck::Unsure(why) => Line::unsure(key, why, "run doctor again"),
    }
}

// Each agent in use whose model is behind `gateway`, with the model's name there.
fn on<'a>(
    agents: &'a Agents,
    gateway: &'a str,
    idle: &'a dyn Fn(&AgentName, &Agent) -> bool,
) -> impl Iterator<Item = (String, &'a str)> {
    agents.iter().filter_map(move |(name, agent)| {
        let (on, model) = agent.runs.gateway()?;
        (on.as_str() == gateway && !idle(name, agent)).then(|| (name.to_string(), model))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::doctor::Verdict;
    use crate::settings::{EndpointUrl, Gateway, GatewayKind, GatewayName, KeyVar};
    use crate::test::{Answer, StandInEndpoint, unreachable_url};

    fn gateways(url: &str, env: fn(&str) -> Option<String>) -> Gateways {
        of_kind(GatewayKind::Paddock, url, env)
    }

    fn of_kind(kind: GatewayKind, url: &str, env: fn(&str) -> Option<String>) -> Gateways {
        let gateway = Gateway {
            kind,
            url: EndpointUrl::try_from(url.to_owned()).unwrap(),
            key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
        };
        let name = GatewayName::try_from("paddock".to_owned()).unwrap();
        Gateways::reading(BTreeMap::from([(name, gateway)]), env)
    }

    fn openai(url: &str, env: fn(&str) -> Option<String>) -> Gateways {
        of_kind(GatewayKind::Openai, url, env)
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

    // A reviewer on the model `iq3_xxs`, the one the lineup these tests were built on put on paddock
    const IQ3_XXS_REVIEW: &str = "---\nrole: reviewer\nharness: endpoint\n\
                                  gateway: paddock\nmodel: iq3_xxs\ncontext: 262144\n---\n";

    fn named(names: &[&str]) -> BTreeSet<AgentName> {
        let name = |n: &&str| AgentName::try_from((*n).to_owned()).unwrap();
        names.iter().map(name).collect()
    }

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
            .like_paddock("pk-right", &["qwen3.8:27b", "iq3_xxs"]);
        let book = agents(&[
            ("local.md", ON_PADDOCK),
            ("box.md", REVIEWS_ON_PADDOCK),
            ("iq3-xxs-review.md", IQ3_XXS_REVIEW),
        ]);
        let lines = checks(
            &gateways(server.host(), |_| Some("pk-right".into())),
            &book,
            &named(&["iq3-xxs-review"]),
        );
        assert_eq!(
            verdicts(&lines),
            [
                ("gateway paddock".to_owned(), "ok"),
                ("gateway paddock: key".to_owned(), "ok"),
                ("gateway paddock: box".to_owned(), "missing"),
                ("gateway paddock: iq3-xxs-review".to_owned(), "ok"),
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
        let lines = checks(
            &gateways(server.host(), |_| Some("pk-wrong".into())),
            &book,
            &named(&[]),
        );
        assert_eq!(
            verdicts(&lines)[1],
            ("gateway paddock: key".into(), "missing")
        );
        assert!(!lines[1].to_string().contains("pk-wrong"), "{}", lines[1]);
        let lines = checks(&gateways(server.host(), |_| None), &book, &named(&[]));
        assert_eq!(
            verdicts(&lines)[1],
            ("gateway paddock: key".into(), "unsure")
        );
    }

    #[test]
    fn a_gateway_that_does_not_answer_or_an_agent_naming_none_is_missing() {
        let down = unreachable_url();
        let book = agents(&[("local.md", &ON_PADDOCK.replace("paddock", "elsewhere"))]);
        let lines = checks(
            &gateways(down.trim_end_matches("/v1"), |_| None),
            &book,
            &named(&[]),
        );
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

    #[test]
    fn an_agent_file_no_project_lists_is_still_looked_for_on_a_gateway_that_serves_other_models() {
        let server = StandInEndpoint::start([]).like_paddock("pk-right", &["qwen3.8:27b"]);
        let gateways = gateways(server.host(), |_| Some("pk-right".into()));
        let book = agents(&[
            ("local.md", ON_PADDOCK),
            ("iq3-xxs-review.md", IQ3_XXS_REVIEW),
        ]);

        let lines = checks(&gateways, &book, &named(&[]));
        let missing = ("gateway paddock: iq3-xxs-review".to_owned(), "missing");
        assert!(verdicts(&lines).contains(&missing), "{lines:?}");
        let lines = checks(&gateways, &book, &named(&["iq3-xxs-review"]));
        assert!(verdicts(&lines).contains(&missing), "{lines:?}");
    }

    #[test]
    fn kelpie_ships_no_agent_that_names_a_gateway() {
        let book = agents(&[]);
        assert_eq!(on(&book, "paddock", &|_, _| false).count(), 0);
    }

    #[test]
    fn doctor_takes_an_openai_gateways_key_from_its_models_route() {
        let server =
            StandInEndpoint::start([]).like_openai("sk-right", &["qwen3.8:27b", "iq3_xxs"]);
        let book = agents(&[
            ("local.md", ON_PADDOCK),
            ("iq3-xxs-review.md", IQ3_XXS_REVIEW),
        ]);
        let lines = checks(
            &openai(server.host(), |_| Some("sk-right".into())),
            &book,
            &named(&["iq3-xxs-review"]),
        );
        assert_eq!(
            verdicts(&lines),
            [
                ("gateway paddock".to_owned(), "ok"),
                ("gateway paddock: key".to_owned(), "ok"),
                ("gateway paddock: iq3-xxs-review".to_owned(), "ok"),
                ("gateway paddock: local".to_owned(), "ok"),
            ]
        );
        let seen = server.seen();
        assert!(
            seen.iter().all(|l| l == "GET /v1/models HTTP/1.0"),
            "{seen:?}"
        );
    }

    #[test]
    fn doctor_reports_an_openai_gateway_that_refuses_the_key_as_missing() {
        let server = StandInEndpoint::start([]).like_openai("sk-right", &[]);
        let book = agents(&[]);
        let lines = checks(
            &openai(server.host(), |_| Some("sk-wrong".into())),
            &book,
            &named(&[]),
        );
        let kinds = verdicts(&lines);
        assert!(
            kinds.contains(&("gateway paddock: key".to_owned(), "missing")),
            "{kinds:?}"
        );
        let shown: String = lines.iter().map(ToString::to_string).collect();
        assert!(!shown.contains("sk-wrong"), "{shown}");
    }

    #[test]
    fn an_openai_gateway_that_lists_to_a_key_alone_is_unsure_of_a_key_this_shell_lacks() {
        let server = StandInEndpoint::start([]).like_openai("sk-right", &[]);
        let lines = checks(&openai(server.host(), |_| None), &agents(&[]), &named(&[]));
        let kinds = verdicts(&lines);
        assert_eq!(
            kinds,
            [
                ("gateway paddock".to_owned(), "missing"),
                ("gateway paddock: key".to_owned(), "unsure"),
            ]
        );
        assert!(lines[1].to_string().contains("PADDOCK_KEY"), "{}", lines[1]);
    }
}
