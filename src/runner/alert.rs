//! Posting each ruling and each notice to the maintainer's webhook
//!
//! A ruling is alerted once its webhook post lands. With no webhook set in
//! kelpie's settings nothing is posted and no ruling waits on it: a ruling
//! reaches the maintainer through the log, `status` and `shep kelpie rule`
//! alone. A ruling is saved before it is posted so a failed post loses
//! nothing, and a failed post is tried again, waiting longer after each
//! failure. A save that fails after a post lands leaves it to be posted
//! again. A notice of an automatic merge takes the same path once no ruling
//! is waiting to be posted, and with no webhook it is only logged.

use std::sync::Mutex;

use super::Runner;
use super::gate::short;
use super::report::StepReport;
use super::trigger::lock;
use crate::ports::{Alert, AlertError, Alerts, ReplyWith, Timestamp};
use crate::state::{Notice, StateError};
use crate::webhook::Webhook;

// A failed post waits a minute, then twice as long after each failure, up
// to half an hour: a webhook that is down is never flooded, and a ruling
// reaches the maintainer within half an hour of it coming back.
const RETRY_FIRST: u64 = 60;
const RETRY_MAX: u64 = 30 * 60;

/// What a post carries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Posting {
    /// The ruling with this id
    Ruling(u64),
    /// The notice of an automatic merge
    Notice {
        /// The work item's issue
        issue: u64,
        /// The pull request merged
        pull_request: u64,
    },
}

/// The last failed post, and when it may be tried again
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Retry {
    of: Posting,
    failures: u32,
    at: Timestamp,
}

/// A ruling or notice to post, and where to post it
#[derive(Debug)]
pub(super) struct Due {
    pub(super) of: Posting,
    /// None where the webhook is off
    pub(super) webhook: Option<Webhook>,
    pub(super) alert: Alert,
}

/// Posts the oldest ruling or notice due to the webhook
///
/// Returns `None` when nothing is due.
pub(super) fn post_due(
    runner: &Mutex<Runner>,
    alerts: &dyn Alerts,
) -> Option<Result<StepReport, StateError>> {
    let due = lock(runner).alert_due()?;
    let sent = match &due.webhook {
        Some(webhook) => alerts.post(webhook, &due.alert),
        None => Ok(()),
    };
    Some(lock(runner).alert_sent(due.of, sent))
}

impl Runner {
    /// The oldest ruling not yet posted, or else the oldest notice, unless
    /// its last failure says wait
    ///
    /// With no webhook no ruling is due, and a notice is due to be logged.
    pub(super) fn alert_due(&mut self) -> Option<Due> {
        let now = self.ports.clock.now();
        let waiting = |retry: Option<Retry>, of| retry.is_some_and(|r| r.of == of && now < r.at);
        if self.webhook.is_some()
            && let Some(ruling) = self.state.rulings.iter().find(|r| !r.alerted)
        {
            let (id, kind) = (ruling.id, ruling.kind.clone());
            if waiting(self.retry, Posting::Ruling(id)) {
                return None;
            }
            let reply = self.reply_with(id, &kind);
            return Some(self.ruling_due(id, reply));
        }
        let notice = self.state.notices.first()?;
        let of = Posting::Notice {
            issue: notice.issue,
            pull_request: notice.pull_request,
        };
        let alert = notice_alert(self.project.as_str(), notice);
        (!waiting(self.retry, of)).then(|| Due {
            of,
            webhook: self.webhook.clone(),
            alert,
        })
    }

    // Ruling `id`'s post, which must be pending
    fn ruling_due(&self, id: u64, reply: Option<ReplyWith>) -> Due {
        let project = self.project.as_str();
        let ruling = self.state.rulings.iter().find(|r| r.id == id);
        let ruling = ruling.expect("the ruling due is pending");
        Due {
            of: Posting::Ruling(id),
            webhook: self.webhook.clone(),
            alert: Alert {
                title: format!("kelpie: {project} ruling {id}"),
                text: ruling.question.clone(),
                reply,
            },
        }
    }

