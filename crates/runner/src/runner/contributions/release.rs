//! Asking the contributor for a release
//!
//! Kelpie finishes a contribution only after its contributor says it may or
//! goes quiet after being asked (ADR 0010); the quiet clock drives each step.
//! At the nudge the representative asks, in one comment, whether kelpie may take
//! the pull request; at the quiet with no ask it posts the same and restarts the
//! clock. An ask's notice runs from the ask itself, late or not. A reply is read
//! as `yes`, `no` or `other` (the last goes to [`answer`](super::answer)), and a
//! thumbs up or down on the ask counts alike. A yes, or no answer by the quiet,
//! is a release. A no restarts the clock; a second no then quiet raises a ruling.

use super::super::Runner;
use super::super::representative::Post;
use super::super::representative::reading::{self, Read};
use super::answer::said_key;
use super::clock::{Clock, Due};
use super::post::{forge_is_busy, short};
use super::ruling::{flagged_lines, raise_waiting};
use crate::state::{Asked, Contribution, How, KEPT, Released, Said, StateError};

/// What kind of post the representative is asked for
const ASK: &str = "Ask the contributor whether kelpie may take their pull request from here, and \
    remind them of the review bot threads still open. Friendly and short. Tell them they can \
    answer in a comment or with a thumbs up (yes) or a thumbs down (no) on this comment. Never \
    promise or hint at a merge: only the maintainer's ruling decides one.";

const READ: &str = "The contributor replied to kelpie's question whether it may take their pull \
    request from here. Use yes only when they clearly let kelpie take it over, no only when they \
    clearly want to keep it or turn the offer down, and other for anything else, a question \
    included.";

/// The words a reply to the release ask is read as, beside `other`
const WORDS: [&str; 2] = ["yes", "no"];

// What the representative is asked to do with a reply to the release ask
fn read_kind() -> String {
    reading::kind(READ, &WORDS)
}

const RELEASED: &str = "Tell the contributor that their pull request is kelpie's now and that \
    kelpie will finish it from here, and that their commits and credit stay. Short. Never promise \
    or hint at a merge: only the maintainer's ruling decides one.";

const THANKS: &str = "Thank the contributor for their pull request and tell them kelpie is \
    closing it. Short and kind. Blame nothing on them, and promise nothing.";

// What the contributor answered, with the reply it came from if it was one
enum Told {
    Yes(How, Option<Said>),
    No(Option<Said>),
}

// What looking for the contributor's answer came to
enum Look {
    // An answer
    Told(Told),
    // Nothing new
    Nothing,
    // Something could not be read, so nothing is concluded from silence
    Stuck,
}

// A reaction on the ask
enum Reaction {
    Up,
    Down,
    Nothing,
    Unread,
}

impl Runner {
    // Asks each contribution's contributor for a release when its clock is
    // due, takes their answer, and carries out the maintainer's choice to
    // close.
    pub(super) fn follow_release(&mut self) -> Result<(), StateError> {
        if self.settings.contributions.is_off() || !self.app_covers() {
            return Ok(());
        }
        let numbers: Vec<u64> = (self.state.contributions.iter())
            .map(|c| c.pull_request)
            .collect();
        for number in numbers {
            self.release_one(number)?;
        }
        Ok(())
    }

