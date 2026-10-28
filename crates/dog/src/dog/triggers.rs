//! The maintainer's triggers on the dog: `status`, `take` and `return`

use serde_json::json;

use super::availability::Availability;
use super::desk::{Delivery, Desk, deliveries};
use crate::lease::book::Asked;
use crate::lease::{Holder, LeaseKind};

/// The triggers the dog answers for the maintainer
pub const ACTIONS: [&str; 3] = ["status", "take", "return"];

impl Desk {
    /// Shorthand for <code>[Desk::answer_with]</code> with nothing read of availability
    pub fn answer(&mut self, action: &str, params: Option<&str>) -> (String, Vec<Delivery>) {
        self.answer_with(action, params, &Availability::default())
    }

    /// Answers one of the maintainer's triggers with a JSON body
    ///
    /// `status` lists every lease and `availability`. `take`
    /// and `return` name a book lease and act for the maintainer. Asking
    /// with `take` again is how a waiting maintainer learns of the grant.
    pub fn answer_with(
        &mut self,
        action: &str,
        params: Option<&str>,
        availability: &Availability,
    ) -> (String, Vec<Delivery>) {
        let answered = self.respond(action, params, availability);
        self.look(true);
        answered
    }

    fn respond(
        &mut self,
        action: &str,
        params: Option<&str>,
        availability: &Availability,
    ) -> (String, Vec<Delivery>) {
        let error = |message: String| (json!({ "error": message }).to_string(), Vec::new());
        let params = params.map(str::trim).filter(|p| !p.is_empty());
        let kind = match (action, params) {
            ("status", None) => return (self.status(availability).to_string(), Vec::new()),
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

    fn status(&self, availability: &Availability) -> serde_json::Value {
        let mut leases = vec![json!(self.tests.status())];
        leases.extend(self.e2e.values().map(|l| json!(l.status())));
        leases.extend(self.book.status().iter().map(|l| json!(l)));
        json!({ "leases": leases, "availability": availability.status() })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ACTIONS;
    use crate::dog::availability::{Availability, Sources};
    use crate::dog::desk::tests::{grant, stand_in, world};
    use crate::lease::Epoch;
    use crate::lease::counted::{Taker, Ticket};
    use crate::lease::wire::Asker;
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
            }], "availability": { "models": [], "accounts": [] } })
        );
    }

    #[test]
    fn status_shows_each_account_and_model_the_dog_reads() {
        let mut w = world();
        let agents = crate::agents::Agents::embedded();
        let sources = Sources::of(&agents, &crate::settings::Gateways::default());
        let (body, _) = w
            .desk
            .answer_with("status", None, &Availability::new(&sources));
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