    /// Records how the post of `of` went
    pub(super) fn alert_sent(
        &mut self,
        of: Posting,
        sent: Result<(), AlertError>,
    ) -> Result<StepReport, StateError> {
        let mut next = self.state.clone();
        if let Err(e) = sent {
            let failures = match self.retry {
                Some(r) if r.of == of => r.failures.saturating_add(1),
                _ => 1,
            };
            let wait = backoff(RETRY_FIRST, failures, RETRY_MAX);
            let at = Timestamp(self.ports.clock.now().0.saturating_add(wait));
            self.retry = Some(Retry { of, failures, at });
            let reason = e.to_string();
            return Ok(match of {
                Posting::Ruling(id) => StepReport::AlertFailed {
                    id,
                    reason,
                    retry_at: at,
                },
                Posting::Notice {
                    issue,
                    pull_request,
                } => StepReport::NoticeFailed {
                    issue,
                    pull_request,
                    reason,
                    retry_at: at,
                },
            });
        }
        self.retry = None;
        let report = match of {
            Posting::Ruling(id) => {
                if let Some(ruling) = next.rulings.iter_mut().find(|r| r.id == id) {
                    ruling.alerted = true;
                }
                // A reply may come even if the ruling is settled first.
                self.read_replies_from(self.ports.clock.now());
                StepReport::Alerted { id }
            }
            Posting::Notice {
                issue,
                pull_request,
            } => {
                next.notices.retain(|n| n.pull_request != pull_request);
                StepReport::Noticed {
                    issue,
                    pull_request,
                }
            }
        };
        self.save(next)?;
        Ok(report)
    }
}

/// Seconds to wait after the `failures`th failure in a row: `first`, then
/// twice as long each time, up to `max`
pub(super) fn backoff(first: u64, failures: u32, max: u64) -> u64 {
    first
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(max)
}

fn notice_alert(project: &str, notice: &Notice) -> Alert {
    let Notice {
        issue,
        pull_request,
        head,
    } = notice;
    Alert {
        title: format!("kelpie: {project} merged #{pull_request}"),
        text: format!(
            "Pull request #{pull_request} for issue #{issue} merged into main at {} \
             on {project}, every gate passed. Nothing to answer.",
            short(head)
        ),
        reply: None,
    }
}

