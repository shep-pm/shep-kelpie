//! Answering rulings from replies in their thread
//!
//! A ruling posted through kelpie's App that mentions someone says to reply in
//! its thread. Each board poll reads the repo's new comments once, and a reply
//! counts when its author is the login the ruling mentions and has write
//! access. The reply is read as `shep kelpie rule` reads an answer, with the
//! ruling's id first (see [`read_answer`]): `99 yes`, `99 no <note>` or `99
//! rework <note>`. A bare `yes` answers nothing, so a conversational one never
//! merges. A 👍 or 👎 on the ruling's own comment, or a ticked box, answers it
//! too (see [`marked`]). The first answer from any channel wins.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use self::settled::Ended;
pub(super) use self::settled::{Outcome, Settler};
use super::pm::{PmReply, held_line, pm_reply};
use super::trigger::lock;
use super::words::{Wants, is_free_text, read_answer};
use super::{Answer, RuleError, Runner, voice};
use crate::ports::{CommentMarks, Forge, ThreadComment, Timestamp};
use crate::settings::ForgeSlug;
use crate::state::{Contribution, Heard, ProjectState, Ruling, RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// Seconds a ruling answered elsewhere is still watched, so a reply that
/// came before the answer is told it is settled
const LATE: u64 = 60 * 60;

/// Seconds a ruling the project manager answered is watched for a `no <note>`
const DECIDED: u64 = 24 * 60 * 60;

/// How far behind a read the next one reaches, in seconds, for a comment the
/// forge was slow to list
const SLACK: u64 = 5 * 60;

/// A ruling's thread being watched for an answer
#[derive(Debug, Clone)]
struct Watch {
    ruling: TicketId,
    thread: u64,
    heard: Heard,
    /// Until when it is watched after it was answered, once it was
    settled: Option<Timestamp>,
}

/// Where reading the rulings' threads stands, kept in memory only
#[derive(Debug, Default)]
pub(super) struct Reading {
    /// When the comments were last asked for, however that went
    read: Option<Timestamp>,
    /// The comments before this are not asked for again
    floor: Option<Timestamp>,
    watching: BTreeMap<TicketId, Watch>,
    /// The comments read and left alone, a bot's or a login's without write
    /// access, by id and when written, until a read no longer reaches them
    judged: BTreeMap<u64, Timestamp>,
    /// Why the last read failed, told once while it keeps failing
    failed: Option<String>,
    /// Why the last read of a ruling's comment for reactions and ticks
    /// failed, told once while it keeps failing
    marks_failed: Option<String>,
    /// Why the last read of the followed pull requests' review threads
    /// failed, told once while it keeps failing
    replies_failed: Option<String>,
    /// Why a ruling's reaction or tick was refused, told once each, since
    /// it stays on the comment and is read again at every poll
    refused: BTreeMap<TicketId, String>,
    /// What became of each ruling that left the state while it was watched,
    /// as a reply in its thread is told, with the id it was read by
    told: BTreeMap<TicketId, (TicketId, String)>,
    /// The rulings the last save took out of the state while their comments
    /// were up, until what became of them is said
    ended: BTreeMap<TicketId, Ended>,
    /// The edit of a settled ruling's comment that failed, by ruling, tried
    /// again after a wait that doubles until it is given up
    unedited: BTreeMap<TicketId, settled::Unedited>,
    /// The rulings whose comment is being posted, each with the first line of
    /// the edit it is owed when it was settled meanwhile
    posting: BTreeMap<TicketId, Option<String>>,
    /// The rulings the project manager answered while they are watched, as
    /// they stood when they were answered, so a `no <note>` can reopen one
    decided: BTreeMap<TicketId, Ruling>,
}

/// How a reply names the ruling it answers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Naming {
    /// It names none
    Not,
    /// By the number of its issue or pull request
    Number,
}

/// What a comment on a watched thread is a reply to
enum Aimed {
    /// A pending ruling, named or not
    At(TicketId, Naming),
    /// A ruling that was answered already
    Settled(TicketId),
    /// A ruling pending on another thread
    Elsewhere(TicketId, u64),
    /// Several rulings wait on the thread and the reply names none
    Unnamed(Vec<TicketId>),
    /// A `no` that names no ruling, on the comment of one the project manager answered
    Undecided(TicketId),
    /// Nothing to answer
    Nothing,
}

