//! `shep kelpie notifications clear`: marks every notification a repo has in
//! the maintainer's inbox done
//!
//! There is no retry and no pacing: a call that fails stops the loop, and
//! running it again picks up what is left.

use crate::ports::Notifications;
use crate::settings::ForgeSlug;

/// Marks each notification on `repo` done, in the order GitHub lists them, and
/// returns the line to print
///
/// # Errors
///
/// A message when the listing fails, which marks nothing, or when a call
/// fails, which stops there and says how many were cleared of how many.
pub fn clear(notifications: &dyn Notifications, repo: &ForgeSlug) -> Result<Vec<String>, String> {
    let all = notifications.list(repo).map_err(|e| e.to_string())?;
    let repo = repo.as_str();
    if all.is_empty() {
        return Ok(vec![format!("no notifications on {repo}")]);
    }
    for (done, notification) in all.iter().enumerate() {
        notifications.mark_done(notification).map_err(|e| {
            let total = all.len();
            format!("cleared {done} of {total} notifications on {repo} before GitHub refused: {e}")
        })?;
    }
    Ok(vec![format!(
        "cleared {} notifications on {repo}",
        all.len()
    )])
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::ports::{Cooldown, ForgeError, Notification};

    // The inbox as GitHub holds it: the ids listed, and the call that fails.
    struct Inbox {
        listed: Result<Vec<&'static str>, ForgeError>,
        fails_on: Option<(&'static str, ForgeError)>,
        done: Mutex<Vec<String>>,
    }

    impl Inbox {
        fn of(ids: &[&'static str]) -> Self {
            Self {
                listed: Ok(ids.to_vec()),
                fails_on: None,
                done: Mutex::default(),
            }
        }

        fn done(&self) -> Vec<String> {
            self.done.lock().unwrap().clone()
        }
    }

    impl Notifications for Inbox {
        fn list(&self, _: &ForgeSlug) -> Result<Vec<Notification>, ForgeError> {
            let ids = self.listed.clone()?;
            Ok(ids
                .into_iter()
                .map(|id| Notification { id: id.into() })
                .collect())
        }

        fn mark_done(&self, notification: &Notification) -> Result<(), ForgeError> {
            if let Some((id, error)) = &self.fails_on
                && *id == notification.id
            {
                return Err(error.clone());
            }
            self.done.lock().unwrap().push(notification.id.clone());
            Ok(())
        }
    }

    fn repo() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
    }

    #[test]
    fn every_notification_is_marked_done() {
        let inbox = Inbox::of(&["11", "12", "13"]);
        let said = clear(&inbox, &repo()).unwrap();
        assert_eq!(said, ["cleared 3 notifications on shep-pm/koji"]);
        assert_eq!(inbox.done(), ["11", "12", "13"]);
    }

    #[test]
    fn an_empty_inbox_says_so() {
        let inbox = Inbox::of(&[]);
        let said = clear(&inbox, &repo()).unwrap();
        assert_eq!(said, ["no notifications on shep-pm/koji"]);
        assert!(inbox.done().is_empty());
    }

    #[test]
    fn a_failed_call_stops_and_counts_what_was_cleared() {
        let limited = ForgeError::Limited {
            said: "gh failed: You have exceeded a secondary rate limit".into(),
            cooldown: Cooldown::default(),
        };
        let failed = ForgeError::Failed("gh: Not Found (HTTP 404)".into());
        for (error, said) in [
            (
                limited,
                "gh failed: You have exceeded a secondary rate limit",
            ),
            (failed, "gh failed: gh: Not Found (HTTP 404)"),
        ] {
            let mut inbox = Inbox::of(&["11", "12", "13"]);
            inbox.fails_on = Some(("12", error));
            let refused = clear(&inbox, &repo()).unwrap_err();
            assert_eq!(
                refused,
                format!(
                    "cleared 1 of 3 notifications on shep-pm/koji before GitHub refused: {said}"
                )
            );
            assert_eq!(inbox.done(), ["11"], "it stopped at the failure");
        }
    }

    #[test]
    fn a_failed_listing_marks_nothing_done() {
        let mut inbox = Inbox::of(&["11"]);
        inbox.listed = Err(ForgeError::Failed("gh: Bad credentials (HTTP 401)".into()));
        let refused = clear(&inbox, &repo()).unwrap_err();
        assert_eq!(refused, "gh failed: gh: Bad credentials (HTTP 401)");
        assert!(inbox.done().is_empty());
    }
}
