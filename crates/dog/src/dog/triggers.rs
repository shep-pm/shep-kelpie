//! The maintainer's triggers on the dog: `status`, `take` and `return`

use std::sync::LazyLock;

use serde_json::{Value, json};

use super::availability::Availability;
use super::desk::{Delivery, Desk, deliveries};
use crate::lease::book::Asked;
use crate::lease::{Holder, LeaseKind};

/// What `status` shows beside the leases: the agent board, the records and the API credit
#[derive(Debug, Clone, Copy)]
pub struct Shown<'a> {
    /// Each implementer's typical item and wait
    pub board: &'a Value,
    /// Each agent's track record
    pub records: &'a Value,
    /// The credit spent and its cap, or null with none set
    pub api_credit: &'a Value,
}

static EMPTY: LazyLock<Value> = LazyLock::new(|| json!({}));

impl Default for Shown<'_> {
    fn default() -> Self {
        Self {
            board: &EMPTY,
            records: &EMPTY,
            api_credit: &Value::Null,
        }
    }
}

/// The triggers the dog answers for the maintainer
pub const ACTIONS: [&str; 3] = ["status", "take", "return"];

impl Desk {
    /// Shorthand for <code>[Desk::answer_with]</code> with nothing read of availability or the board
    #[cfg(test)]
    pub fn answer(&mut self, action: &str, params: Option<&str>) -> (String, Vec<Delivery>) {
        self.answer_with(action, params, &Availability::default(), Shown::default())
    }

    /// Answers one of the maintainer's triggers with a JSON body
    ///
    /// `status` lists every lease, `availability` and the `board`, `records` and `api_credit` given. `take`
    /// and `return` name a book lease and act for the maintainer. Asking
    /// with `take` again is how a waiting maintainer learns of the grant.
    pub fn answer_with(
        &mut self,
        action: &str,
        params: Option<&str>,
        availability: &Availability,
        shown: Shown<'_>,
    ) -> (String, Vec<Delivery>) {
        let answered = self.respond(action, params, availability, shown);
        self.look(true);
        answered
    }

    fn respond(
        &mut self,
        action: &str,
        params: Option<&str>,
        availability: &Availability,
        shown: Shown<'_>,
    ) -> (String, Vec<Delivery>) {
        let error = |message: String| (json!({ "error": message }).to_string(), Vec::new());
        let params = params.map(str::trim).filter(|p| !p.is_empty());
        let kind = match (action, params) {
            ("status", None) => {
                return (self.status(availability, shown).to_string(), Vec::new());
            }
            ("status", Some(_)) => return error("`status` takes no params".into()),
            ("take" | "return", Some(kind)) => match LeaseKind::try_from(kind) {
                Ok(kind) => kind,
                Err(e) => return error(e.to_string()),
            },
            ("take" | "return", None) => return error(format!("`{action}` takes a lease kind")),
            _ => return error(format!("unknown action `{action}`")),
        };
        if action == "return" {
            let held = self.book.holder(&kind) == Some(&Holder::Maintainer);
            let grants = self.book.give_back(&kind, &Holder::Maintainer);
            let body = json!({ "kind": kind, "returned": held });
            return (body.to_string(), deliveries(grants));
        }
        let body = match self.book.ask(&kind, Holder::Maintainer) {
            Asked::Granted | Asked::AlreadyHeld => json!({ "kind": kind, "granted": true }),
            Asked::Queued { ahead } => json!({ "kind": kind, "queued": ahead }),
        };
        (body.to_string(), Vec::new())
    }

