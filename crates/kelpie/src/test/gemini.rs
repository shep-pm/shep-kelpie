//! The rig's Gemini: what it posts is what a test says it posted, in the
//! shapes recorded from its reviews on public repos

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::FakeClock;
use crate::gemini::{Activity, Comment, Review, Thread};
use crate::ports::{Clock, ForgeError, Timestamp};

/// Gemini's activity on each pull request, with every summon posted to it
#[derive(Debug, Clone)]
pub(crate) struct FakeGemini {
    clock: FakeClock,
    activity: Arc<Mutex<HashMap<u64, Activity>>>,
    summons: Arc<Mutex<Vec<u64>>>,
}

/// A Gemini inline comment, as it words a medium finding
fn finding_body(title: &str) -> String {
    format!(
        "![medium](https://www.gstatic.com/codereviewagent/medium-priority.svg)\n\n\
         {title}\n\nIt matters because it breaks.\n\n```suggestion\nfixed\n```"
    )
}

impl FakeGemini {
    pub(super) fn new(clock: FakeClock) -> Self {
        Self {
            clock,
            activity: Arc::default(),
            summons: Arc::default(),
        }
    }

    /// Reviews `head` of pull request `number` now, opening a thread for
    /// each title. A review with none is clean, and posted all the same.
    pub(crate) fn review(&self, number: u64, head: &str, titles: &[&str]) {
        let at = self.clock.now();
        let mut activity = self.activity.lock().unwrap();
        let seen = activity.entry(number).or_default();
        let id = 9000 + seen.reviews.len() as u64;
        seen.reviews.push(Review {
            id,
            commit: head.to_owned(),
            at,
        });
        let first = seen.threads.len();
        seen.threads
            .extend(titles.iter().enumerate().map(|(i, title)| Thread {
                id: format!("PRRT_gemini_{number}_{}", first + i),
                resolved: false,
                path: "work.txt".into(),
                line: Some(1),
                body: finding_body(title),
                review: Some(id),
            }));
    }

    /// Refuses the latest summon on pull request `number` now, out of quota
    pub(crate) fn refuse(&self, number: u64) {
        let at = self.clock.now();
        let mut activity = self.activity.lock().unwrap();
        activity.entry(number).or_default().comments.push(Comment {
            body: "> [!WARNING]\n> You have reached your daily quota limit. Please wait up \
                   to 24 hours and I will start processing your requests again!"
                .into(),
            at,
        });
    }

    /// Every pull request a summon was posted on, in order
    pub(crate) fn summons(&self) -> Vec<u64> {
        self.summons.lock().unwrap().clone()
    }

    /// A `/gemini review` comment, posted now, by kelpie or by hand
    pub(crate) fn summoned(&self, number: u64) {
        let at: Timestamp = self.clock.now();
        let mut activity = self.activity.lock().unwrap();
        activity.entry(number).or_default().summons.push(at);
        self.summons.lock().unwrap().push(number);
    }

    pub(super) fn activity(&self, number: u64) -> Result<Activity, ForgeError> {
        let activity = self.activity.lock().unwrap();
        Ok(activity.get(&number).cloned().unwrap_or_default())
    }

    pub(super) fn knows(&self, id: &str) -> bool {
        let activity = self.activity.lock().unwrap();
        activity
            .values()
            .flat_map(|a| &a.threads)
            .any(|t| t.id == id)
    }

    pub(super) fn settle(&self, id: &str) {
        let mut activity = self.activity.lock().unwrap();
        for thread in activity.values_mut().flat_map(|a| a.threads.iter_mut()) {
            if thread.id == id {
                thread.resolved = true;
            }
        }
    }
}