    fn release_one(&mut self, number: u64) -> Result<(), StateError> {
        let Some(held) = (self.state.contributions.iter())
            .find(|c| c.pull_request == number)
            .cloned()
        else {
            return Ok(());
        };
        // A ruling on the pull request holds its calls, and the one for a
        // contributor who said no twice is that ruling.
        if held.released.is_some() || self.ruling_waits(number) {
            return Ok(());
        }
        if held.asked.as_ref().is_some_and(|a| a.closing) {
            return self.close_with_thanks(&held);
        }
        let now = self.ports.clock.now();
        let Some(clock) = Clock::of(&held, &self.settings.contributions) else {
            return Ok(());
        };
        let due = clock.due(now);
        match &held.asked {
            None => match due {
                Some(due) => self.ask(&held, &clock, due == Due::Quiet),
                None => Ok(()),
            },
            Some(asked) if asked.is_open() => match self.look_for_answer(&held, asked)? {
                Look::Told(Told::Yes(how, said)) => self.release_to(&held, how, said),
                Look::Told(Told::No(said)) => self.record_no(number, said),
                // Kelpie takes a pull request over by pushing to its branch,
                // so a silence on one it cannot push to is the maintainer's
                // to decide, after the replies are read.
                Look::Nothing if due == Some(Due::Quiet) => match self.awaits_edits(number)? {
                    true => Ok(()),
                    false => self.release_to(&held, How::Silence, None),
                },
                Look::Nothing | Look::Stuck => Ok(()),
            },
            Some(asked) if due == Some(Due::Quiet) && asked.nos >= 2 => {
                let now = self.ports.clock.now();
                let mut next = self.state.clone();
                raise_waiting(&mut next, number, now);
                self.save(next)
            }
            Some(_) if due == Some(Due::Quiet) => self.ask(&held, &clock, true),
            Some(_) => Ok(()),
        }
    }

    // Whether a call may start on the pull request: no ruling holds it and
    // its spend is under the cap
    fn can_call(&mut self, number: u64) -> Result<bool, StateError> {
        Ok(!self.ruling_waits(number) && !self.over_budget(number)?)
    }

    // The contributor's answer to the latest ask: a reaction first, then
    // their replies since, oldest first. A reply read as neither yes nor no is
    // kept so it is not read again, and left for the conversation.
    fn look_for_answer(&mut self, held: &Contribution, asked: &Asked) -> Result<Look, StateError> {
        match self.reaction(held, asked) {
            Reaction::Up => return Ok(Look::Told(Told::Yes(How::ThumbsUp, None))),
            Reaction::Down => return Ok(Look::Told(Told::No(None))),
            Reaction::Unread => return Ok(Look::Stuck),
            Reaction::Nothing => {}
        }
        let number = held.pull_request;
        for said in replies_since(held, asked) {
            if !self.can_call(number)? {
                return Ok(Look::Stuck);
            }
            let Some(read) = self.read_reply(held, asked, &said)? else {
                return Ok(Look::Stuck);
            };
            // The words it was given are `yes` and `no`; anything the reading
            // returns beside them is `other`.
            match read.word() {
                "yes" => return Ok(Look::Told(Told::Yes(How::Yes, Some(said)))),
                "no" => return Ok(Look::Told(Told::No(Some(said)))),
                _ => self.keep_read(number, said)?,
            }
        }
        Ok(Look::Nothing)
    }

    // What the contributor left on the ask: a thumbs down wins over a thumbs
    // up. Only their own count.
    fn reaction(&self, held: &Contribution, asked: &Asked) -> Reaction {
        let number = held.pull_request;
        let key = format!("react:{number}:{}", asked.comment);
        match self.ports.forge.comment_marks(&self.remote, asked.comment) {
            Ok(marks) => {
                self.answer_ends(&key);
                let theirs: Vec<bool> = (marks.thumbs.iter())
                    .filter(|t| t.login.eq_ignore_ascii_case(&held.author))
                    .map(|t| t.up)
                    .collect();
                match (theirs.contains(&false), theirs.contains(&true)) {
                    (true, _) => Reaction::Down,
                    (false, true) => Reaction::Up,
                    (false, false) => Reaction::Nothing,
                }
            }
            Err(e) => {
                self.answer_failed(&key, number, "read the reaction of", &e, false);
                Reaction::Unread
            }
        }
    }