    fn status(&self, availability: &Availability, shown: Shown<'_>) -> Value {
        let mut leases = vec![json!(self.tests.status())];
        leases.extend(self.e2e.values().map(|l| json!(l.status())));
        leases.extend(self.book.status().iter().map(|l| json!(l)));
        let mut status = json!({
            "leases": leases,
            "availability": availability.status(),
            "board": shown.board,
            "records": shown.records,
        });
        if !shown.api_credit.is_null() {
            status["api_credit"] = shown.api_credit.clone();
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ACTIONS, Shown};
    use crate::agent_board::DEFAULT_SECS;
    use crate::dog::availability::{Availability, Sources};
    use crate::dog::board::tests::{WINDOW, call, credit};
    use crate::dog::board::{Board, refresh};
    use crate::dog::desk::tests::{grant, stand_in, world};
    use crate::lease::Epoch;
    use crate::lease::counted::{Taker, Ticket};
    use crate::lease::wire::Asker;
    use crate::pacer::RECHECK_SECS;
    use crate::ports::Timestamp;
    use crate::test::EPOCH;

    #[test]
    fn the_maintainer_goes_ahead_of_queued_runners_without_preempting() {
        let mut w = world();
        let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
        w.raise("koji", koji.want(&stand_in()));
        w.raise("reactmap", reactmap.want(&stand_in()));
        let (body, out) = w.ask("take", Some("stand-in"));
        assert_eq!(
            (body, out),
            (json!({ "kind": "stand-in", "queued": 0 }), vec![])
        );
        assert_eq!(w.book_line()["holder"], json!({ "runner": "koji" }));

        assert_eq!(w.raise("koji", koji.give_back(&stand_in())), []);
        let (body, _) = w.ask("take", Some("stand-in"));
        assert_eq!(body, json!({ "kind": "stand-in", "granted": true }));
        let (body, out) = w.ask("return", Some("stand-in"));
        assert_eq!(body, json!({ "kind": "stand-in", "returned": true }));
        assert_eq!(out, [grant("reactmap", 202)]);
        let (body, _) = w.ask("return", Some("stand-in"));
        assert_eq!(body, json!({ "kind": "stand-in", "returned": false }));
    }

    #[test]
    fn status_lists_cargo_test_and_the_coderabbit_window() {
        let mut w = world();
        assert_eq!(
            w.ask("status", None).0,
            json!({ "leases": [{
                "kind": "cargo-test",
                "capacity": 3,
                "holders": [],
                "queue": [],
            }, {
                "kind": "coderabbit",
                "holder": null,
                "since": null,
                "queue": [],
                "window": { "quota": 1, "summons": [], "opens": null },
            }], "availability": { "models": [], "accounts": [] }, "board": {}, "records": {} })
        );
    }

    #[test]
    fn status_shows_each_account_and_model_the_dog_reads() {
        let mut w = world();
        let (_dir, agents) = crate::test::claude_implementers();
        let sources = Sources::of(&agents, &crate::settings::Gateways::default());
        let shown = Availability::new(&sources);
        let (body, _) = w.desk.answer_with("status", None, &shown, Shown::default());
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body["availability"],
            json!({
                "models": [],
                "accounts": [{ "account": "claude", "unknown": true, "read_at": null }],
            })
        );
    }

