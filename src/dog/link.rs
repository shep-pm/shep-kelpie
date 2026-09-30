//! The dog's connections to the shepherd: named in the handshake, and kept
//! across a shepherd restart
//!
//! Shep lists a dog `online` only once it has handshaken under its own name,
//! and a dog that never does is `silent`: restarted once, then given up on.
//! So both connections are made with the name shep put in `SHEP_DOG_NAME`,
//! and a reconnecting client re-announces it to each successor shepherd.

use std::path::Path;
use std::time::Duration;

use shep_client::dogs::DogIdentity;
use shep_client::{EventStream, LinkLost, ReconnectingClient, RequestError};
use tokio::time::Instant;

/// How long the dog waits for a restarted shepherd before it gives up and exits
pub const BUDGET: Duration = Duration::from_secs(30);

/// How long between tries while the shepherd is coming back
const RETRY: Duration = Duration::from_millis(250);

/// The bus topics the dog listens to
const TOPICS: [&str; 2] = ["channel.metric", "process.*"];

/// Connects as the dog the shepherd named in `SHEP_DOG_NAME`
///
/// Kelpie's own name stands in for the section only. With no name from the
/// shepherd the handshake names no dog, since a guessed name gets some
/// other dog restarted for this one's fault.
///
/// # Errors
///
/// A message when the shepherd's socket cannot be reached or refuses the
/// handshake. `what` says which connection it was.
pub async fn connect(
    socket: &Path,
    identity: &DogIdentity,
    what: &str,
) -> Result<ReconnectingClient, String> {
    ReconnectingClient::connect_as(socket, identity)
        .await
        .map_err(|e| {
            format!(
                "cannot reach the shepherd at {} {what}: {e}",
                socket.display()
            )
        })
}

/// This process's dog identity, read from the environment
pub fn identity() -> DogIdentity {
    DogIdentity::from_env(&|key| std::env::var(key).ok(), super::NAME)
}

/// Subscribes `listener` to the bus, waiting up to `budget` for a shepherd
/// that is coming back
///
/// A stream ends when its connection dies and is not re-armed by the
/// reconnect, so this is called again each time one ends.
///
/// # Errors
///
/// A message when the shepherd refuses the dog, does not come back in
/// `budget`, or refuses the subscription.
pub async fn subscribe(
    listener: &ReconnectingClient,
    budget: Duration,
) -> Result<EventStream, String> {
    let deadline = Instant::now() + budget;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        up(listener.connected_within(left).await)?;
        let topics = TOPICS.iter().map(|t| (*t).to_owned()).collect();
        match listener.subscribe(topics).await {
            Ok(events) => return Ok(events),
            // The link can read connected for a moment after it is cut.
            Err(RequestError::Closed) if Instant::now() < deadline => {
                tokio::time::sleep(RETRY).await;
            }
            Err(e) => return Err(format!("cannot subscribe to the shepherd's bus: {e}")),
        }
    }
}

/// Waits up to `budget` for `client` to be connected again
///
/// # Errors
///
/// A message when the shepherd refuses the dog or does not come back.
pub async fn wait_for(client: &ReconnectingClient, budget: Duration) -> Result<(), String> {
    up(client.connected_within(budget).await)
}

/// Pauses before the caller tries again
pub async fn pause() {
    tokio::time::sleep(RETRY).await;
}

fn up(link: Result<(), LinkLost>) -> Result<(), String> {
    link.map_err(|lost| match lost {
        LinkLost::Refused { message, .. } => {
            format!("the restarted shepherd refused the dog: {message}")
        }
        LinkLost::Budget { waited } => {
            format!("the shepherd did not come back in {}s", waited.as_secs())
        }
        other => format!("the shepherd is gone: {other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use shep_client::testing::{
        Handshake, control_address, fake_daemon_across_handovers, sample_ack,
    };

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(10);

    fn named() -> DogIdentity {
        DogIdentity::named("kelpie")
    }

    // The handshake names `kelpie`, which is what shep lists online.
    #[tokio::test]
    async fn both_connections_name_kelpie_in_the_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let shepherd = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        connect(&socket, &named(), "for events").await.unwrap();
        connect(&socket, &named(), "for requests").await.unwrap();
        let names: Vec<_> = shepherd.hellos().into_iter().map(|h| h.dog_name).collect();
        assert_eq!(
            names,
            [Some("kelpie".to_owned()), Some("kelpie".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_process_no_shepherd_named_announces_no_dog() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let shepherd = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        let unnamed = DogIdentity::from_env(&|_| None, "kelpie");
        connect(&socket, &unnamed, "for events").await.unwrap();
        assert_eq!(shepherd.hellos()[0].dog_name, None);
    }

    // A restarted shepherd gets the name again, and the bus stream is
    // armed again, rather than the dog exiting with its bus.
    #[tokio::test]
    async fn a_restarted_shepherd_hears_the_name_and_the_bus_is_subscribed_again() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let shepherd = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        let listener = connect(&socket, &named(), "for events").await.unwrap();
        let mut events = subscribe(&listener, BUDGET).await.unwrap();
        shepherd.cut().await;
        let ended = tokio::time::timeout(PATIENCE, async { events.next().await.is_none() }).await;
        assert_eq!(ended, Ok(true), "the cut ends the stream");

        tokio::time::timeout(PATIENCE, subscribe(&listener, BUDGET))
            .await
            .expect("the dog neither resubscribed nor gave up in time")
            .unwrap();
        let names: Vec<_> = shepherd.hellos().into_iter().map(|h| h.dog_name).collect();
        assert_eq!(
            names,
            [Some("kelpie".to_owned()), Some("kelpie".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_successor_that_refuses_the_dog_ends_the_wait() {
        use shep_client::shep_core::protocol::{RpcError, RpcErrorCode};
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let refusal = RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "protocol 99".into(),
            daemon_version: None,
        };
        let shepherd = fake_daemon_across_handovers(
            &socket,
            vec![Handshake::Accept(sample_ack()), Handshake::Refuse(refusal)],
        );
        let listener = connect(&socket, &named(), "for events").await.unwrap();
        let mut events = subscribe(&listener, BUDGET).await.unwrap();
        shepherd.cut().await;
        while events.next().await.is_some() {}
        let err = tokio::time::timeout(PATIENCE, subscribe(&listener, BUDGET))
            .await
            .expect("a refusal neither waited out the budget nor ended the wait")
            .unwrap_err();
        assert!(err.contains("refused the dog"), "{err}");
    }
}