/// A read of the comments the runner decided was due, which needs no lock
pub(super) struct ThreadJob {
    forge: Arc<dyn Forge>,
    repo: ForgeSlug,
    since: Timestamp,
    watches: Vec<Watch>,
    judged: BTreeSet<u64>,
    own: BTreeSet<u64>,
    /// The pull requests followed as contributions, each with the time its
    /// review threads are read since
    reviews: Vec<(u64, Timestamp)>,
}

/// What a read found
pub(super) struct ThreadRead {
    now: Timestamp,
    comments: Vec<ThreadComment>,
    /// Whether each author a reply could come from has write access
    writers: BTreeMap<String, bool>,
    /// The comment whose author's access could not be read, and why, where
    /// the read ended
    stopped: Option<(u64, String)>,
    /// What was done to each pending ruling's own comment
    marks: BTreeMap<TicketId, Result<CommentMarks, String>>,
    /// The comments in each followed pull request's review threads
    replies: BTreeMap<u64, Result<Vec<ThreadComment>, String>>,
}

/// Reads the rulings' threads for answers, if a read is due, asking the
/// forge with the runner's lock let go
pub(super) fn read_threads(runner: &Mutex<Runner>) -> Result<(), StateError> {
    let job = lock(runner).thread_job();
    let Some((now, job)) = job else {
        return Ok(());
    };
    let read = job.run(now);
    lock(runner).apply_threads(read)
}

impl ThreadJob {
    // Asks for the comments since the place read to, and who may write among
    // the authors of those that could be an answer.
    fn run(self, now: Timestamp) -> Result<ThreadRead, String> {
        let comments = (self.forge.comments_since(&self.repo, self.since))
            .map_err(|e| format!("cannot read the comments on the rulings' threads: {e}"))?;
        let mut writers = BTreeMap::new();
        let mut stopped = None;
        for comment in &comments {
            let aimed = (self.watches.iter()).any(|w| aims_at(w, comment));
            if comment.bot
                || !aimed
                || self.judged.contains(&comment.id)
                || self.own.contains(&comment.id)
                || writers.contains_key(&comment.author)
            {
                continue;
            }
            match self.forge.can_write(&self.repo, &comment.author) {
                Ok(may) => {
                    writers.insert(comment.author.clone(), may);
                }
                Err(e) => {
                    let who = &comment.author;
                    stopped = Some((comment.id, format!("cannot tell if {who} may write: {e}")));
                    break;
                }
            }
        }
        // A reaction or a tick on the ruling's own comment is read from the
        // comment itself, whatever was written in the thread since.
        let marks = (self.watches.iter())
            .filter(|w| w.settled.is_none() && w.heard.ruling_comment != 0)
            .map(|w| (w.ruling, w.heard.ruling_comment))
            .map(|(ruling, comment)| {
                let marks = (self.forge.comment_marks(&self.repo, comment))
                    .map_err(|e| format!("cannot read the reactions on comment {comment}: {e}"));
                (ruling, marks)
            })
            .collect();
        // Read apart from the repo's comments: GitHub lists a review thread's
        // comments only with its pull request.
        let replies = (self.reviews.iter())
            .map(|&(number, since)| {
                let replies = (self.forge.thread_replies_since(&self.repo, number, since))
                    .map_err(|e| format!("cannot read the review threads of #{number}: {e}"));
                (number, replies)
            })
            .collect();
        Ok(ThreadRead {
            now,
            comments,
            writers,
            stopped,
            marks,
            replies,
        })
    }
}

impl Runner {
    // Decides whether the threads are read now: at most once a board poll,
    // where kelpie's App covers the repo and a ruling waits in its thread or a
    // contribution is followed. One read of the repo's comments serves both.
    // Returns the time and what the read needs.
    fn thread_job(&mut self) -> Option<(Timestamp, ThreadJob)> {
        let now = self.ports.clock.now();
        self.watch_threads(now);
        let followed = &self.state.contributions;
        // An issue whose triage waits on its author is read too: the wait holds
        // no slot and no call, but their word is what ends it.
        let waits = self.triage_waits();
        if self.threads.watching.is_empty() && followed.is_empty() && waits.is_empty() {
            return None;
        }
        let poll = self.looks.poll().as_secs();
        if (self.threads.read).is_some_and(|at| now.0 < at.0.saturating_add(poll)) {
            return None;
        }
        if !self.app_covers() {
            return None;
        }
        self.threads.read = Some(now);
        let watches: Vec<Watch> = self.threads.watching.values().cloned().collect();
        let followed = &self.state.contributions;
        let heard = (watches
            .iter()
            .map(|w| w.heard.time)
            .chain(followed.iter().map(Contribution::latest))
            .chain(waits.iter().map(|(_, at)| *at)))
        .min();
        let since = heard.into_iter().chain(self.threads.floor).max();
        let since = since.unwrap_or(now);
        let reviews = (followed.iter())
            .map(|c| (c.pull_request, c.reviews_since()))
            .collect();
        let mut own: BTreeSet<u64> = self.state.posted.iter().copied().collect();
        own.extend(self.posted.borrow().iter().copied());
        let job = ThreadJob {
            forge: Arc::clone(&self.ports.forge),
            repo: self.remote.clone(),
            since,
            watches,
            judged: self.threads.judged.keys().copied().collect(),
            own,
            reviews,
        };
        Some((now, job))
    }