    #[test]
    fn status_shows_each_implementer_s_typical_item_and_its_wait() {
        let mut w = world();
        let (_dir, agents) = crate::test::claude_implementers();
        let sources = Sources::of(&agents, &crate::settings::Gateways::default());
        let shown = Availability::new(&sources);
        let board = Board::new(&agents).view().status(&shown, Timestamp(EPOCH));
        let records = json!({ "opus-high": { "fix": { "items": 1 } } });
        let (body, _) = (w.desk).answer_with(
            "status",
            None,
            &shown,
            Shown {
                board: &board,
                records: &records,
                ..Shown::default()
            },
        );
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["records"], records, "beside the board");
        // No ledger yet, and the Claude plan is unread, so it waits a recheck.
        let unread = json!({ "secs": DEFAULT_SECS, "cost_usd": null, "wait": RECHECK_SECS });
        assert_eq!(
            body["board"],
            json!({ "opus-high": unread, "sonnet-high": unread })
        );
    }

    #[test]
    fn status_shows_the_credit_spent_and_its_cap_and_omits_it_when_none_is_set() {
        let mut w = world();
        let dir = tempfile::tempdir().unwrap();
        let billed = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                      effort: high\nlogin: api\n---\n";
        crate::test::write_in(&dir.path().join("agents"), "billed.md", billed);
        let agents = crate::agents::Agents::load(&dir.path().join("agents")).unwrap();
        let home = dir.path().join("kelpie");
        let (start, end) = WINDOW;
        call(&home, "koji", ("billed", start + 5), Some(12.5));
        let mut asked = |credit| {
            let board = std::sync::Mutex::new(Board::new(&agents).credited(credit));
            refresh(&board, &home);
            let view = crate::dog::board::lock(&board).view();
            let credit = view.credit_status(Timestamp(EPOCH));
            let shown = Shown {
                api_credit: &credit,
                ..Shown::default()
            };
            let (body, _) = w
                .desk
                .answer_with("status", None, &Availability::default(), shown);
            serde_json::from_str::<serde_json::Value>(&body).unwrap()
        };

        let none = asked(None);
        let set = asked(Some(credit(50)));

        assert_eq!(none.get("api_credit"), None, "no credit, no key");
        assert_eq!(
            set["api_credit"],
            json!({
                "spent_usd": 12.5, "cap_usd": 50.0, "window_start": start, "window_end": end,
            })
        );
    }

    #[test]
    fn status_lists_a_hosts_e2e_lease_with_its_holder() {
        let mut w = world();
        assert_eq!(w.line("e2e:e2e-box"), json!(null), "no host asked yet");
        let taker = |ticket, pid, what: &str| Taker {
            ticket: Ticket(ticket),
            pid,
            what: what.into(),
        };
        w.desk
            .take_e2e("e2e-box", taker(1, 4242, "shep-kelpie lease e2e"));
        w.desk
            .take_e2e("e2e-box", taker(2, 4243, "shep-kelpie lease e2e"));
        w.desk
            .take_e2e("other-box", taker(3, 4244, "shep-kelpie lease e2e"));

        assert_eq!(
            w.line("e2e:e2e-box"),
            json!({
                "kind": "e2e:e2e-box",
                "capacity": 1,
                "holders": [{ "pid": 4242, "what": "shep-kelpie lease e2e", "since": EPOCH }],
                "queue": [{ "pid": 4243, "what": "shep-kelpie lease e2e" }],
            })
        );
        assert_eq!(w.line("e2e:other-box")["holders"][0]["pid"], 4244);
    }

    #[test]
    fn the_gpu_is_no_lease_to_take_or_return() {
        let mut w = world();
        for action in ["take", "return"] {
            let (body, _) = w.ask(action, Some("gpu"));
            let error = body["error"].as_str().unwrap();
            assert!(error.contains("shep paddock run"), "{action}: {error}");
        }
        assert_eq!(w.ask("status", None).0["leases"][0]["kind"], "cargo-test");
    }

    #[test]
    fn a_malformed_trigger_is_refused() {
        let mut w = world();
        for (action, params, error) in [
            ("status", Some("now"), "`status` takes no params"),
            ("take", None, "`take` takes a lease kind"),
            ("return", Some("  "), "`return` takes a lease kind"),
            ("grant", Some("stand-in"), "unknown action `grant`"),
        ] {
            assert_eq!(w.ask(action, params).0, json!({ "error": error }));
        }
        assert_eq!(
            w.ask("take", Some("Stand In")).0["error"],
            "\"Stand In\" is not a lease kind: use lowercase letters, digits and -"
        );
    }

    #[test]
    fn every_listed_action_is_answered() {
        let mut w = world();
        for action in ACTIONS {
            let (body, _) = w.ask(action, (action != "status").then_some("stand-in"));
            assert!(body.get("error").is_none(), "{action}: {body}");
        }
    }
}
