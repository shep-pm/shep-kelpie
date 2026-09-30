//! The maintainer's triggers on the dog: `status`, `take` and `return`

use serde_json::json;

use super::desk::{Delivery, Desk, deliveries};
use crate::lease::book::Asked;
use crate::lease::{GPU, Holder, LeaseKind};

/// The triggers the dog answers for the maintainer
pub const ACTIONS: [&str; 3] = ["status", "take", "return"];

impl Desk {
    /// Answers one of the maintainer's triggers with a JSON body
    ///
    /// `status` lists every lease, the GPU's read from its lock. `take`
    /// and `return` name a book lease and act for the maintainer. Asking
    /// with `take` again is how a waiting maintainer learns of the grant.
    pub fn answer(&mut self, action: &str, params: Option<&str>) -> (String, Vec<Delivery>) {
        let error = |message: String| (json!({ "error": message }).to_string(), Vec::new());
        let params = params.map(str::trim).filter(|p| !p.is_empty());
        let kind = match (action, params) {
            ("status", None) => return (self.status().to_string(), Vec::new()),
            ("status", Some(_)) => return error("`status` takes no params".into()),
            ("take" | "return", Some(GPU)) => {
                return error(format!(
                    "the GPU lease is the qwen scripts' lock: run `shep kelpie lease {action} gpu`"
                ));
            }
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

    fn status(&self) -> serde_json::Value {
        let holder = self.gpu.holder();
        let gpu = json!({
            "kind": GPU,
            "lock": self.gpu.path(),
            "since": holder.as_ref().and_then(|h| h.since),
            "holder": holder,
        });
        let mut leases = vec![gpu];
        leases.extend(self.book.status().iter().map(|l| json!(l)));
        json!({ "leases": leases })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ACTIONS;
    use crate::dog::desk::tests::{grant, stand_in, world};
    use crate::lease::Epoch;
    use crate::lease::wire::Asker;

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
    fn status_reads_the_gpu_from_its_lock() {
        let mut w = world();
        let lock = w.desk.gpu.clone();
        assert_eq!(
            w.ask("status", None).0,
            json!({ "leases": [{
                "kind": "gpu",
                "lock": lock.path(),
                "since": null,
                "holder": null,
            }, {
                "kind": "coderabbit",
                "holder": null,
                "since": null,
                "queue": [],
                "window": { "quota": 1, "summons": [], "opens": null },
            }] })
        );
        let me = std::process::id();
        let claim = crate::lease::gpu::Claim {
            pid: me,
            what: "round 1 in /tmp/hunks".into(),
        };
        lock.try_take(&claim).unwrap();
        let gpu = &w.ask("status", None).0["leases"][0];
        assert_eq!(
            gpu["holder"],
            json!({
                "pid": me,
                "live": true,
                "what": "round 1 in /tmp/hunks",
                "session": std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").unwrap_or_default(),
                "since": gpu["since"],
            })
        );
        assert!(gpu["since"].as_u64().is_some(), "{gpu}");
    }

    #[test]
    fn the_maintainer_is_sent_to_the_lock_for_the_gpu() {
        let mut w = world();
        for action in ["take", "return"] {
            let (body, _) = w.ask(action, Some("gpu"));
            assert_eq!(
                body["error"],
                format!(
                    "the GPU lease is the qwen scripts' lock: run `shep kelpie lease {action} gpu`"
                )
            );
        }
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
