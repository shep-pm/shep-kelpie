//! `shep kelpie notifications clear`: marks every notification a repo has in
//! the maintainer's inbox done
//!
//! A call GitHub rate-limits is waited out and asked again, so one run clears
//! the whole repo. Five limits in a row, or any other failure, stop the loop,
//! and running it again picks up what is left.

use crate::ports::rate_limit::FALLBACK;
use crate::ports::{Clock, ForgeError, Notifications};
use crate::settings::ForgeSlug;

/// How many limits in a row, with no call succeeding between them, stop the loop
const LIMITS_IN_A_ROW: u32 = 5;

/// How many notifications go by between lines saying how far the run is
const SAY_EVERY: usize = 100;

/// Marks each notification on `repo` done, in the order GitHub lists them, and
/// returns the line to print
///
/// The count, each [`SAY_EVERY`] cleared and each wait are said through `say`
/// as they happen, so a long run does not look hung.
///
/// # Errors
///
/// A message when the listing fails, which marks nothing, or when a call
/// fails, which stops there and says how many were cleared of how many.
pub fn clear(
    notifications: &dyn Notifications,
    clock: &dyn Clock,
    say: &dyn Fn(&str),
    repo: &ForgeSlug,
) -> Result<Vec<String>, String> {
    let mut limits = Limits {
        clock,
        say,
        in_a_row: 0,
    };
    let all = limits
        .ask(None, || notifications.list(repo))
        .map_err(|e| e.to_string())?;
    let repo = repo.as_str();
    if all.is_empty() {
        return Ok(vec![format!("no notifications on {repo}")]);
    }
    let total = all.len();
    say(&format!("clearing {total} notifications on {repo}"));
    for (done, notification) in all.iter().enumerate() {
        limits
            .ask(Some((done, total)), || {
                notifications.mark_done(notification)
            })
            .map_err(|e| {
                format!(
                    "cleared {done} of {total} notifications on {repo} before GitHub refused: {e}"
                )
            })?;
        let cleared = done + 1;
        if cleared % SAY_EVERY == 0 && cleared < total {
            say(&format!("cleared {cleared} of {total}"));
        }
    }
    Ok(vec![format!("cleared {total} notifications on {repo}")])
}

// The waits a run has made on GitHub's limits.
struct Limits<'a> {
    clock: &'a dyn Clock,
    say: &'a dyn Fn(&str),
    in_a_row: u32,
}