    // Reads `said`, a reply to the ask, with one representative call, or
    // `None` when it could not be had, told in the log and tried at the next
    // poll. What the call cost is added to the spend whatever it read.
    fn read_reply(
        &mut self,
        held: &Contribution,
        asked: &Asked,
        said: &Said,
    ) -> Result<Option<Read>, StateError> {
        let number = held.pull_request;
        let key = read_key(number, said);
        if let Some(kept) = self.kept_answer(&key) {
            return Ok(Read::of(&kept, &WORDS));
        }
        // A reply that cannot be read after every try is left to the
        // conversation, so it does not hold the release for ever.
        if self.answer_given_up(&key) {
            return Ok(Some(Read::Other));
        }
        let wrote = match self.said_text(number, said) {
            Ok(wrote) => wrote,
            Err(why) => {
                self.answer_failed(&key, number, "read the reply of", &why, true);
                return Ok(None);
            }
        };
        let facts = format!(
            "Pull request #{number} by @{}. Kelpie asked its contributor whether it may take the \
             pull request from here, in the comment given as your earlier post. The contributor \
             replied.\n",
            held.author
        );
        let kind = read_kind();
        let post = Post {
            pull_request: number,
            kind: &kind,
            facts: &facts,
            earlier: &[asked.comment],
            contributor: &held.author,
            said: &wrote,
        };
        let (words, cost) = match self.represent_priced(&post) {
            Ok(done) => done,
            Err(e) => {
                self.answer_failed(&key, number, "read the reply of", &e, true);
                return Ok(None);
            }
        };
        self.add_spend(number, cost)?;
        match reading::read(&words, &WORDS) {
            Some(reading) => {
                self.keep_answer(&key, reading.word());
                Ok(Some(reading))
            }
            None => {
                let why = "its reading is not the JSON asked for";
                self.answer_failed(&key, number, "read the reply of", &why, true);
                Ok(None)
            }
        }
    }

    // Remembers that `said` was read as neither yes nor no
    fn keep_read(&mut self, number: u64, said: Said) -> Result<(), StateError> {
        let key = read_key(number, &said);
        self.change(number, |held| {
            if let Some(asked) = held.asked.as_mut() {
                asked.read.push(said);
                let over = asked.read.len().saturating_sub(KEPT);
                asked.read.drain(..over);
            }
        })?;
        self.answer_ends(&key);
        Ok(())
    }

    // Posts the ask, and records it. `quiet` is whether the contributor has
    // been quiet the whole window: the clock starts over so they have one. The
    // days it states run from the ask itself.
    fn ask(&mut self, held: &Contribution, clock: &Clock, quiet: bool) -> Result<(), StateError> {
        let number = held.pull_request;
        let (asks, nos) = held.asked.as_ref().map_or((1, 0), |a| (a.asks + 1, a.nos));
        let key = format!("ask:{number}:{asks}");
        let now = self.ports.clock.now();
        let id = match self.sent_before(&key) {
            Some(id) => id,
            None => {
                let days = match quiet {
                    true => clock.quiet_days(),
                    false => clock.notice_days(),
                };
                let facts = self.ask_facts(held, asks, days);
                match self.word_and_post(held, ASK, &facts, &key, "ask")? {
                    Some(id) => id,
                    None => return Ok(()),
                }
            }
        };
        let change = |held: &mut Contribution| {
            held.asked = Some(Asked {
                comment: id,
                at: now,
                asks,
                nos,
                read: Vec::new(),
                closing: false,
                thanked: false,
            });
            if quiet {
                held.waiting_since = Some(now);
            }
        };
        self.keep_record(number, &key, Some(id), change)
    }