    // Acts on what a read found: the new replies by the login a ruling
    // mentions, with write access.
    fn apply_threads(&mut self, read: Result<ThreadRead, String>) -> Result<(), StateError> {
        let read = match read {
            Ok(read) => read,
            Err(why) => {
                if self.threads.failed.replace(why.clone()).as_deref() != Some(why.as_str()) {
                    self.notes.push(why);
                }
                return Ok(());
            }
        };
        self.threads.failed = None;
        let heard = self.hear_replies(&read)?;
        // An issue whose triage waits on its author is heard the same way, from
        // the same read.
        self.hear_triage(&read.comments)?;
        for comment in &read.comments {
            if (read.stopped.as_ref()).is_some_and(|(id, _)| *id == comment.id) {
                let why = read.stopped.as_ref().map(|(_, why)| why.clone());
                self.notes.extend(why);
                return Ok(());
            }
            let aimed = self.threads.aimed_by(comment);
            if aimed.is_empty() || self.threads.judged.contains_key(&comment.id) {
                continue;
            }
            // The maintainer's own login wrote it for kelpie, not as an answer.
            let own = self.state.posted.contains(&comment.id)
                || self.posted.borrow().contains(&comment.id);
            if comment.bot || own {
                self.threads.judged.insert(comment.id, comment.created);
                continue;
            }
            // An author the read did not ask about is left for the next one.
            let Some(&may) = read.writers.get(&comment.author) else {
                continue;
            };
            if may {
                // Before it is acted on, so a reply is answered once. Only a
                // reply acted on moves the place read to on its thread: any
                // other comment moving it would skip a reply the forge lists
                // later.
                let held = self.held_by(comment, &aimed);
                self.hear(comment, held.as_deref())?;
                if held.is_some() {
                    self.react_held(comment);
                } else {
                    self.respond(comment, &aimed);
                }
            } else {
                self.threads.judged.insert(comment.id, comment.created);
            }
        }
        let floor = Timestamp(read.now.0.saturating_sub(SLACK));
        // Held behind while a contribution's comments were not all heard, so
        // the next read reaches them again. An issue whose triage waits on its
        // author is held back to when the ask or the questions went up: a
        // reply that fell out of the read would end the wait as silence.
        if heard {
            let floor = (self.triage_waits())
                .into_iter()
                .map(|(_, at)| at)
                .min()
                .map_or(floor, |first| floor.min(first));
            self.threads.floor = Some(floor);
        }
        if let Some(floor) = self.threads.floor {
            self.threads.judged.retain(|_, created| *created >= floor);
        }
        self.apply_marks(read.marks);
        Ok(())
    }

    // Records what the contributors and the maintainer said, in the comments
    // and the review threads read, and returns whether all of it was: when it
    // was not, the place the comments are read to stays behind. A review read
    // that failed is read again from where it began, a place the contribution keeps.
    fn hear_replies(&mut self, read: &ThreadRead) -> Result<bool, StateError> {
        let mut replies = Vec::new();
        let mut done = Vec::new();
        let mut failed = Vec::new();
        for (number, result) in &read.replies {
            match result {
                Ok(found) => {
                    replies.extend(found.iter().cloned());
                    done.push(*number);
                }
                Err(why) => failed.push(why.clone()),
            }
        }
        let behind = Timestamp(read.now.0.saturating_sub(SLACK));
        let heard = self.hear_contributions(&read.comments, &replies, &done, behind)?;
        let failed = (!failed.is_empty()).then(|| failed.join("; "));
        if self.threads.replies_failed != failed {
            self.notes.extend(failed.clone());
            self.threads.replies_failed = failed;
        }
        Ok(heard)
    }

