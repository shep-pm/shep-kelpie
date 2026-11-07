//! The rig's CodeRabbit: what it posts is what a test says it posted, in
//! the shapes the real one was recorded posting on shep

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::ports::{ForgeError, Remark, Standing, Talk, TalkThread, Timestamp};
use crate::review_bot::{Activity, Comment, Review, Status, Thread};

/// Labels on pull requests, each bot's activity on them by its login, and
/// every label change and resolved thread, in order
#[derive(Debug, Clone, Default)]
pub struct FakeCodeRabbit {
    activity: Arc<Mutex<HashMap<(u64, String), Activity>>>,
    labels: Arc<Mutex<HashMap<u64, Vec<String>>>>,
    label_log: Arc<Mutex<Vec<(u64, String, bool)>>>,
    resolved: Arc<Mutex<Vec<String>>>,
    replies: Arc<Mutex<Vec<(String, String)>>>,
    logins: Arc<Mutex<Vec<String>>>,
    down: Arc<AtomicBool>,
    resolve_down: Arc<AtomicBool>,
    reply_down: Arc<AtomicBool>,
    gh_writes: Arc<AtomicUsize>,
    // What people wrote on each pull request, and the ids the next remark gets
    talk: Arc<Mutex<HashMap<u64, Talk>>>,
    remark_ids: Arc<AtomicUsize>,
}

// CodeRabbit's own store on pull request `number`.
fn ours(number: u64) -> (u64, String) {
    (number, crate::coderabbit::LOGIN.rest.to_owned())
}

/// A CodeRabbit thread's first comment, as it words a Minor finding
fn finding_body(title: &str) -> String {
    format!(
        "_🎯 Functional Correctness_ | _🟡 Minor_ | _⚡ Quick win_\n\n\
         <details>\n<summary>🔎 Supported by static analysis</summary>\n\n\
         🏁 Script executed:\n</details>\n\n**{title}**\n\nIt matters because it breaks.\n"
    )
}

