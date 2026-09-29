//! `kelpie relay-yes` and `relay-answer`: a ruling sent to kelpie's shepherd
//!
//! Both reach the shepherd over its socket through the shep client kelpie
//! is built with, never through a `shep` binary. A `shep` found on `PATH`
//! can be another version, and an older one can take over a newer
//! shepherd. A shepherd on another shep major or minor is refused with the
//! reason and nothing to run, since shep's own advice for a skew is a reload.

use std::path::Path;
use std::process::ExitCode;

use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::{ActionOutcome, Response, SelectorSpec};
use shep_client::{Client, ConnectError, TRIGGER_DEADLINE};

use crate::runner::{RELAY_RULE, is_no_or_answer};

/// The shep version kelpie is built with, pinned in the workspace manifest
pub const SHEP_VERSION: &str = "0.10.1";

/// A ruling the relay passes on to a runner's `relay-rule` action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ruling<'a> {
    /// `kelpie relay-yes`: the ruling's id, sent as `<id> yes`
    Yes(&'a str),
    /// `kelpie relay-answer`: a `<id> no <note>` or `<id> answer <text>`
    NoOrAnswer(&'a str),
}

impl Ruling<'_> {
    // `relay-answer` is pre-allowed, so a yes disguised as an answer stops
    // here: the `ask` rule on `relay-yes` is the only way one merges.
    fn params(self) -> Result<String, String> {
        match self {
            Self::Yes(id) => Ok(format!("{id} yes")),
            Self::NoOrAnswer(params) if is_no_or_answer(params) => Ok(params.to_owned()),
            Self::NoOrAnswer(params) => Err(format!(
                "relay-answer refuses {params:?}: not a `<id> no <note>` or `<id> answer <text>`"
            )),
        }
    }
}

