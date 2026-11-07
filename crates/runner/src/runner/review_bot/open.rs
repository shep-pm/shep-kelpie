//! The threads an item at the gate waits on, which a ruling on it names

use super::settle::first_sentence;
use crate::ports::{Remark, Severity, Standing, Talk, TalkThread};
use crate::review_bot::Thread;
use crate::runner::Runner;
use crate::work_item::WorkItem;

/// An open thread an item waits on, as the gate rules on it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct RuledThread {
    /// The forge's id for it
    pub id: String,
    /// The file it is on
    pub path: String,
    /// The line, when the forge still places it
    pub line: Option<u32>,
    /// Who opened it
    pub author: String,
    /// How serious the bot's profile reads it, when a listed bot opened it
    pub severity: Option<Severity>,
    /// What it says, to its first sentence
    pub what: String,
    /// Whether a writer spoke in it
    pub maintainer_wrote: bool,
}

/// The open threads an item waits on, and how many `case_threads` left out
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::runner) struct ThreadsToRule {
    /// Every open thread that stays and is not the worker's or accepted
    pub threads: Vec<RuledThread>,
    /// How many open threads `case_threads` left out
    pub left_out: usize,
}

impl Runner {
    /// The threads in `talk` that `item` waits on
    ///
    /// These are the open threads `case_threads` keeps, less those in
    /// `threads_sent` and `threads_accepted`. A thread comes back whole in
    /// number: nothing is capped.
    pub(in crate::runner) fn threads_to_rule(&self, item: &WorkItem, talk: &Talk) -> ThreadsToRule {
        let kept = self.case_threads(talk);
        let settled = item.threads_sent.iter().chain(&item.threads_accepted);
        let settled: Vec<&String> = settled.collect();
        let threads = (kept.threads.iter())
            .filter(|t| !settled.contains(&&t.id))
            .filter_map(|t| self.ruled(t))
            .collect();
        ThreadsToRule {
            threads,
            left_out: kept.left_out,
        }
    }

