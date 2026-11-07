//! Which of a pull request's talk a case file holds
//!
//! Only what a writer said and what a bot the project lists said goes in.
//! What anyone else wrote is kept nowhere: a thread that holds nothing else is
//! counted, and a remark among others is dropped.

use crate::ports::{Remark, Standing, Talk, TalkThread};
use crate::runner::Runner;

/// The part of a pull request's talk a case file holds
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::runner) struct CaseThreads {
    /// The open threads that stay, each with its writers' and listed bots' remarks only
    pub threads: Vec<TalkThread>,
    /// The conversation's comments by a writer or a listed bot
    pub comments: Vec<Remark>,
    /// How many open threads were left out, their words kept nowhere
    pub left_out: usize,
}

impl Runner {
    /// What of `talk` a case file holds
    ///
    /// A thread is open when it is neither resolved nor outdated. One stays
    /// when a listed bot opened it or a writer spoke in it.
    pub(in crate::runner) fn case_threads(&self, talk: &Talk) -> CaseThreads {
        let logins = self.listed_logins();
        let listed = |r: &Remark| logins.iter().any(|l| l == &r.author);
        let trusted = |r: &Remark| match r.standing {
            Standing::Writer => true,
            Standing::Bot => listed(r),
            Standing::Other => false,
        };
        let mut kept = CaseThreads::default();
        for thread in (talk.threads.iter()).filter(|t| !t.resolved && !t.outdated) {
            let opened_by_trusted = thread.remarks.first().is_some_and(trusted);
            let spoken_in = thread
                .remarks
                .iter()
                .any(|r| r.standing == Standing::Writer);
            if !(opened_by_trusted || spoken_in) {
                kept.left_out += 1;
                continue;
            }
            kept.threads.push(TalkThread {
                remarks: thread
                    .remarks
                    .iter()
                    .filter(|r| trusted(r))
                    .cloned()
                    .collect(),
                ..thread.clone()
            });
        }
        kept.comments = talk
            .comments
            .iter()
            .filter(|r| trusted(r))
            .cloned()
            .collect();
        kept
    }

    // The logins, on both of the forge's APIs, of the review bots the project lists
    fn listed_logins(&self) -> Vec<String> {
        let mut logins = Vec::new();
        for bot in self.listed_bots() {
            let profile = self.profile(bot.bot);
            let login = profile.login();
            logins.extend([login.graphql.to_owned(), login.rest.to_owned()]);
        }
        logins
    }
}