    // Answers each ruling its comment's reaction or ticked box answers, when
    // the maintainer it mentions gave it. Their reaction stays on the comment
    // and is read at every poll, so a refusal is told once.
    fn apply_marks(&mut self, marks: BTreeMap<TicketId, Result<CommentMarks, String>>) {
        let mut failed = Vec::new();
        for (id, marks) in marks {
            let marks = match marks {
                Ok(marks) => marks,
                Err(why) => {
                    failed.push(why);
                    continue;
                }
            };
            let Some(ruling) = self.state.rulings.iter().find(|r| r.id == id) else {
                continue;
            };
            let Some(watch) = self.threads.watching.get(&id) else {
                continue;
            };
            let Some(answer) = marked(&ruling.kind, &watch.heard.mentions, &marks) else {
                continue;
            };
            match self.rule_by(id, answer, Settler::Thread) {
                Ok(()) => {
                    self.threads.refused.remove(&id);
                }
                Err(e) => {
                    let why =
                        format!("cannot answer ruling {id} from the reaction on its comment: {e}");
                    if self.threads.refused.insert(id, why.clone()).as_deref() != Some(&why) {
                        self.notes.push(why);
                    }
                }
            }
        }
        // Every comment that could not be read, told together and only when
        // that set changes.
        let failed = (!failed.is_empty()).then(|| failed.join("; "));
        if self.threads.marks_failed != failed {
            self.notes.extend(failed.clone());
            self.threads.marks_failed = failed;
        }
    }

    // Watches each pending ruling's thread, and a ruling that was answered
    // for a while after.
    fn watch_threads(&mut self, now: Timestamp) {
        let state = &self.state;
        let open: Vec<_> = (state.rulings.iter())
            .filter_map(|ruling| watch_of(state, ruling))
            .collect();
        let threads = &mut self.threads.watching;
        for (id, watch) in &open {
            // The place read to is kept in memory too, but the state's wins.
            threads.insert(*id, watch.clone());
        }
        threads.retain(|id, watch| {
            if open.iter().any(|(open, _)| open == id) {
                return true;
            }
            let until = *watch
                .settled
                .get_or_insert(Timestamp(now.0.saturating_add(LATE)));
            now < until
        });
        self.threads.told.retain(|id, _| threads.contains_key(id));
        self.threads
            .decided
            .retain(|id, _| threads.contains_key(id));
        // What a ruling's end said is said by the call that ended it, before
        // any poll, so a ruling still here at a poll ended another way.
        self.threads.ended.clear();
        self.retry_edits(now);
    }

    // Moves the place read to forward to `comment` for each ruling on its
    // thread, and saves it with each pending ruling.
    fn hear(&mut self, comment: &ThreadComment, held: Option<&str>) -> Result<(), StateError> {
        let plain = self.is_plain(comment, &self.threads.aimed_by(comment));
        let mut moved = false;
        for watch in self.threads.watching.values_mut() {
            if watch.thread == comment.thread && watch.heard.before(comment.created, comment.id) {
                watch.heard.time = comment.created;
                watch.heard.comment = comment.id;
                moved = true;
            }
        }
        if !moved && held.is_none() {
            return Ok(());
        }
        let mut next = self.state.clone();
        // The words a comment held are saved with it marked heard, so a kill
        // between the two cannot lose them.
        next.pm_told.extend(held.map(str::to_owned));
        // A reply is the ruling's answer, not a request of the worker. A plain
        // comment is left for the worker's comment round to read.
        if let Some(item) = next
            .work_items
            .iter_mut()
            .filter(|_| !plain)
            .find(|i| i.pull_request == Some(comment.thread))
        {
            item.talked.hear(comment.id);
        }
        for ruling in &self.state.rulings {
            if let Some(watch) = self.threads.watching.get(&ruling.id) {
                next.heard.insert(ruling.id, watch.heard.clone());
            }
        }
        self.save(next)
    }

    // Whether `comment` is the worker's to read: it is neither a number-led
    // reply nor a yes, a no or a rework, and every ruling `aimed` names that
    // still waits is a merge ruling on a pull request, where the maintainer's
    // plain comment is a `no <note>`. With a project manager none is.
    fn is_plain(&self, comment: &ThreadComment, aimed: &[TicketId]) -> bool {
        if self.agents.pm.is_some() {
            return false;
        }
        let waiting: Vec<_> = (self.state.rulings.iter())
            .filter(|r| aimed.contains(&r.id))
            .collect();
        let merges =
            |r: &&Ruling| matches!(r.kind, RulingKind::Merge { .. }) && r.pull_request.is_some();
        !waiting.is_empty() && waiting.iter().all(merges) && !is_reply_words(&comment.body)
    }