#[cfg(test)]
mod no_webhook;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Clock};
    use crate::runner::step;
    use crate::test::{Rig, Scripted};

    #[test]
    fn each_new_ruling_posts_once_with_its_id_question_and_trigger() {
        let (rig, runner, head) = Rig::parked("shep");
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        let [(webhook, alert)] = rig.alerts.posts().try_into().unwrap();
        assert_eq!(webhook, rig.webhook());
        assert_eq!(alert.title, "kelpie: shep ruling 1");
        let short = &head[..7];
        assert_eq!(
            alert.text,
            format!(
                "Merge pull request #71 at {short} into main? \
                 `shep kelpie rule 1 yes` merges it, and \
                 `shep kelpie rule 1 no <note>` sends the worker your note."
            )
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["alerted"],
            true
        );

        for _ in 0..3 {
            rig.clock.advance(3600);
            assert_eq!(step(&runner).unwrap(), None);
        }
        drop(runner);
        let runner = rig.open().unwrap();
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "a restart posts nothing again"
        );
        assert_eq!(rig.alerts.posts().len(), 1);
    }

    #[test]
    fn a_failed_post_is_logged_and_retried_and_never_loses_the_ruling() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.alerts.set_down(true);
        let failed = |at| StepReport::AlertFailed {
            id: 1,
            reason: "the webhook answered HTTP 503".into(),
            retry_at: Timestamp(Rig::EPOCH + at),
        };
        let start = rig.clock.now().0 - Rig::EPOCH;
        assert_eq!(step(&runner).unwrap(), Some(failed(start + 60)));
        assert_eq!(step(&runner).unwrap(), None, "not before its retry");
        rig.clock.advance(60);
        assert_eq!(step(&runner).unwrap(), Some(failed(start + 60 + 120)));
        rig.clock.advance(120);
        assert_eq!(step(&runner).unwrap(), Some(failed(start + 180 + 240)));

        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["rulings"][0]["id"], 1);
        assert_eq!(status["rulings"][0]["alerted"], false);
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );

        rig.alerts.set_down(false);
        rig.clock.advance(240);
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        assert_eq!(rig.alerts.posts().len(), 4, "three failures and one post");
        rig.clock.advance(3600);
        assert_eq!(step(&runner).unwrap(), None);
    }

    #[test]
    fn a_long_outage_waits_half_an_hour_between_tries() {
        let (rig, runner, _) = Rig::parked("golbat");
        rig.alerts.set_down(true);
        let mut waits = Vec::new();
        for _ in 0..8 {
            let now = rig.clock.now().0;
            let Some(StepReport::AlertFailed { retry_at, .. }) = step(&runner).unwrap() else {
                panic!("the post was not tried");
            };
            waits.push(retry_at.0 - now);
            rig.clock.advance(retry_at.0 - now);
        }
        assert_eq!(waits, [60, 120, 240, 480, 960, 1800, 1800, 1800]);
    }

    #[test]
    fn the_webhook_url_reaches_no_log_line_status_or_comment() {
        let rig = Rig::new("webapp");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.alerts.set_down(true);
        rig.claude.script([
            Scripted::Say("<kelpie-question>Which name?</kelpie-question>"),
            Scripted::Push("work.txt", "work\n"),
            Scripted::Text("CLEAN"),
        ]);
        let mut seen = Vec::new();
        let mut log = |report: Option<StepReport>| {
            seen.push(serde_json::to_string(&report).unwrap());
        };
        log(step(&runner).unwrap()); // asked
        log(step(&runner).unwrap()); // alert-failed: the webhook is down
        rig.alerts.set_down(false);
        rig.clock.advance(60);
        log(step(&runner).unwrap()); // alerted: the question's ruling, retried
        rig.ask(&runner, "rule", Some("1 answer --dry-run"));
        log(step(&runner).unwrap()); // ended: pushes work.txt, enters round 1
        log(step(&runner).unwrap()); // review round 1, qwen: clean by default
        log(step(&runner).unwrap()); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        log(rig.verdict(&runner)); // the merge ruling
        log(step(&runner).unwrap()); // alerted: the merge ruling

        assert!(seen[1].contains("alert-failed"), "{seen:?}");
        assert!(seen[7].contains("alerted"), "{seen:?}");
        seen.push(rig.ask(&runner, "status", None).to_string());
        seen.push(format!("{:?}", runner.lock().unwrap()));
        seen.extend(rig.forge.comments().into_iter().map(|(_, c)| c));
        seen.extend(
            rig.alerts
                .posts()
                .into_iter()
                .map(|(w, a)| format!("{w:?} {a:?}")),
        );
        for line in &seen {
            assert!(!line.contains(Rig::WEBHOOK_SECRET), "{line}");
        }
    }

    #[test]
    fn a_ruling_answered_while_its_post_is_out_is_not_posted_again() {
        let (rig, runner, _) = Rig::parked("reactmap");
        let due = runner.lock().unwrap().alert_due().unwrap();
        rig.ask(&runner, "rule", Some("1 no not yet"));
        let report = runner.lock().unwrap().alert_sent(due.of, Ok(())).unwrap();
        assert_eq!(report, StepReport::Alerted { id: 1 });
        assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
        assert!(runner.lock().unwrap().alert_due().is_none());
    }

    #[test]
    fn a_restart_tries_a_failed_post_at_once() {
        let (rig, runner, _) = Rig::parked("koji");
        rig.alerts.set_down(true);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::AlertFailed { .. })
        ));
        drop(runner);
        rig.alerts.set_down(false);
        let runner = rig.open().unwrap();
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    }
}
