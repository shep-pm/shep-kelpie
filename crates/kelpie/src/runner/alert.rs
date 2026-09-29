//! Posting each ruling to the maintainer's relay and webhook, and each notice
//! to the webhook
//!
//! The relay is a faster, nicer path when it is reachable, but it is a
//! stopgap over an undocumented protocol, so it is never what keeps a
//! ruling from being lost: every ruling also posts to the webhook, saved
//! before it is posted so a failed post loses nothing. A failed post is
//! tried again, waiting longer after each failure, and the ruling counts
//! as alerted only once the webhook post lands, whatever the relay's send
//! did. A save that fails after a post lands leaves it to be posted again.
//! A notice of an automatic merge takes the same path, webhook only, once
//! no ruling is waiting to be posted.

use super::Runner;
use super::gate::short;
use super::report::StepReport;
use crate::ports::{Alert, AlertError, Timestamp};
use crate::relay;
use crate::settings::Effort;
use crate::state::{Notice, StateError};
use crate::webhook::Webhook;

// A failed post waits a minute, then twice as long after each failure, up
// to half an hour: a webhook that is down is never flooded, and a ruling
// reaches the maintainer within half an hour of it coming back.
const RETRY_FIRST: u64 = 60;
const RETRY_MAX: u64 = 30 * 60;

// The relay is cleared once a day, so its context never grows without
// bound: see docs/design-log.md.
const CLEAR_EVERY: u64 = 24 * 60 * 60;

/// What a post carries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Posting {
    /// The ruling with this id
    Ruling(u64),
    /// The notice of this pull request's automatic merge
    Notice(u64),
}

/// The last failed post, and when it may be tried again
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Retry {
    of: Posting,
    failures: u32,
    at: Timestamp,
}

/// What the relay is sent, best-effort, alongside a ruling's webhook post
#[derive(Debug)]
pub(super) struct RelayMessage {
    pub(super) text: String,
    /// Passed to the relay's `--model`, only spent if it needs starting
    pub(super) model: String,
    /// Passed to the relay's `--effort`, only spent if it needs starting
    pub(super) effort: Effort,
}

/// A ruling or notice to post, and where to post it
#[derive(Debug)]
pub(super) struct Due {
    pub(super) of: Posting,
    /// None for a notice, which needs no answer
    pub(super) relay: Option<RelayMessage>,
    pub(super) webhook: Webhook,
    pub(super) alert: Alert,
}

impl Runner {
    /// The oldest ruling not yet posted, or else the oldest notice, unless
    /// its last failure says wait
    pub(super) fn alert_due(&self) -> Option<Due> {
        let now = self.ports.clock.now();
        let waiting = |of| self.retry.is_some_and(|r| r.of == of && now < r.at);
        let project = self.project.as_str();
        if let Some(ruling) = self.state.rulings.iter().find(|r| !r.alerted) {
            let of = Posting::Ruling(ruling.id);
            return (!waiting(of)).then(|| Due {
                of,
                relay: Some(RelayMessage {
                    text: relay::message(
                        project,
                        ruling.id,
                        relay::Wants::of(&ruling.kind),
                        &ruling.question,
                    ),
                    model: self.settings.models.relay.model.as_str().to_owned(),
                    effort: self.settings.models.relay.effort,
                }),
                webhook: self.webhook.clone(),
                alert: Alert {
                    title: format!("kelpie: {project} ruling {}", ruling.id),
                    text: ruling.question.clone(),
                },
            });
        }
        let notice = self.state.notices.first()?;
        let of = Posting::Notice(notice.pull_request);
        (!waiting(of)).then(|| Due {
            of,
            relay: None,
            webhook: self.webhook.clone(),
            alert: notice_alert(project, notice),
        })
    }

    /// Whether the relay is due a daily clear, which is recorded as done
    /// once this returns true: called only when a ruling is about to be
    /// sent, since an idle relay never grows and needs no clearing.
    pub(super) fn relay_clear_due(&mut self) -> bool {
        let now = self.ports.clock.now();
        let due = self
            .relay_cleared
            .is_none_or(|last| now.0.saturating_sub(last.0) >= CLEAR_EVERY);
        if due {
            self.relay_cleared = Some(now);
        }
        due
    }