    // Acts on a reply by the login a ruling mentions, with write access.
    fn respond(&mut self, comment: &ThreadComment, aimed: &[TicketId]) {
        if self.is_plain(comment, aimed) {
            return;
        }
        let (aim, words) = self.aim(comment, aimed);
        let who = &comment.author;
        let thread = comment.thread;
        match aim {
            Aimed::Nothing => {}
            Aimed::At(id, naming) => self.answer(comment, id, naming, words),
            Aimed::Settled(id) if self.reopens(comment, id, words) => {}
            Aimed::Settled(id) => {
                let (number, told) = match self.threads.told.get(&id) {
                    Some((number, told)) => (*number, told.as_str()),
                    None => (self.ruling_number(id), "is already answered"),
                };
                let said = format!("@{who} Ruling {} {told}.", number.shown());
                self.say(thread, &said);
            }
            Aimed::Undecided(id) => {
                let number = self.ruling_number(id);
                let said = format!(
                    "@{who} Ruling {} was not reopened: start your reply with its number, as \
                     `{number} no <note>`.",
                    number.shown()
                );
                self.say(thread, &said);
            }
            Aimed::Elsewhere(id, there) => {
                let number = self.ruling_number(id);
                let said = format!(
                    "@{who} Ruling {} waits on #{there}, so answer it there.",
                    number.shown()
                );
                self.say(thread, &said);
            }
            // Rulings on one thread share its number, so only an older kelpie
            // leaves several here, and only their ids tell them apart.
            Aimed::Unnamed(ids) => {
                let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
                let said = format!(
                    "@{who} Rulings {} wait on this thread, so start your reply with the id of \
                     the one you answer, such as `{} yes`.",
                    ids.join(", "),
                    ids[0]
                );
                self.say(thread, &said);
            }
        }
    }