    // What the ask may state
    fn ask_facts(&self, held: &Contribution, asks: u32, days: u64) -> String {
        let mut facts = format!(
            "Pull request #{} by @{}, at {}.\n\
             Kelpie asks whether it may take the pull request from here: it would push to the \
             branch itself to finish the work, and the contributor's commits and credit stay.\n",
            held.pull_request,
            held.author,
            short(&held.head)
        );
        facts.push_str(match asks {
            1 => "This is the first time kelpie asks.\n",
            _ => "Kelpie asked before and was told no. This is the last time it asks.\n",
        });
        let plural = if days == 1 { "" } else { "s" };
        facts.push_str(&format!(
            "If the contributor says nothing for {days} more day{plural}, kelpie takes that as a \
             yes.\n"
        ));
        let threads = self.open_threads(held);
        match threads.is_empty() {
            true => facts.push_str("No review bot thread is open.\n"),
            false => facts.push_str(&format!(
                "The review bot threads still open on this head:\n{threads}\n"
            )),
        }
        facts
    }

    // The open threads of a review bot on the current head, findings and
    // nits, one to a line. A local reviewer's findings are its own review and
    // not named.
    pub(super) fn open_threads(&self, held: &Contribution) -> String {
        let Some(pass) = held.pass.as_ref().filter(|p| p.head == held.head) else {
            return String::new();
        };
        let by_bot = (pass.answered.as_ref())
            .and_then(|a| self.contribution_lineup.iter().find(|r| &r.name == a))
            .is_some_and(|r| r.bot().is_some());
        match by_bot {
            true => {
                let all: Vec<_> = pass.findings.iter().chain(&pass.nits).cloned().collect();
                flagged_lines(&all)
            }
            false => String::new(),
        }
    }

    // Words `kind` from `facts` for the pull request and posts it, returning
    // the id the forge gave, or `None` when it could not be, told in the log
    // and tried at the next poll. A wording kept from a try that failed to
    // post is sent again.
    pub(super) fn word_and_post(
        &mut self,
        held: &Contribution,
        kind: &str,
        facts: &str,
        key: &str,
        what: &str,
    ) -> Result<Option<u64>, StateError> {
        let number = held.pull_request;
        if self.answer_given_up(key) {
            return Ok(None);
        }
        let text = match self.kept_answer(key) {
            Some(text) => text,
            None => {
                if !self.can_call(number)? {
                    return Ok(None);
                }
                let post = Post {
                    pull_request: number,
                    kind,
                    facts,
                    earlier: &[],
                    contributor: &held.author,
                    said: "",
                };
                match self.represent_priced(&post) {
                    Ok((words, cost)) => {
                        self.add_spend(number, cost)?;
                        self.keep_answer(key, &words);
                        words
                    }
                    Err(e) => {
                        self.answer_failed(key, number, what, &e, true);
                        return Ok(None);
                    }
                }
            }
        };
        Ok(self.send_answer(number, key, &text, what))
    }

    // Records the release and has the representative say it. A line that
    // cannot be posted, after `POST_TRIES` tries, does not hold the release.
    fn release_to(
        &mut self,
        held: &Contribution,
        how: How,
        said: Option<Said>,
    ) -> Result<(), StateError> {
        let number = held.pull_request;
        let key = format!("release:{number}");
        let posted = match self.sent_before(&key) {
            Some(id) => Some(id),
            None => {
                let facts = format!(
                    "Pull request #{number} by @{}. {}\n",
                    held.author,
                    match how {
                        How::Yes => "The contributor said kelpie may take it from here.",
                        How::ThumbsUp => "The contributor gave kelpie's question a thumbs up.",
                        How::Silence => {
                            "The contributor did not answer kelpie's question in time, which \
                             kelpie takes as a yes."
                        }
                        How::Maintainer | How::Rework => "The maintainer decided kelpie takes it.",
                    }
                );
                self.word_and_post(held, RELEASED, &facts, &key, "tell")?
            }
        };
        if posted.is_none() && !self.answer_given_up(&key) {
            return Ok(());
        }
        let now = self.ports.clock.now();
        let read = said.as_ref().map(|said| read_key(number, said));
        self.keep_record(number, &key, posted, |held| {
            held.released = Some(Released { at: now, how });
            if let Some(said) = said {
                held.answer(said);
            }
        })?;
        if let Some(read) = read {
            self.answer_ends(&read);
        }
        Ok(())
    }

