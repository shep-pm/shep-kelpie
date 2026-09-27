//! The rig's CodeRabbit: what it posts is what a test says it posted, in
//! the shapes the real one was recorded posting on shep

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::coderabbit::{Activity, Comment, Review, Thread};
use crate::ports::{ForgeError, Timestamp};

/// Labels on pull requests, CodeRabbit's activity on them, and every
/// label change and resolved thread, in order
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeCodeRabbit {
    activity: Arc<Mutex<HashMap<u64, Activity>>>,
    labels: Arc<Mutex<HashMap<u64, Vec<String>>>>,
    label_log: Arc<Mutex<Vec<(u64, String, bool)>>>,
    resolved: Arc<Mutex<Vec<String>>>,
    down: Arc<AtomicBool>,
}

/// A CodeRabbit thread's first comment, as it words a Minor finding
pub(crate) fn finding_body(title: &str) -> String {
    format!(
        "_🎯 Functional Correctness_ | _🟡 Minor_ | _⚡ Quick win_\n\n\
         <details>\n<summary>🔎 Supported by static analysis</summary>\n\n\
         🏁 Script executed:\n</details>\n\n**{title}**\n\nIt matters because it breaks.\n"
    )
}

impl FakeCodeRabbit {
    /// Reviews `head` of pull request `number` at `at`, opening a thread
    /// for each title, or posting a clean walkthrough when there are none
    pub(crate) fn review(&self, number: u64, head: &str, at: u64, titles: &[&str]) {
        let mut activity = self.activity.lock().unwrap();
        let seen = activity.entry(number).or_default();
        seen.comments.retain(|c| !c.body.contains("## Walkthrough"));
        seen.comments.push(Comment {
            body: format!(
                "## Walkthrough\n\nA change.\n\n<!-- final_review_risk_coverage:\
                 {{\"sourceCommitId\":\"{head}\",\"coveredCommitId\":\"{head}\",\"kind\":\"reviewed\"}} -->\n\n\
                 **Included review availability:** Your plan provides up to 1 included \
                 review per hour; 0 remain after this review."
            ),
            at: Timestamp(at),
        });
        if titles.is_empty() {
            return;
        }
        seen.reviews.push(Review {
            commit: head.to_owned(),
            body: format!("**Actionable comments posted: {}**", titles.len()),
            at: Timestamp(at),
        });
        let first = seen.threads.len();
        seen.threads
            .extend(titles.iter().enumerate().map(|(i, title)| Thread {
                id: format!("PRRT_{number}_{}", first + i),
                resolved: false,
                path: "work.txt".into(),
                line: Some(1),
                body: finding_body(title),
            }));
    }

    /// Refuses a summon on pull request `number` at `at`, quoting `minutes`
    pub(crate) fn refuse(&self, number: u64, at: u64, minutes: u64) {
        let mut activity = self.activity.lock().unwrap();
        activity.entry(number).or_default().comments.push(Comment {
            body: format!(
                "> [!WARNING]\n> ## Review limit reached\n> \n> \
                 **Next included review available in {minutes} minutes.**"
            ),
            at: Timestamp(at),
        });
    }

    /// Starts reading pull request `number` at `at`, with nothing posted yet
    pub(crate) fn start(&self, number: u64, at: u64) {
        let mut activity = self.activity.lock().unwrap();
        activity.entry(number).or_default().comments.push(Comment {
            body: "<summary>📒 Files selected for processing (1)</summary>".into(),
            at: Timestamp(at),
        });
    }

    /// Resolves thread `id` as CodeRabbit does once it sees a fix
    pub(crate) fn settle(&self, id: &str) {
        let mut activity = self.activity.lock().unwrap();
        for thread in activity.values_mut().flat_map(|a| a.threads.iter_mut()) {
            if thread.id == id {
                thread.resolved = true;
            }
        }
    }

    /// Makes reading CodeRabbit fail, or work again
    pub(crate) fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Every label change, oldest first: pull request, label, on or off
    pub(crate) fn label_log(&self) -> Vec<(u64, String, bool)> {
        self.label_log.lock().unwrap().clone()
    }

    /// Every thread resolved, in order
    pub(crate) fn resolved(&self) -> Vec<String> {
        self.resolved.lock().unwrap().clone()
    }

    pub(super) fn labels(&self, number: u64) -> Vec<String> {
        let labels = self.labels.lock().unwrap();
        labels.get(&number).cloned().unwrap_or_default()
    }

    pub(super) fn set_label(&self, number: u64, label: &str, on: bool) {
        let mut labels = self.labels.lock().unwrap();
        let on_pr = labels.entry(number).or_default();
        on_pr.retain(|l| l != label);
        if on {
            on_pr.push(label.to_owned());
        }
        let entry = (number, label.to_owned(), on);
        self.label_log.lock().unwrap().push(entry);
    }

    pub(super) fn activity(&self, number: u64) -> Result<Activity, ForgeError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("CodeRabbit's comments are down".into()));
        }
        let activity = self.activity.lock().unwrap();
        Ok(activity.get(&number).cloned().unwrap_or_default())
    }

    pub(super) fn resolve(&self, id: &str) {
        self.settle(id);
        self.resolved.lock().unwrap().push(id.to_owned());
    }
}