    // What `comment` is a reply to, and its words with any number off.
    fn aim<'a>(&self, comment: &'a ThreadComment, aimed: &[TicketId]) -> (Aimed, &'a str) {
        let words = comment.body.trim();
        let (named, words) = self.threads.name_of(words, comment, &self.state.rulings);
        let open = |id: &TicketId| self.state.rulings.iter().any(|r| r.id == *id);
        let waiting: Vec<TicketId> = aimed.iter().copied().filter(open).collect();
        let aimed = match named {
            Some((id, naming)) if waiting.contains(&id) => Aimed::At(id, naming),
            Some((id, _)) => match self.threads.watching.get(&id) {
                Some(Watch {
                    settled: Some(_), ..
                }) => Aimed::Settled(id),
                Some(Watch { thread, .. }) => Aimed::Elsewhere(id, *thread),
                None => Aimed::Nothing,
            },
            None => match waiting.as_slice() {
                [id] => Aimed::At(*id, Naming::Not),
                [] => match self.undecided(aimed, words) {
                    Some(id) => Aimed::Undecided(id),
                    None => Aimed::Nothing,
                },
                several => Aimed::Unnamed(several.to_vec()),
            },
        };
        (aimed, words)
    }

    // The `pm_told` line for `comment` where a project manager is named and
    // it is free text under a pending merge or yes-or-no ruling, which waits.
    fn held_by(&self, comment: &ThreadComment, aimed: &[TicketId]) -> Option<String> {
        self.agents.pm.as_ref()?;
        let (Aimed::At(id, naming), words) = self.aim(comment, aimed) else {
            return None;
        };
        let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
        let stale = naming == Naming::Number
            && ruling.raised.is_some_and(|raised| comment.created < raised);
        (!stale && Wants::of(&ruling.kind) != Wants::Answer && is_free_text(words))
            .then(|| held_line(comment.thread, comment.id, Some(id), words))
    }

    // The number ruling `id` is read by, or `id` once it has left the state
    // and the rulings the project manager answered
    pub(super) fn ruling_number(&self, id: TicketId) -> TicketId {
        let pending = self.state.rulings.iter().find(|r| r.id == id);
        let ended = self.threads.ended.get(&id).map(|e| &e.ruling);
        (pending.or(self.threads.decided.get(&id)).or(ended)).map_or(id, Ruling::number)
    }

    // The ruling the project manager answered that `words`, a `no` naming none, reply to.
    fn undecided(&self, aimed: &[TicketId], words: &str) -> Option<TicketId> {
        let first = words.split_whitespace().next()?;
        if !first.eq_ignore_ascii_case("no") {
            return None;
        }
        (aimed.iter().copied()).find(|id| self.threads.decided.contains_key(id))
    }

    // Answers ruling `id` with `words`, reacting if it took, or says why not.
    // A ruling that takes a yes or a no is answered only by a reply that
    // names it, so a conversational yes never merges.
    fn answer(&mut self, comment: &ThreadComment, id: TicketId, naming: Naming, words: &str) {
        let (who, thread) = (&comment.author, comment.thread);
        let Some(ruling) = self.state.rulings.iter().find(|r| r.id == id) else {
            return;
        };
        let wants = Wants::of(&ruling.kind);
        let stale = naming == Naming::Number
            && ruling.raised.is_some_and(|raised| comment.created < raised);
        let ruled = if stale {
            let asks = ruling.question.lines().next().unwrap_or_default();
            Err(format!(
                "the ruling changed since that comment was written, and it asks now: {asks}"
            ))
        } else if naming != Naming::Not || wants == Wants::Answer {
            // A question's answer may open with the word `pm`.
            let pm = pm_reply(words, ruling.number());
            match pm.filter(|pm| pm.is_ok() || wants != Wants::Answer) {
                Some(Ok(PmReply::Ask(asked))) => return self.pm_asked_by(comment, id, asked),
                Some(Ok(PmReply::Take(n))) => self.approach(id, n).and_then(|answer| {
                    (self.rule_by(id, answer, Settler::Thread)).map_err(|e| e.to_string())
                }),
                Some(Err(why)) => Err(why),
                None => read_answer(wants, words).and_then(|answer| {
                    (self.rule_by(id, answer, Settler::Thread)).map_err(|e| e.to_string())
                }),
            }
        } else {
            Err(format!(
                "start your reply with its number, as {}",
                forms(wants, ruling.number())
            ))
        };
        let said = match ruled {
            Ok(()) => {
                if let Err(e) = self.ports.voice.thumbs_up(&self.remote, comment.id) {
                    self.notes
                        .push(format!("cannot react to {who}'s answer on #{thread}: {e}"));
                }
                return;
            }
            Err(why) => {
                let number = self.ruling_number(id);
                format!(
                    "@{who} Ruling #{number} was not answered: {why}. Reply again in this \
                     thread, starting with `{number}`."
                )
            }
        };
        self.say(thread, &said);
    }

    // Reacts to `comment`, whose words are held for the project manager.
    fn react_held(&mut self, comment: &ThreadComment) {
        if let Err(e) = self.ports.voice.thumbs_up(&self.remote, comment.id) {
            let (who, thread) = (&comment.author, comment.thread);
            self.notes
                .push(format!("cannot react to {who}'s comment on #{thread}: {e}"));
        }
    }

    /// What became of ruling `id` as a reply in its thread is told, if it ended
    pub(super) fn threads_told(&self, id: TicketId) -> Option<&String> {
        self.threads.told.get(&id).map(|(_, told)| told)
    }

    /// Watches the comment that posted the project manager's answer to ruling
    /// `decided`, so `login`'s `no <note>` in `thread` reopens it
    pub(super) fn watch_decided(
        &mut self,
        decided: Ruling,
        thread: u64,
        login: &str,
        posted: crate::ports::PostedComment,
    ) {
        let until = Timestamp(self.ports.clock.now().0.saturating_add(DECIDED));
        let id = decided.id;
        let heard = Heard {
            mentions: login.to_owned(),
            time: posted.created,
            comment: posted.id,
            ruling_comment: 0,
        };
        let watch = Watch {
            ruling: id,
            thread,
            heard,
            settled: Some(until),
        };
        self.threads.watching.insert(id, watch);
        self.threads.decided.insert(id, decided);
    }

    // Reopens a ruling the project manager answered when `words`, a reply to it, are a
    // `no <note>`: it is raised again as it stood, its work item is parked on
    // it again, and the note answers it. Says why when it cannot, and
    // returns whether the ruling was one the project manager answered.
    fn reopens(&mut self, comment: &ThreadComment, id: TicketId, words: &str) -> bool {
        let Some(ruling) = self.threads.decided.get(&id).cloned() else {
            return false;
        };
        let (who, thread) = (&comment.author, comment.thread);
        // A worker's question takes text, so its `no <note>` sends the note.
        let answer = match Wants::of(&ruling.kind) {
            Wants::Answer => (no_note(words).map(Answer::Text))
                .ok_or_else(|| "only a `no <note>` reopens it".to_owned()),
            wants => read_answer(wants, words),
        };
        let reopened = match answer {
            Ok(answer @ (Answer::No(_) | Answer::Rework(_) | Answer::Text(_))) => {
                self.reopen(ruling, answer).map_err(|e| e.to_string())
            }
            Ok(_) => Err("only a `no <note>` reopens it".to_owned()),
            Err(why) => Err(why),
        };
        match reopened {
            Ok(()) => {
                self.threads.decided.remove(&id);
                if let Err(e) = self.ports.voice.thumbs_up(&self.remote, comment.id) {
                    self.notes
                        .push(format!("cannot react to {who}'s answer on #{thread}: {e}"));
                }
            }
            Err(why) => {
                let number = self.ruling_number(id);
                let said = format!("@{who} Ruling #{number} was not reopened: {why}.");
                self.say(thread, &said);
            }
        }
        true
    }

    // Puts `ruling` back where its work item waits on it, and answers it.
    fn reopen(&mut self, ruling: Ruling, answer: Answer) -> Result<(), RuleError> {
        let id = ruling.id;
        let not = |why: &str| RuleError::Reopen(id, why.to_owned());
        let Some(ticket) = ruling.issue else {
            return Err(not("it has no work item"));
        };
        let item = self.state.item(ticket);
        let item = item.ok_or_else(|| not("its work item has finished"))?;
        if self.flights.flying(ticket) {
            return Err(not("its worker is running"));
        }
        if matches!(item.phase, Phase::Ruling { .. } | Phase::Done { .. }) {
            return Err(not("its work item waits on another ruling or is done"));
        }
        let before = self.state.clone();
        let mut next = before.clone();
        if let Some(item) = next.item_mut(ticket) {
            item.phase = Phase::Ruling { id };
        }
        next.rulings.push(ruling);
        self.save(next).map_err(RuleError::State)?;
        let ruled = self.rule_by(id, answer, Settler::Thread);
        if ruled.is_err() {
            // The ruling stays pending when its answer fails, so put the
            // work item back as it was: the reply says it was not reopened.
            if let Err(e) = self.save(before) {
                self.notes.push(format!(
                    "cannot put ruling {id} back after a failed reopen: {e}"
                ));
            }
        }
        ruled
    }

    // Posts `text` on `thread` as the App, and notes why it could not.
    pub(super) fn say(&mut self, thread: u64, text: &str) {
        if let Err(e) = self.ports.voice.comment(&self.remote, thread, text) {
            self.notes.push(format!("cannot reply on #{thread}: {e}"));
        }
    }
}

