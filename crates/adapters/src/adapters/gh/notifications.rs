//! The login's notifications, from `gh api`
//!
//! They belong to a user, so these calls run as the maintainer's login and
//! never as the App.

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::{ForgeError, Notification};
use crate::settings::ForgeSlug;

/// Every notification on `repo`, the read ones too, which stay in the inbox
/// until they are done
pub(super) fn list(repo: &ForgeSlug) -> Result<Vec<Notification>, ForgeError> {
    let path = format!("repos/{}/notifications?all=true&per_page=50", repo.as_str());
    parse_listing(&gh(&["api", "--paginate", &path])?)
}

/// Marks thread `notification` done, as `DELETE` on it does in the web inbox
pub(super) fn mark_done(notification: &Notification) -> Result<(), ForgeError> {
    let path = format!("notifications/threads/{}", notification.id);
    gh(&["api", "--method", "DELETE", &path]).map(drop)
}

// `--paginate` prints one array holding every page, and older `gh` one array
// for each page, one after another. Reading a stream of arrays takes both.
fn parse_listing(stdout: &[u8]) -> Result<Vec<Notification>, ForgeError> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
    }
    let pages = serde_json::Deserializer::from_slice(stdout).into_iter::<Vec<Row>>();
    let mut found = Vec::new();
    for page in pages {
        let page = page.map_err(|_| unreadable(stdout))?;
        found.extend(page.into_iter().map(|row| Notification { id: row.id }));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Three rows of `gh api --paginate` on shep-pm/shep-kelpie's notifications.
    const RECORDED: &str = include_str!("../../../../../fixtures/gh-api-notifications.json");

    #[test]
    fn a_recorded_listing_reads_every_id() {
        let ids: Vec<String> = parse_listing(RECORDED.as_bytes())
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(ids, ["26985669461", "26902698638", "26967646426"]);
    }

    #[test]
    fn pages_printed_one_after_another_read_as_one_listing() {
        let listing = parse_listing(br#"[{"id":"1"}][{"id":"2"}]"#).unwrap();
        let ids: Vec<&str> = listing.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["1", "2"]);
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_unreadable() {
        for wrong in [r#"{"message":"Not Found"}"#, r#"[{"id":19}]"#, "<html>"] {
            assert!(
                matches!(
                    parse_listing(wrong.as_bytes()),
                    Err(ForgeError::Unreadable(_))
                ),
                "{wrong}"
            );
        }
    }
}