    // Records a no: the clock starts over, and the reply is dealt with
    fn record_no(&mut self, number: u64, said: Option<Said>) -> Result<(), StateError> {
        let now = self.ports.clock.now();
        let read = said.as_ref().map(|said| read_key(number, said));
        self.change(number, |held| {
            if let Some(asked) = held.asked.as_mut() {
                asked.nos += 1;
            }
            held.waiting_since = Some(now);
            if let Some(said) = said {
                held.answer(said);
            }
        })?;
        if let Some(read) = read {
            self.answer_ends(&read);
        }
        Ok(())
    }

    // Thanks the contributor and closes the pull request, as the maintainer
    // chose. The thanks is posted once, and saved as posted, so a close that
    // fails is tried again alone, a restart included.
    fn close_with_thanks(&mut self, held: &Contribution) -> Result<(), StateError> {
        let number = held.pull_request;
        let key = format!("thanks:{number}");
        if self.answer_given_up(&key) {
            return Ok(());
        }
        let thanked = held.asked.as_ref().is_some_and(|a| a.thanked);
        if !thanked {
            let posted = match self.sent_before(&key) {
                Some(id) => id,
                None => {
                    let facts = format!(
                        "Pull request #{number} by @{}. The maintainer decided to close it \
                         without a merge.\n",
                        held.author
                    );
                    match self.word_and_post(held, THANKS, &facts, &key, "thank")? {
                        Some(id) => id,
                        None => return Ok(()),
                    }
                }
            };
            self.keep_record(number, &key, Some(posted), |held| {
                if let Some(asked) = held.asked.as_mut() {
                    asked.thanked = true;
                }
            })?;
        }
        if let Err(e) = self.ports.forge.close_pull_request(&self.remote, number) {
            let counts = !forge_is_busy(&e);
            self.answer_failed(&key, number, "close the pull request of", &e, counts);
        }
        Ok(())
    }

    // Changes the contribution for `number` in the state
    pub(super) fn change(
        &mut self,
        number: u64,
        change: impl FnOnce(&mut Contribution),
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        if let Some(held) = (next.contributions.iter_mut()).find(|c| c.pull_request == number) {
            change(held);
        }
        self.save(next)
    }

    // Records the change after the forge took a post, `posted` its id. A save
    // that fails leaves the id to be recorded at the next poll, so the post is
    // not made twice.
    pub(super) fn keep_record(
        &mut self,
        number: u64,
        key: &str,
        posted: Option<u64>,
        change: impl FnOnce(&mut Contribution),
    ) -> Result<(), StateError> {
        match self.change(number, change) {
            Ok(()) => {
                self.answer_ends(key);
                Ok(())
            }
            Err(e) => {
                if let Some(id) = posted {
                    let mut tries = self.post_tries.borrow_mut();
                    tries.entry(key.to_owned()).or_default().sent = Some(id);
                }
                Err(e)
            }
        }
    }
}

// What the contributor wrote since the ask that nothing has taken up and that
// was not read as neither yes nor no: comments and review replies, oldest
// first
fn replies_since(held: &Contribution, asked: &Asked) -> Vec<Said> {
    let known = |list: &[Said], said: &Said| list.iter().any(|k| said_key(k) == said_key(said));
    (held.unanswered().into_iter())
        .filter(|said| said.at() >= asked.at && !known(&asked.read, said))
        .collect()
}

/// Whether `said` is a reply to the open ask that the release stage has yet to
/// read, so the conversation leaves it alone: a yes or a no is never answered
/// in public
pub(super) fn awaits_release(held: &Contribution, said: &Said) -> bool {
    held.released.is_none()
        && (held.asked.as_ref())
            .is_some_and(|a| a.is_open() && !a.closing && replies_since(held, a).contains(said))
}

fn read_key(number: u64, said: &Said) -> String {
    format!("read:{number}:{}", said_key(said))
}