// The note of a `no <note>`, if `words` are one.
fn no_note(words: &str) -> Option<String> {
    let (first, note) = words.trim().split_once(char::is_whitespace)?;
    let note = note.trim();
    (first.eq_ignore_ascii_case("no") && !note.is_empty()).then(|| note.to_owned())
}

impl Reading {
    // Keeps watching each ruling that `before` had and `after` has not,
    // however it was answered, for a while.
    pub(super) fn settled(&mut self, before: &ProjectState, after: &ProjectState, now: Timestamp) {
        let until = Timestamp(now.0.saturating_add(LATE));
        let gone =
            (before.rulings.iter()).filter(|r| !after.rulings.iter().any(|a| a.is_raised_with(r)));
        for ruling in gone {
            // A ruling posted with no mention has no place read to, only its
            // comment's id.
            let kept = before.heard.get(&ruling.id).map(|h| h.ruling_comment);
            let comment = kept
                .filter(|comment| *comment != 0)
                .or_else(|| before.unmentioned.get(&ruling.id).copied());
            if let Some(comment) = comment {
                let ruling = ruling.clone();
                self.ended.insert(ruling.id, Ended { ruling, comment });
            }
            if let Some((id, watch)) = watch_of(before, ruling) {
                let watch = self.watching.entry(id).or_insert(watch);
                watch.settled.get_or_insert(until);
            }
        }
    }

    // The watched rulings `comment` is new to: on their thread, written after
    // the place read to, by the login each mentions.
    fn aimed_by(&self, comment: &ThreadComment) -> Vec<TicketId> {
        (self.watching.iter())
            .filter(|(_, w)| aims_at(w, comment))
            .map(|(&id, _)| id)
            .collect()
    }

