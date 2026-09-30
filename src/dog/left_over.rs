//! The `kelpie-dog` sheep a flock may still hold, which the adopted dog removes
//!
//! Before the adopted kelpie was the lease dog, `shep kelpie add` ran the
//! dog as a sheep of its own. Both read one book, so the adopted dog removes
//! that sheep before it opens the book, and never touches the file.

use std::path::Path;

use shep_client::ReconnectingClient;
use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::protocol::{Request, SelectorSpec};

/// The sheep the dog ran as before it was the adopted dog
pub const NAME: &str = "kelpie-dog";

/// Removes a left-over `kelpie-dog` sheep of kelpie's, and says so
///
/// Shep answers the delete once the sheep has exited, so the book it last
/// saved is the one `book` holds. A sheep of that name that kelpie did not
/// start as `dog` is left alone.
///
/// # Errors
///
/// A message when the shepherd refuses a request, or the sheep's entry sets
/// `KELPIE_HOME` or `HOME`, whose values shep withholds, so its book may
/// not be `book`.
pub async fn remove(client: &ReconnectingClient, book: &Path) -> Result<Option<String>, String> {
    let rows = match client.request(Request::ListFlock).await {
        Ok(Response::Flock(rows)) => rows,
        Ok(other) => return Err(format!("the flock listing came back as {other:?}")),
        Err(e) => return Err(format!("cannot list the flock: {e}")),
    };
    if !rows.iter().any(|r| r.name == NAME && r.dog.is_none()) {
        return Ok(None);
    }
    let view = match client
        .request(Request::SheepConfig { name: NAME.into() })
        .await
    {
        Ok(Response::SheepConfig(view)) => view,
        Ok(other) => return Err(format!("the shepherd answered {other:?} for `{NAME}`")),
        Err(e) => return Err(format!("cannot read `{NAME}`'s config: {e}")),
    };
    if view.config.args != ["dog"] {
        return Ok(None);
    }
    if let Some(key) = view
        .env_keys
        .iter()
        .find(|k| ["KELPIE_HOME", "HOME"].contains(&k.as_str()))
    {
        return Err(format!(
            "`{NAME}`, the sheep kelpie's dog ran as before, sets {key}, so its book may not be \
             {}: move its book there, then `shep delete {NAME}` and `shep restart kelpie`",
            book.display()
        ));
    }
    let delete = Request::Delete {
        selector: SelectorSpec::Name(NAME.into()),
    };
    match client.request(delete).await {
        Ok(Response::Deleted(_)) => Ok(Some(format!(
            "removed `{NAME}`, the sheep kelpie's dog ran as before, keeping its book at {}",
            book.display()
        ))),
        Ok(other) => Err(format!(
            "the shepherd answered {other:?} to deleting `{NAME}`"
        )),
        Err(e) => Err(format!("cannot delete `{NAME}`: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::shep_core::config::AppConfig;

    use super::*;
    use crate::test::FakeShepherd;

    // Bounds every call against a fake shepherd, so a hang fails by name.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn sheep(name: &str, args: &[&str]) -> AppConfig {
        let mut app = AppConfig::minimal(name, "/opt/kelpie");
        app.args = args.iter().map(|&a| a.to_owned()).collect();
        app
    }

    async fn removed(shepherd: &FakeShepherd) -> Result<Option<String>, String> {
        let socket = shepherd.home().join("run/shep.sock");
        let client = ReconnectingClient::connect(&socket).await.unwrap();
        let book = Path::new("/k/dog/book.json");
        tokio::time::timeout(PATIENCE, remove(&client, book))
            .await
            .expect("remove neither ended nor failed in time")
    }

    // Real sockets under a real clock: a paused one would time out the
    // handshake while the fake's socket is merely waiting.
    #[tokio::test]
    async fn a_left_over_dog_sheep_is_deleted_and_its_book_named() {
        let mut shepherd = FakeShepherd::new().await;
        shepherd.holds(sheep(NAME, &["dog"]), true);
        let said = removed(&shepherd).await.unwrap().unwrap();
        assert!(shepherd.sheep(NAME).is_none());
        assert!(
            said.ends_with("keeping its book at /k/dog/book.json"),
            "{said}"
        );
        let writes = shepherd.writes();
        assert!(
            matches!(writes.as_slice(), [Request::Delete { .. }]),
            "{writes:?}"
        );
    }

    #[tokio::test]
    async fn no_left_over_changes_nothing() {
        let mut shepherd = FakeShepherd::new().await;
        shepherd.holds(sheep("koji", &["runner", "koji"]), true);
        assert_eq!(removed(&shepherd).await, Ok(None));
        assert_eq!(shepherd.writes(), []);
    }

    #[tokio::test]
    async fn a_dog_of_that_name_is_not_a_left_over() {
        let mut shepherd = FakeShepherd::new().await;
        shepherd.holds_dog(NAME, true);
        assert_eq!(removed(&shepherd).await, Ok(None));
        assert!(shepherd.sheep(NAME).is_some());
        assert_eq!(shepherd.writes(), []);
    }

    #[tokio::test]
    async fn a_sheep_of_that_name_that_is_not_kelpie_s_stays() {
        let mut shepherd = FakeShepherd::new().await;
        shepherd.holds(sheep(NAME, &["serve"]), true);
        assert_eq!(removed(&shepherd).await, Ok(None));
        assert!(shepherd.sheep(NAME).is_some());
        assert_eq!(shepherd.writes(), []);
    }

    #[tokio::test]
    async fn a_left_over_with_its_own_kelpie_home_or_home_is_refused_and_kept() {
        for key in ["KELPIE_HOME", "HOME"] {
            let mut shepherd = FakeShepherd::new().await;
            let mut left_over = sheep(NAME, &["dog"]);
            left_over.env.insert(key.into(), "/elsewhere".into());
            shepherd.holds(left_over, true);
            let err = removed(&shepherd).await.unwrap_err();
            assert!(err.contains(&format!("sets {key},")), "{err}");
            assert!(shepherd.sheep(NAME).is_some());
            assert_eq!(shepherd.writes(), []);
        }
    }
}