/// Sends `ruling` to `project`'s runner on the shepherd at `shep_home`
///
/// Prints the runner's reply, or why the ruling was not sent.
pub fn send(shep_home: &Path, project: &str, ruling: Ruling<'_>) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let sent = match runtime {
        Ok(runtime) => runtime.block_on(deliver(shep_home, project, ruling)),
        Err(e) => Err(format!("cannot start the async runtime: {e}")),
    };
    match sent {
        Ok(reply) => {
            println!("{reply}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

async fn deliver(shep_home: &Path, project: &str, ruling: Ruling<'_>) -> Result<String, String> {
    let params = ruling.params()?;
    let client = Client::connect(&shep_home.join("run/shep.sock"))
        .await
        .map_err(|e| match e {
            ConnectError::ProtocolMismatch { daemon_version, .. } => {
                skew(shep_home, daemon_version.as_deref())
            }
            other => format!(
                "cannot reach kelpie's shepherd at {}: {other}",
                shep_home.display()
            ),
        })?;
    let running = client.daemon().daemon_version.as_str();
    if release_line(running) != release_line(SHEP_VERSION) {
        return Err(skew(shep_home, Some(running)));
    }
    let mut outcome = trigger(&client, project, RELAY_RULE, &params).await?;
    // A runner started before `relay-rule` existed takes the same ruling as `rule`.
    if let Some(ActionOutcome::Replied { body }) = &outcome
        && refusal(body).is_some_and(|why| why == format!("unknown action `{RELAY_RULE}`"))
    {
        outcome = trigger(&client, project, "rule", &params).await?;
    }
    match outcome {
        Some(ActionOutcome::Replied { body }) => match refusal(&body) {
            None => Ok(body),
            Some(why) => Err(format!(
                "{project}'s runner refused the ruling: {why}. \
                 Tell the maintainer, and send nothing else."
            )),
        },
        Some(other) => Err(format!("{project}'s runner did not answer: {other:?}")),
        None => Err(format!("kelpie's shepherd runs no sheep named {project}")),
    }
}

// The first sheep's outcome of `action`, under `shep trigger`'s own budget:
// a ruling can wait on the runner's lock.
async fn trigger(
    client: &Client,
    project: &str,
    action: &str,
    params: &str,
) -> Result<Option<ActionOutcome>, String> {
    let trigger = Request::Trigger {
        selector: SelectorSpec::Name(project.into()),
        action: action.into(),
        params: Some(params.into()),
    };
    let reply = client
        .request_with_deadline(trigger, Some(TRIGGER_DEADLINE))
        .await
        .map_err(|e| format!("kelpie's shepherd did not take the ruling: {e}"))?;
    let Response::Triggered(rows) = reply else {
        return Err(format!("kelpie's shepherd answered {reply:?}"));
    };
    Ok(rows.into_iter().next().map(|row| row.outcome))
}

// The `error` a runner's reply carries, if any. A reply that is not JSON
// carries none.
fn refusal(body: &str) -> Option<String> {
    match serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
    {
        serde_json::Value::Null => None,
        serde_json::Value::String(why) => Some(why.clone()),
        why => Some(why.to_string()),
    }
}

// A version's major and minor. A patch release of the pinned line is
// accepted, so shep's patch releases never need a kelpie rebuild.
fn release_line(version: &str) -> (Option<&str>, Option<&str>) {
    let mut parts = version.split('.');
    (parts.next(), parts.next())
}

// Names nothing to run: shep's own advice for a skew is a reload, and a
// relay that ran it with the wrong `shep` could replace kelpie's shepherd.
fn skew(shep_home: &Path, running: Option<&str>) -> String {
    let running = running.map_or_else(
        || "a shep version it did not name".to_owned(),
        |v| format!("shep {v}"),
    );
    let (major, minor) = release_line(SHEP_VERSION);
    format!(
        "kelpie's shepherd at {} runs {running}, and kelpie is built for shep {SHEP_VERSION} \
         and takes only a {}.{}.x shepherd, so the ruling was not sent. \
         Tell the maintainer, and leave the shepherd as it is.",
        shep_home.display(),
        major.unwrap_or_default(),
        minor.unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::shep_core::protocol::request::ActionReply;
    use shep_client::shep_core::protocol::{Envelope, HelloAck, RpcError, RpcErrorCode};
    use shep_client::testing::{fake_daemon, fake_daemon_answering_with_ack, sample_ack};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;

    // Bounds every call against a fake shepherd, so a hang fails by name.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn ack(version: &str) -> HelloAck {
        let mut ack = sample_ack();
        ack.daemon_version = version.into();
        ack
    }

    fn scratch_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("run")).unwrap();
        home
    }

    // A shepherd on `version` whose `project` sheep answers every action
    // with `outcome`, and the requests it was sent.
    async fn shepherd(
        home: &Path,
        version: &str,
        outcome: ActionOutcome,
    ) -> UnboundedReceiver<Envelope> {
        fake_daemon_answering_with_ack(&home.join("run/shep.sock"), ack(version), move |_| {
            Response::Triggered(vec![ActionReply {
                id: 1,
                name: "shep".into(),
                outcome: outcome.clone(),
            }])
        })
        .await
    }

    fn replied(body: &str) -> ActionOutcome {
        ActionOutcome::Replied { body: body.into() }
    }

    async fn deliver_in_time(home: &Path, ruling: Ruling<'_>) -> Result<String, String> {
        tokio::time::timeout(PATIENCE, deliver(home, "shep", ruling))
            .await
            .expect("the ruling neither went nor failed in time")
    }

    fn rule_trigger(params: &str) -> Request {
        trigger_of("relay-rule", params)
    }

    fn trigger_of(action: &str, params: &str) -> Request {
        Request::Trigger {
            selector: SelectorSpec::Name("shep".into()),
            action: action.into(),
            params: Some(params.into()),
        }
    }

    // A runner started on an older kelpie answers only `rule`.
    #[tokio::test]
    async fn a_runner_without_relay_rule_gets_the_ruling_as_rule() {
        let home = scratch_home();
        let socket = home.path().join("run/shep.sock");
        let mut sent = fake_daemon_answering_with_ack(&socket, ack(SHEP_VERSION), |request| {
            let body = match request {
                Request::Trigger { action, .. } if action == "rule" => "merging #71",
                _ => r#"{"error":"unknown action `relay-rule`"}"#,
            };
            Response::Triggered(vec![ActionReply {
                id: 1,
                name: "shep".into(),
                outcome: replied(body),
            }])
        })
        .await;
        let reply = deliver_in_time(home.path(), Ruling::Yes("3")).await;
        assert_eq!(reply, Ok("merging #71".into()));
        assert_eq!(sent.try_recv().unwrap().body, rule_trigger("3 yes"));
        assert_eq!(sent.try_recv().unwrap().body, trigger_of("rule", "3 yes"));
    }

    // Real sockets under a real clock: a paused one would time out the
    // handshake while the fake's socket is merely waiting.
    #[tokio::test]
    async fn a_yes_reaches_the_projects_rule_action() {
        let home = scratch_home();
        let mut sent = shepherd(home.path(), SHEP_VERSION, replied("merging #71")).await;
        let reply = deliver_in_time(home.path(), Ruling::Yes("3")).await;
        assert_eq!(reply, Ok("merging #71".into()));
        assert_eq!(sent.try_recv().unwrap().body, rule_trigger("3 yes"));
    }

    #[tokio::test]
    async fn a_ruling_gets_shep_trigger_s_full_minute() {
        let home = scratch_home();
        let mut sent = shepherd(home.path(), SHEP_VERSION, replied("")).await;
        deliver_in_time(home.path(), Ruling::Yes("3"))
            .await
            .unwrap();
        assert_eq!(sent.try_recv().unwrap().deadline_ms, Some(60_000));
    }

    #[tokio::test]
    async fn a_no_or_an_answer_passes_through_verbatim() {
        for params in ["3 no rename the flag", "3 answer use --dry-run"] {
            let home = scratch_home();
            let mut sent = shepherd(home.path(), SHEP_VERSION, replied("")).await;
            let reply = deliver_in_time(home.path(), Ruling::NoOrAnswer(params)).await;
            assert_eq!(reply, Ok(String::new()), "{params:?}");
            assert_eq!(sent.try_recv().unwrap().body, rule_trigger(params));
        }
    }

    // No shepherd listens, so a refusal that names the grammar was made
    // before anything was sent.
    #[tokio::test]
    async fn relay_answer_refuses_every_shape_of_yes() {
        let home = scratch_home();
        for disguised in ["3 yes", "3 Yes", " 3 yes", "3  yes", "3 yes extra"] {
            let refused = deliver_in_time(home.path(), Ruling::NoOrAnswer(disguised))
                .await
                .unwrap_err();
            assert!(refused.starts_with("relay-answer refuses"), "{refused}");
        }
    }

    #[tokio::test]
    async fn a_shepherd_on_another_minor_or_major_gets_no_ruling() {
        for version in ["0.8.2", "0.9.4", "0.11.0", "1.10.1", "0.100.1"] {
            let home = scratch_home();
            let mut sent = shepherd(home.path(), version, replied("merging #71")).await;
            let refused = deliver_in_time(home.path(), Ruling::Yes("3"))
                .await
                .unwrap_err();
            assert!(
                refused.contains(&format!("runs shep {version}")),
                "{refused}"
            );
            assert!(
                refused.contains("takes only a 0.10.x shepherd"),
                "{refused}"
            );
            assert!(!refused.contains("reload"), "{refused}");
            assert!(sent.try_recv().is_err(), "the ruling was sent to {version}");
        }
    }

    #[tokio::test]
    async fn a_patch_release_of_the_pinned_line_gets_the_ruling() {
        for version in ["0.10.0", "0.10.2"] {
            let home = scratch_home();
            let mut sent = shepherd(home.path(), version, replied("merging #71")).await;
            let reply = deliver_in_time(home.path(), Ruling::Yes("3")).await;
            assert_eq!(reply, Ok("merging #71".into()), "{version}");
            assert_eq!(sent.try_recv().unwrap().body, rule_trigger("3 yes"));
        }
    }

    #[tokio::test]
    async fn a_shepherd_refusing_the_protocol_is_named_without_shep_s_advice() {
        let home = scratch_home();
        let refusal = RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "client protocol 9 is below this shepherd's floor of 10".into(),
            daemon_version: Some("0.12.0".into()),
        };
        let _shepherd = fake_daemon(&home.path().join("run/shep.sock"), Err(refusal)).await;
        let refused = deliver_in_time(home.path(), Ruling::Yes("3"))
            .await
            .unwrap_err();
        assert!(refused.contains("runs shep 0.12.0"), "{refused}");
        assert!(!refused.contains("reload"), "{refused}");
    }

    #[tokio::test]
    async fn a_refused_ruling_fails_the_command_and_names_nothing_to_run() {
        let home = scratch_home();
        let error = r#"{"error":"ruling 3 is the worker's question, so it takes an answer, not a yes or no"}"#;
        let _sent = shepherd(home.path(), SHEP_VERSION, replied(error)).await;
        let refused = deliver_in_time(home.path(), Ruling::Yes("3"))
            .await
            .unwrap_err();
        assert!(
            refused.starts_with("shep's runner refused the ruling: ruling 3"),
            "{refused}"
        );
        assert!(refused.contains("Tell the maintainer"), "{refused}");
        assert!(!refused.contains('`'), "{refused}");
    }

    #[tokio::test]
    async fn a_runner_that_does_not_answer_fails_the_command() {
        let home = scratch_home();
        let _sent = shepherd(home.path(), SHEP_VERSION, ActionOutcome::NoChannel).await;
        let failed = deliver_in_time(home.path(), Ruling::Yes("3"))
            .await
            .unwrap_err();
        assert!(failed.contains("shep's runner did not answer"), "{failed}");
    }

    #[tokio::test]
    async fn a_project_the_shepherd_does_not_run_is_named() {
        let home = scratch_home();
        let socket = home.path().join("run/shep.sock");
        let _sent = fake_daemon_answering_with_ack(&socket, ack(SHEP_VERSION), |_| {
            Response::Triggered(Vec::new())
        })
        .await;
        let failed = deliver_in_time(home.path(), Ruling::Yes("3"))
            .await
            .unwrap_err();
        assert_eq!(failed, "kelpie's shepherd runs no sheep named shep");
    }

    #[tokio::test]
    async fn no_shepherd_at_the_home_is_named() {
        let home = scratch_home();
        let failed = deliver_in_time(home.path(), Ruling::Yes("3"))
            .await
            .unwrap_err();
        assert!(
            failed.starts_with("cannot reach kelpie's shepherd at"),
            "{failed}"
        );
    }

    #[test]
    fn the_version_is_the_one_the_workspace_pins() {
        let manifest = include_str!("../../../../Cargo.toml");
        for krate in ["shep-client", "shep-channel"] {
            let pin = format!("{krate} = {{ version = \"={SHEP_VERSION}\"");
            assert!(
                manifest.contains(&pin),
                "{krate} is not pinned to {SHEP_VERSION}"
            );
        }
    }
}