    // `thread` as the gate rules on it, none when it holds no remark
    fn ruled(&self, thread: &TalkThread) -> Option<RuledThread> {
        let first: &Remark = thread.remarks.first()?;
        let bot = (self.listed_bots().into_iter())
            .map(|listed| self.profile(listed.bot))
            .find(|profile| {
                let login = profile.login();
                first.standing == Standing::Bot
                    && (first.author == login.graphql || first.author == login.rest)
            });
        let (severity, what) = match &bot {
            Some(profile) => {
                let finding = profile.finding(&Thread {
                    id: thread.id.clone(),
                    resolved: thread.resolved,
                    outdated: thread.outdated,
                    path: thread.path.clone(),
                    line: thread.line,
                    body: first.body.clone(),
                });
                (Some(finding.severity), first_sentence(&finding.what))
            }
            None => (None, first_sentence(&first.body)),
        };
        Some(RuledThread {
            id: thread.id.clone(),
            path: thread.path.clone(),
            line: thread.line,
            author: first.author.clone(),
            severity,
            what,
            maintainer_wrote: (thread.remarks.iter()).any(|r| r.standing == Standing::Writer),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;
    use crate::ports::SessionId;
    use crate::runner::review_bot::two_bots::listing;
    use crate::test::{Rig, ticket};

    fn runner() -> (Rig, Mutex<Runner>) {
        let rig = listing("open-threads", &["cubic"]);
        let runner = rig.open().unwrap();
        (rig, runner)
    }

    fn item() -> WorkItem {
        WorkItem::new(
            ticket(7),
            "Title of #7",
            "kelpie/7",
            PathBuf::from("wt"),
            PathBuf::from("build"),
            "sonnet-high".to_owned().try_into().unwrap(),
            SessionId("s".to_owned()),
        )
    }

    fn remark(id: u64, author: &str, standing: Standing, body: &str) -> Remark {
        Remark {
            id,
            author: author.to_owned(),
            standing,
            body: body.to_owned(),
        }
    }

    fn thread(id: &str, path: &str, remarks: Vec<Remark>) -> TalkThread {
        TalkThread {
            id: id.to_owned(),
            path: path.to_owned(),
            line: Some(3),
            resolved: false,
            outdated: false,
            remarks,
        }
    }

    fn cubic(body: &str) -> Remark {
        remark(1, "cubic-dev-ai", Standing::Bot, body)
    }

    fn writer(body: &str) -> Remark {
        remark(2, "octocat", Standing::Writer, body)
    }

    fn talk(threads: Vec<TalkThread>) -> Talk {
        Talk {
            comments: Vec::new(),
            threads,
        }
    }

    fn ruled(item: &WorkItem, talk: &Talk) -> ThreadsToRule {
        let (_rig, runner) = runner();
        let runner = runner.lock().unwrap();
        runner.threads_to_rule(item, talk)
    }

    #[test]
    fn a_listed_bots_nit_is_in_the_list() {
        let nit = "P3: The name could be shorter. It is long.";
        let talk = talk(vec![thread("T1", "src/a.rs", vec![cubic(nit)])]);

        let got = ruled(&item(), &talk);

        assert_eq!(
            got.threads,
            [RuledThread {
                id: "T1".to_owned(),
                path: "src/a.rs".to_owned(),
                line: Some(3),
                author: "cubic-dev-ai".to_owned(),
                severity: Some(Severity::Low),
                what: "The name could be shorter.".to_owned(),
                maintainer_wrote: false,
            }]
        );
    }

    #[test]
    fn a_writers_open_thread_is_in_the_list_with_its_place() {
        let talk = talk(vec![thread(
            "T2",
            "src/b.rs",
            vec![writer("Rename this. It reads badly.")],
        )]);

        let got = ruled(&item(), &talk).threads;

        let [one] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!(
            (one.id.as_str(), one.path.as_str(), one.line),
            ("T2", "src/b.rs", Some(3))
        );
        assert_eq!((one.author.as_str(), one.severity), ("octocat", None));
        assert_eq!(one.what, "Rename this.");
    }

    #[test]
    fn a_thread_sent_to_the_worker_is_not_in_the_list_until_it_is_answered() {
        let talk = talk(vec![thread("T1", "a.rs", vec![cubic("P1: Wrong.")])]);
        let mut item = item();
        item.threads_sent = vec!["T1".to_owned()];
        assert!(ruled(&item, &talk).threads.is_empty());

        item.threads_sent.clear();

        let ids: Vec<String> = ruled(&item, &talk)
            .threads
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["T1"]);
    }

    #[test]
    fn a_thread_accepted_earlier_is_not_in_the_list() {
        let talk = talk(vec![
            thread("T1", "a.rs", vec![cubic("P1: Wrong.")]),
            thread("T2", "b.rs", vec![cubic("P2: Odd.")]),
        ]);
        let mut item = item();
        item.threads_accepted = vec!["T1".to_owned()];

        let ids: Vec<String> = ruled(&item, &talk)
            .threads
            .into_iter()
            .map(|t| t.id)
            .collect();

        assert_eq!(ids, ["T2"]);
    }

    #[test]
    fn a_thread_the_maintainer_wrote_in_says_so() {
        let talk = talk(vec![
            thread("T1", "a.rs", vec![cubic("P2: Odd."), writer("Fix it.")]),
            thread("T2", "b.rs", vec![cubic("P2: Odd.")]),
        ]);

        let got = ruled(&item(), &talk).threads;

        let wrote: Vec<(&str, bool)> = got
            .iter()
            .map(|t| (t.id.as_str(), t.maintainer_wrote))
            .collect();
        assert_eq!(wrote, [("T1", true), ("T2", false)]);
    }

    #[test]
    fn what_case_threads_left_out_is_counted_and_not_in_the_list() {
        let stranger = remark(3, "passerby", Standing::Other, "Nice.");
        let talk = talk(vec![
            thread("T1", "a.rs", vec![stranger]),
            thread("T2", "b.rs", vec![cubic("P2: Odd.")]),
        ]);

        let got = ruled(&item(), &talk);

        assert_eq!(got.left_out, 1);
        let ids: Vec<String> = got.threads.into_iter().map(|t| t.id).collect();
        assert_eq!(ids, ["T2"]);
    }
}