impl Limits<'_> {
    // Asks `call` until it answers other than with a limit, waiting what each
    // limit says. `progress` is how many are cleared of how many, once there
    // is a list.
    fn ask<T>(
        &mut self,
        progress: Option<(usize, usize)>,
        mut call: impl FnMut() -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        loop {
            match call() {
                Err(ForgeError::Limited { said, cooldown }) => {
                    self.in_a_row += 1;
                    if self.in_a_row >= LIMITS_IN_A_ROW {
                        return Err(ForgeError::Limited { said, cooldown });
                    }
                    let now = self.clock.now();
                    let seconds = cooldown.until(now).map_or(FALLBACK, |at| at.0 - now.0);
                    let cleared = match progress {
                        Some((done, total)) => format!("{done} of {total} cleared"),
                        None => "none cleared yet".to_owned(),
                    };
                    (self.say)(&format!("GitHub's limit: waiting {seconds}s, {cleared}"));
                    self.clock.wait(seconds);
                }
                answered => {
                    if answered.is_ok() {
                        self.in_a_row = 0;
                    }
                    return answered;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::ports::{Cooldown, Notification, Timestamp};
    use crate::test::FakeClock;

    const START: u64 = 1_000;

    // The inbox as GitHub holds it: the ids listed, and the calls that fail.
    struct Inbox {
        listed: Vec<&'static str>,
        // The answers the next listings give before one succeeds.
        list_fails: Mutex<Vec<ForgeError>>,
        // Each id whose mark done fails, with the answer and how many times.
        fails: Mutex<Vec<(&'static str, ForgeError, usize)>>,
        done: Mutex<Vec<String>>,
    }

    impl Inbox {
        fn of(ids: &[&'static str]) -> Self {
            Self {
                listed: ids.to_vec(),
                list_fails: Mutex::default(),
                fails: Mutex::default(),
                done: Mutex::default(),
            }
        }

        fn failing(self, id: &'static str, error: ForgeError, times: usize) -> Self {
            self.fails.lock().unwrap().push((id, error, times));
            self
        }

        fn done(&self) -> Vec<String> {
            self.done.lock().unwrap().clone()
        }
    }

    impl Notifications for Inbox {
        fn list(&self, _: &ForgeSlug) -> Result<Vec<Notification>, ForgeError> {
            if let Some(error) = self.list_fails.lock().unwrap().pop() {
                return Err(error);
            }
            Ok(self
                .listed
                .iter()
                .map(|id| Notification { id: (*id).into() })
                .collect())
        }

        fn mark_done(&self, notification: &Notification) -> Result<(), ForgeError> {
            for (id, error, times) in self.fails.lock().unwrap().iter_mut() {
                if *id == notification.id && *times > 0 {
                    *times -= 1;
                    return Err(error.clone());
                }
            }
            self.done.lock().unwrap().push(notification.id.clone());
            Ok(())
        }
    }

    fn repo() -> ForgeSlug {
        ForgeSlug::try_from("shep-pm/koji".to_owned()).unwrap()
    }

    fn limited(retry_after: Option<u64>) -> ForgeError {
        ForgeError::Limited {
            said: "gh failed: You have exceeded a secondary rate limit".into(),
            cooldown: Cooldown {
                retry_after,
                reset: None,
            },
        }
    }

    // Runs `clear` on a clock at `START`, returning its answer, the lines said
    // as it ran and the seconds the clock moved.
    fn run(inbox: &Inbox) -> (Result<Vec<String>, String>, Vec<String>, u64) {
        let clock = FakeClock::at(START);
        let said = Mutex::new(Vec::new());
        let answer = clear(
            inbox,
            &clock,
            &|line| said.lock().unwrap().push(line.to_owned()),
            &repo(),
        );
        let waited = clock.now().0 - START;
        (answer, said.into_inner().unwrap(), waited)
    }

    #[test]
    fn every_notification_is_marked_done() {
        let inbox = Inbox::of(&["11", "12", "13"]);
        let (answer, said, waited) = run(&inbox);
        assert_eq!(answer.unwrap(), ["cleared 3 notifications on shep-pm/koji"]);
        assert_eq!(inbox.done(), ["11", "12", "13"]);
        assert_eq!(said, ["clearing 3 notifications on shep-pm/koji"]);
        assert_eq!(waited, 0);
    }

    #[test]
    fn a_long_run_says_how_far_it_is_every_hundred() {
        let inbox = Inbox::of(&["11"; 250]);
        let (answer, said, _) = run(&inbox);
        assert_eq!(
            answer.unwrap(),
            ["cleared 250 notifications on shep-pm/koji"]
        );
        assert_eq!(
            said,
            [
                "clearing 250 notifications on shep-pm/koji",
                "cleared 100 of 250",
                "cleared 200 of 250",
            ]
        );
    }

    #[test]
    fn the_last_notification_leaves_its_line_to_the_answer() {
        let inbox = Inbox::of(&["11"; 100]);
        let (_, said, _) = run(&inbox);
        assert_eq!(said, ["clearing 100 notifications on shep-pm/koji"]);
    }

    #[test]
    fn an_empty_inbox_says_so() {
        let inbox = Inbox::of(&[]);
        let (answer, said, _) = run(&inbox);
        assert_eq!(answer.unwrap(), ["no notifications on shep-pm/koji"]);
        assert!(inbox.done().is_empty());
        assert!(said.is_empty(), "there is no count to say");
    }

    #[test]
    fn a_failed_call_stops_and_counts_what_was_cleared() {
        let failed = ForgeError::Failed("gh: Not Found (HTTP 404)".into());
        let inbox = Inbox::of(&["11", "12", "13"]).failing("12", failed, usize::MAX);
        let (answer, _, waited) = run(&inbox);
        assert_eq!(
            answer.unwrap_err(),
            "cleared 1 of 3 notifications on shep-pm/koji before GitHub refused: \
             gh failed: gh: Not Found (HTTP 404)"
        );
        assert_eq!(inbox.done(), ["11"], "it stopped at the failure");
        assert_eq!(waited, 0, "a failure that is no limit is not waited on");
    }

    #[test]
    fn a_limited_call_waits_what_it_said_then_carries_on() {
        let inbox = Inbox::of(&["11", "12", "13"]).failing("12", limited(Some(30)), 1);
        let (answer, said, waited) = run(&inbox);
        assert_eq!(answer.unwrap(), ["cleared 3 notifications on shep-pm/koji"]);
        assert_eq!(inbox.done(), ["11", "12", "13"]);
        assert_eq!(waited, 30);
        assert_eq!(
            said,
            [
                "clearing 3 notifications on shep-pm/koji",
                "GitHub's limit: waiting 30s, 1 of 3 cleared",
            ]
        );
    }

    #[test]
    fn a_limit_naming_a_reset_waits_until_then() {
        let reset = ForgeError::Limited {
            said: "gh failed: API rate limit exceeded".into(),
            cooldown: Cooldown {
                retry_after: None,
                reset: Some(Timestamp(START + 90)),
            },
        };
        let inbox = Inbox::of(&["11"]).failing("11", reset, 1);
        let (answer, _, waited) = run(&inbox);
        assert!(answer.is_ok());
        assert_eq!(waited, 90);
    }

    #[test]
    fn a_limit_naming_no_time_waits_the_fallback() {
        let inbox = Inbox::of(&["11", "12"]).failing("11", limited(None), 1);
        let (answer, said, waited) = run(&inbox);
        assert_eq!(answer.unwrap(), ["cleared 2 notifications on shep-pm/koji"]);
        assert_eq!(waited, FALLBACK);
        assert_eq!(
            said,
            [
                "clearing 2 notifications on shep-pm/koji".to_owned(),
                format!("GitHub's limit: waiting {FALLBACK}s, 0 of 2 cleared"),
            ]
        );
    }

    #[test]
    fn a_limited_listing_waits_and_lists_again() {
        let inbox = Inbox::of(&["11", "12"]);
        *inbox.list_fails.lock().unwrap() = vec![limited(Some(45))];
        let (answer, said, waited) = run(&inbox);
        assert_eq!(answer.unwrap(), ["cleared 2 notifications on shep-pm/koji"]);
        assert_eq!(inbox.done(), ["11", "12"]);
        assert_eq!(waited, 45);
        assert_eq!(
            said,
            [
                "GitHub's limit: waiting 45s, none cleared yet",
                "clearing 2 notifications on shep-pm/koji",
            ]
        );
    }

    #[test]
    fn five_limits_in_a_row_stop_and_count_what_was_cleared() {
        let inbox = Inbox::of(&["11", "12", "13"]).failing("12", limited(Some(10)), usize::MAX);
        let (answer, said, waited) = run(&inbox);
        let refused = answer.unwrap_err();
        assert_eq!(
            refused,
            "cleared 1 of 3 notifications on shep-pm/koji before GitHub refused: \
             gh failed: You have exceeded a secondary rate limit"
        );
        assert_eq!(inbox.done(), ["11"]);
        let waits = said.iter().filter(|line| line.contains("waiting"));
        assert_eq!(waits.count(), 4, "it waits after each of the first four");
        assert_eq!(waited, 40);
    }

    #[test]
    fn limits_with_a_call_succeeding_between_them_are_not_in_a_row() {
        // Four limits on each of two ids: eight in all, never five together.
        let inbox = Inbox::of(&["11", "12"])
            .failing("11", limited(Some(1)), 4)
            .failing("12", limited(Some(1)), 4);
        let (answer, _, waited) = run(&inbox);
        assert_eq!(answer.unwrap(), ["cleared 2 notifications on shep-pm/koji"]);
        assert_eq!(waited, 8);
    }

    #[test]
    fn a_failed_listing_marks_nothing_done() {
        let inbox = Inbox::of(&["11"]);
        *inbox.list_fails.lock().unwrap() =
            vec![ForgeError::Failed("gh: Bad credentials (HTTP 401)".into())];
        let (answer, _, _) = run(&inbox);
        assert_eq!(
            answer.unwrap_err(),
            "gh failed: gh: Bad credentials (HTTP 401)"
        );
        assert!(inbox.done().is_empty());
    }
}