impl FakeCodeRabbit {
    /// Reviews `head` of pull request `number` at `at`, opening a thread
    /// for each title, or posting a clean walkthrough when there are none
    pub fn review(&self, number: u64, head: &str, at: u64, titles: &[&str]) {
        let mut activity = self.activity.lock().unwrap();
        let seen = activity.entry(ours(number)).or_default();
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
                outdated: false,
                path: "work.txt".into(),
                line: Some(1),
                body: finding_body(title),
            }));
    }

    /// Changes the activity of the bot posting as `login` on pull request
    /// `number` as `post` says, in whatever shape that bot posts it
    pub fn post_as(&self, number: u64, login: &str, post: impl FnOnce(&mut Activity)) {
        let mut activity = self.activity.lock().unwrap();
        post(activity.entry((number, login.to_owned())).or_default());
    }

    /// Every login the runner read a bot's activity by, in order
    pub fn logins(&self) -> Vec<String> {
        self.logins.lock().unwrap().clone()
    }

    /// Refuses a summon on pull request `number` at `at`, quoting `minutes`
    pub fn refuse(&self, number: u64, at: u64, minutes: u64) {
        let mut activity = self.activity.lock().unwrap();
        activity
            .entry(ours(number))
            .or_default()
            .comments
            .push(Comment {
                body: format!(
                    "> [!WARNING]\n> ## Review limit reached\n> \n> \
                 **Next included review available in {minutes} minutes.**"
                ),
                at: Timestamp(at),
            });
    }

    /// Starts reading pull request `number` at `at`, with nothing posted yet
    pub fn start(&self, number: u64, at: u64) {
        let mut activity = self.activity.lock().unwrap();
        activity
            .entry(ours(number))
            .or_default()
            .comments
            .push(Comment {
                body: "<summary>📒 Files selected for processing (1)</summary>".into(),
                at: Timestamp(at),
            });
    }

    /// Sets "Review in progress" on `head` of pull request `number` at `at`
    pub fn progress(&self, number: u64, head: &str, at: u64) {
        let mut activity = self.activity.lock().unwrap();
        activity.entry(ours(number)).or_default().statuses.insert(
            0,
            Status {
                commit: head.to_owned(),
                description: "Review in progress".into(),
                at: Timestamp(at),
            },
        );
    }

    /// Sets "Review completed" on `head` of pull request `number` at `at`,
    /// with no review or comment: CodeRabbit found nothing new to read
    pub fn complete(&self, number: u64, head: &str, at: u64) {
        let mut activity = self.activity.lock().unwrap();
        activity.entry(ours(number)).or_default().statuses.insert(
            0,
            Status {
                commit: head.to_owned(),
                description: "Review completed".into(),
                at: Timestamp(at),
            },
        );
    }

    /// Sets the skip status `description` on `head` of pull request
    /// `number` at `at`, as CodeRabbit answers a summon its settings do not
    /// let start a review
    pub fn skip(&self, number: u64, head: &str, at: u64, description: &str) {
        let mut activity = self.activity.lock().unwrap();
        activity.entry(ours(number)).or_default().statuses.insert(
            0,
            Status {
                commit: head.to_owned(),
                description: description.into(),
                at: Timestamp(at),
            },
        );
    }

    /// Resolves thread `id` as CodeRabbit does once it sees a fix
    pub fn settle(&self, id: &str) {
        let mut activity = self.activity.lock().unwrap();
        for thread in activity.values_mut().flat_map(|a| a.threads.iter_mut()) {
            if thread.id == id {
                thread.resolved = true;
            }
        }
    }

    /// Makes reading CodeRabbit fail, or work again
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Every label change, oldest first: pull request, label, on or off
    pub fn label_log(&self) -> Vec<(u64, String, bool)> {
        self.label_log.lock().unwrap().clone()
    }

    /// Every thread resolved, in order
    pub fn resolved(&self) -> Vec<String> {
        self.resolved.lock().unwrap().clone()
    }

    /// Every reply posted on a thread, in order: the thread's id and the text
    pub fn replies(&self) -> Vec<(String, String)> {
        self.replies.lock().unwrap().clone()
    }

    /// How many replies and resolves came through `gh`, as the maintainer
    pub fn gh_writes(&self) -> usize {
        self.gh_writes.load(Ordering::SeqCst)
    }

    /// A reply or resolve made through the forge port, which is `gh`.
    pub fn made_by_gh(&self) {
        self.gh_writes.fetch_add(1, Ordering::SeqCst);
    }

    /// Makes replying to a thread fail, or work again
    pub fn set_reply_down(&self, down: bool) {
        self.reply_down.store(down, Ordering::SeqCst);
    }

    /// The labels the stand-in holds on `number`
    pub fn labels(&self, number: u64) -> Vec<String> {
        let labels = self.labels.lock().unwrap();
        labels.get(&number).cloned().unwrap_or_default()
    }

    /// Kelpie's own label change, which the log keeps.
    pub fn set_label(&self, number: u64, label: &str, on: bool) {
        self.put_label(number, label, on);
        let entry = (number, label.to_owned(), on);
        self.label_log.lock().unwrap().push(entry);
    }

    /// Anyone's label change, kelpie's or someone else's.
    pub fn put_label(&self, number: u64, label: &str, on: bool) {
        let mut labels = self.labels.lock().unwrap();
        let on_pr = labels.entry(number).or_default();
        on_pr.retain(|l| l != label);
        if on {
            on_pr.push(label.to_owned());
        }
    }

    /// What the stand-in reads of `login`'s activity on `number`
    pub fn activity(&self, number: u64, login: &str) -> Result<Activity, ForgeError> {
        self.logins.lock().unwrap().push(login.to_owned());
        let coderabbit = login == crate::coderabbit::LOGIN.rest;
        if coderabbit && self.down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("CodeRabbit's comments are down".into()));
        }
        let activity = self.activity.lock().unwrap();
        let key = (number, login.to_owned());
        Ok(activity.get(&key).cloned().unwrap_or_default())
    }

    /// `author` writes `body` on pull request `number`'s conversation, and the comment's id
    pub fn person_says(&self, number: u64, author: &str, standing: Standing, body: &str) -> u64 {
        let remark = self.remark(author, standing, body);
        let id = remark.id;
        self.talk
            .lock()
            .unwrap()
            .entry(number)
            .or_default()
            .comments
            .push(remark);
        id
    }

    /// `author` opens a review thread on `path` of pull request `number`, and its id
    pub fn person_opens(
        &self,
        number: u64,
        path: &str,
        author: &str,
        standing: Standing,
        body: &str,
    ) -> String {
        let remark = self.remark(author, standing, body);
        let id = format!("PRRT_person_{}", remark.id);
        self.talk
            .lock()
            .unwrap()
            .entry(number)
            .or_default()
            .threads
            .push(TalkThread {
                id: id.clone(),
                path: path.to_owned(),
                line: Some(3),
                resolved: false,
                outdated: false,
                remarks: vec![remark],
            });
        id
    }

    /// `author` replies in review thread `thread`, and the reply's id
    pub fn person_replies(
        &self,
        thread: &str,
        author: &str,
        standing: Standing,
        body: &str,
    ) -> u64 {
        let remark = self.remark(author, standing, body);
        let id = remark.id;
        let mut talk = self.talk.lock().unwrap();
        let mut found = talk.values_mut().flat_map(|t| &mut t.threads);
        let found = found.find(|t| t.id == thread);
        found.expect("the thread exists").remarks.push(remark);
        id
    }

    /// Marks review thread `thread` outdated: the lines it is on changed
    pub fn person_outdates(&self, thread: &str) {
        let mut talk = self.talk.lock().unwrap();
        let mut found = talk.values_mut().flat_map(|t| &mut t.threads);
        found
            .find(|t| t.id == thread)
            .expect("the thread exists")
            .outdated = true;
    }

    fn remark(&self, author: &str, standing: Standing, body: &str) -> Remark {
        Remark {
            id: 7000 + self.remark_ids.fetch_add(1, Ordering::SeqCst) as u64,
            author: author.to_owned(),
            standing,
            body: body.to_owned(),
        }
    }

    /// What the stand-in holds of the talk on `number`
    pub fn talk_of(&self, number: u64) -> Talk {
        let talk = self.talk.lock().unwrap();
        talk.get(&number).cloned().unwrap_or_default()
    }

    // Whether `id` is a thread a person opened, which a resolve closes.
    fn person_thread(&self, id: &str, resolve: bool) -> bool {
        let mut talk = self.talk.lock().unwrap();
        let mut known = false;
        for thread in talk.values_mut().flat_map(|t| &mut t.threads) {
            if thread.id == id {
                thread.resolved |= resolve;
                known = true;
            }
        }
        known
    }

    /// Makes resolving a thread fail, or work again
    pub fn set_resolve_down(&self, down: bool) {
        self.resolve_down.store(down, Ordering::SeqCst);
    }

    /// A thread that does not exist is done, as the gh adapter reads
    /// GitHub's not-found.
    pub fn resolve(&self, id: &str) -> Result<(), ForgeError> {
        if self.resolve_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("resolving is down".into()));
        }
        let activity = self.activity.lock().unwrap();
        let known = activity
            .values()
            .flat_map(|a| &a.threads)
            .any(|t| t.id == id);
        drop(activity);
        let theirs = self.person_thread(id, true);
        if !known && !theirs {
            return Ok(());
        }
        self.settle(id);
        self.resolved.lock().unwrap().push(id.to_owned());
        Ok(())
    }

    /// A thread that does not exist is done, as the gh adapter reads
    /// GitHub's not-found.
    pub fn reply(&self, id: &str, body: &str) -> Result<(), ForgeError> {
        if self.reply_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("replying is down".into()));
        }
        // As the guarded forge refuses a text naming this machine's folders.
        if body.contains("/Users/") {
            let leak = crate::local_paths::Leak::Path;
            return Err(ForgeError::LocalPath {
                what: "the reply",
                leak,
            });
        }
        let activity = self.activity.lock().unwrap();
        let known = activity
            .values()
            .flat_map(|a| &a.threads)
            .any(|t| t.id == id);
        drop(activity);
        if known || self.person_thread(id, false) {
            let reply = (id.to_owned(), body.to_owned());
            self.replies.lock().unwrap().push(reply);
        }
        Ok(())
    }
}