    // The id of a ruling a reply starts with, and the rest of it. The id is
    // the pull request number or ticket id of the one ruling in `rulings`
    // this author may answer, or failing that the id a ruling was lately
    // named by, waiting or settled, so a late answer is told so.
    fn name_of<'a>(
        &self,
        words: &'a str,
        comment: &ThreadComment,
        rulings: &[Ruling],
    ) -> (Option<(TicketId, Naming)>, &'a str) {
        let Some((first, rest)) = words.split_once(char::is_whitespace) else {
            return (None, words);
        };
        let Ok(number) = first.trim_start_matches('#').parse::<TicketId>() else {
            return (None, words);
        };
        if rest.trim().is_empty() {
            return (None, words);
        }
        let mentioned = |id: &TicketId| {
            (self.watching.get(id)).is_some_and(|w| w.heard.is_mentioned(&comment.author))
        };
        let rest = rest.trim_start();
        let about = |r: &&Ruling| {
            r.issue == Some(number)
                || r.pull_request.map(TicketId::from_pull_request) == Some(number)
        };
        // A ruling that takes any text is answered by a reply that names no
        // ruling when it is the only one waiting, so a number opening the
        // reply is part of the answer, not its name.
        let named_by_number = |r: &&Ruling| Wants::of(&r.kind) != Wants::Answer;
        let takes_text = |r: &Ruling| r.id == number && !named_by_number(&r);
        let mut about = (rulings.iter())
            .filter(about)
            .filter(named_by_number)
            .filter(|r| mentioned(&r.id));
        match (about.next(), about.next()) {
            (Some(ruling), None) => (Some((ruling.id, Naming::Number)), rest),
            _ if rulings.iter().any(takes_text) => (None, words),
            _ if mentioned(&number) => (Some((number, Naming::Number)), rest),
            _ => (None, words),
        }
    }
}

/// Whether `body` opens as a reply to a ruling does: with a number or an
/// answer word
pub(super) fn is_reply_words(body: &str) -> bool {
    let first = body.split_whitespace().next().unwrap_or_default();
    let first = first.trim_start_matches('#');
    first.parse::<TicketId>().is_ok()
        || ["yes", "no", "rework"]
            .iter()
            .any(|word| first.eq_ignore_ascii_case(word))
}

// Whether `comment` is new to the ruling `watch` follows, and from the login
// it mentions.
fn aims_at(watch: &Watch, comment: &ThreadComment) -> bool {
    watch.thread == comment.thread
        && watch.heard.before(comment.created, comment.id)
        && watch.heard.is_mentioned(&comment.author)
}

// The answer a ruling's comment gives of itself: the thumbs the login it
// mentions left, else a box ticked in it. A thumbs up with a thumbs down
// answers nothing. A tick counts only when every edit of the comment was by
// that login or by the App that wrote it.
fn marked(kind: &RulingKind, maintainer: &str, marks: &CommentMarks) -> Option<Answer> {
    let thumbs = |up: bool| {
        (marks.thumbs.iter())
            .any(|thumb| thumb.up == up && thumb.login.eq_ignore_ascii_case(maintainer))
    };
    match (thumbs(true), thumbs(false)) {
        (true, true) => return None,
        (true, false) => {
            if let Some(answer) = voice::thumbs(kind, true) {
                return Some(answer);
            }
        }
        (false, true) => {
            if let Some(answer) = voice::thumbs(kind, false) {
                return Some(answer);
            }
        }
        (false, false) => {}
    }
    let own = |login: &str| plain_login(login) == plain_login(&marks.author);
    let theirs = |login: &String| login.eq_ignore_ascii_case(maintainer) || own(login);
    let edited = !marks.editors.is_empty() && marks.editors.iter().all(theirs);
    edited.then(|| voice::ticked(kind, &marks.body)).flatten()
}

// GraphQL names an App's login without the `[bot]` REST adds.
pub(in crate::runner) fn plain_login(login: &str) -> String {
    let login = login.strip_suffix("[bot]").unwrap_or(login);
    login.to_ascii_lowercase()
}

// What a reply to a ruling that wants `wants` starts with: its number.
fn forms(wants: Wants, id: TicketId) -> String {
    match wants {
        Wants::Answer => format!("`{id} <your answer>`"),
        Wants::Merge => format!("`{id} yes`, `{id} no <note>` or `{id} rework <note>`"),
        Wants::YesOrNo => format!("`{id} yes` or `{id} no <note>`"),
    }
}

// The thread a pending ruling is answered in, and the place read to there.
// A ruling kelpie's App did not post with a mention is answered at the
// terminal only, so it has none.
fn watch_of(state: &ProjectState, ruling: &Ruling) -> Option<(TicketId, Watch)> {
    let thread = ruling
        .pull_request
        .or_else(|| ruling.issue.and_then(|t| t.github()))?;
    let heard = state.heard.get(&ruling.id)?.clone();
    let settled = None;
    Some((
        ruling.id,
        Watch {
            ruling: ruling.id,
            thread,
            heard,
            settled,
        },
    ))
}

mod settled;

#[cfg(test)]
mod tests;