    /// Records how the post of `of` went
    pub(super) fn alert_sent(
        &mut self,
        of: Posting,
        sent: Result<(), AlertError>,
    ) -> Result<StepReport, StateError> {
        if let Err(e) = sent {
            let failures = match self.retry {
                Some(r) if r.of == of => r.failures.saturating_add(1),
                _ => 1,
            };
            let wait = RETRY_FIRST
                .saturating_mul(1 << (failures - 1).min(16))
                .min(RETRY_MAX);
            let at = Timestamp(self.ports.clock.now().0.saturating_add(wait));
            self.retry = Some(Retry { of, failures, at });
            let reason = e.to_string();
            return Ok(match of {
                Posting::Ruling(id) => StepReport::AlertFailed {
                    id,
                    reason,
                    retry_at: at,
                },
                Posting::Notice(pull_request) => StepReport::NoticeFailed {
                    pull_request,
                    reason,
                    retry_at: at,
                },
            });
        }
        self.retry = None;
        let mut next = self.state.clone();
        let report = match of {
            // An answer can land while the post is out, and takes the ruling with it.
            Posting::Ruling(id) => {
                if let Some(ruling) = next.rulings.iter_mut().find(|r| r.id == id) {
                    ruling.alerted = true;
                }
                StepReport::Alerted { id }
            }
            Posting::Notice(pull_request) => {
                next.notices.retain(|n| n.pull_request != pull_request);
                StepReport::Noticed { pull_request }
            }
        };
        self.save(next)?;
        Ok(report)
    }
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
            "Kelpie merged pull request #{pull_request} for issue #{issue} into main at {} \
             on {project}, every gate passed. Nothing to answer.",
            short(head)
        ),
    }
}

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
                 `shep trigger shep rule '1 yes'` merges it, and \
                 `shep trigger shep rule '1 no <note>'` sends the worker your note."
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
        let rig = Rig::new("hazels-lab");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
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

    #[test]
    fn a_ruling_reaches_the_relay_alongside_the_webhook() {
        let (rig, runner, head) = Rig::parked("shep");
        rig.relay.set_up(true);
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        let [(sent, model, effort)] = rig.relay.sent().try_into().unwrap();
        assert!(
            sent.starts_with("[kelpie]\nproject=shep ruling=1 wants=yes-or-no\n\n"),
            "{sent}"
        );
        assert!(sent.contains(&head[..7]), "{sent}");
        assert_eq!(
            (model.as_str(), effort),
            ("claude-haiku-4-5-20251001", Effort::Low)
        );
        assert_eq!(rig.alerts.posts().len(), 1, "the webhook still posts");
    }

    #[test]
    fn a_relay_that_cannot_be_reached_still_reaches_the_webhook_and_it_appears_in_status() {
        let (rig, runner, _) = Rig::parked("koji");
        // The rig's relay starts down, as if the relay were stopped.
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        assert_eq!(rig.relay.sent(), []);
        assert_eq!(rig.alerts.posts().len(), 1);
        assert_eq!(
            rig.ask(&runner, "status", None)["rulings"][0]["alerted"],
            true
        );
    }

    #[test]
    fn the_relay_is_cleared_once_a_day_and_not_sooner() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.relay.set_up(true);
        step(&runner).unwrap();
        assert_eq!(rig.relay.clears(), 1, "the first alert clears it");

        rig.ask(&runner, "rule", Some("1 no not yet"));
        rig.claude.script([
            Scripted::Push("again.txt", "again\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the noted turn: pushes, enters round 1
        let head = rig.forge.head_of("kelpie/7").unwrap();
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        rig.clock.advance(3600);
        step(&runner).unwrap();
        assert_eq!(
            rig.relay.clears(),
            1,
            "less than a day since the last clear"
        );

        rig.clock.advance(Rig::DAY);
        rig.ask(&runner, "rule", Some("2 no still not yet"));
        rig.claude.script([
            Scripted::Push("third.txt", "third\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the noted turn: pushes, enters round 1
        let head = rig.forge.head_of("kelpie/7").unwrap();
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        rig.forge.set_checks(&head, Checks::Passed);
        rig.verdict(&runner);
        step(&runner).unwrap();
        assert_eq!(rig.relay.clears(), 2, "a full day passed");
    }
}
