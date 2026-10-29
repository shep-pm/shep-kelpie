//! The limits kelpie puts on triage, checked before an issue's research call
//!
//! Four checks, in order, none a model call: the day's calls, the author's share
//! of them, the age of the author's account, and the size of the body. The
//! maintainer's own issues and one `shep kelpie triage` asked for skip all four.
//! The first holds an issue in `triage.due` for a free place. The next two end it
//! at `needs-triage` with no call and no comment, final until the verb is asked.
//! The last cuts the body the call is given, and says in the fence that it did.
//! Counts come from the calls saved in `ProjectState.triage`, so a cap survives a
//! restart; a call on an issue with no record yet is saved under `triage.unrecorded`.

use crate::board::TRIAGE;
use crate::ports::{OpenIssue, Timestamp};
use crate::state::{StateError, TriageHeld, TriageStep, Triaged};

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::DueStatus;
use super::comment::NEEDS_TRIAGE;

/// The window both daily caps count, in seconds
pub(in crate::runner) const DAY: u64 = 24 * 60 * 60;

/// What a limit says about an issue before its call
pub(in crate::runner) enum Held {
    /// The day's places are taken. The issue waits in `triage.due` for the next
    /// one, and is never dropped.
    Day,
    /// The issue gets `needs-triage` with no call and no comment, and the pass
    /// does not come back to it
    Final(TriageHeld),
}

/// The body as a research call is given it, and the line that says it was cut.
/// A body past `max_kb` kilobytes is cut at that size, on no character, with a
/// line naming the size the whole body was. 0 sends the whole body, and the
/// title is never cut.
pub(in crate::runner) fn cut_body(body: &str, max_kb: u32) -> (String, String) {
    let most = usize::try_from(max_kb)
        .ok()
        .and_then(|kb| kb.checked_mul(1024))
        .unwrap_or(usize::MAX);
    if max_kb == 0 || body.len() <= most {
        return (body.to_owned(), String::new());
    }
    let mut end = most;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    let whole = body.len().div_ceil(1024);
    let note = format!(
        "\n\n[Kelpie cut the issue's body here: it is {whole} KB, and this call is given at \
         most {max_kb} KB of it.]"
    );
    (body[..end].to_owned(), note)
}

impl Runner {
    // Whether the four limits reach an issue at all. The maintainer's own
    // issues and one `shep kelpie triage` asked for skip all of them, and a
    // maintainer that cannot be named is no one's, so nothing is skipped.
    pub(super) fn triage_limited(&self, author: &str, asked: bool) -> bool {
        !asked && !self.triage_is_maintainer(author)
    }

    fn triage_is_maintainer(&self, login: &str) -> bool {
        self.own_author(login)
    }

    // A record made before the caps existed, or from a triage comment found on the
    // issue, names no author. The listing gives it: the author's cap counts by it
    // and it tells the maintainer's issues, which skip every limit. An account
    // deleted since leaves none, and the record stays as it is.
    pub(super) fn note_triage_authors(&mut self, open: &[OpenIssue]) -> Result<(), StateError> {
        let Some(triage) = self.state.triage.as_ref() else {
            return Ok(());
        };
        if (triage.issues.iter()).all(|record| !record.author.is_empty()) {
            return Ok(());
        }
        let mut next = self.state.clone();
        let mut filled = 0;
        if let Some(triage) = next.triage.as_mut() {
            for record in &mut triage.issues {
                if !record.author.is_empty() {
                    continue;
                }
                if let Some(issue) = (open.iter()).find(|i| i.number == record.issue)
                    && !issue.author.is_empty()
                {
                    record.author = issue.author.clone();
                    filled += 1;
                }
            }
        }
        if filled == 0 {
            return Ok(());
        }
        self.save(next)
    }

    // What the day's records say: how many triage calls fall in the 24 hours
    // back from `now`, and when the next place opens once they fill
    // `triage.per_day`. A record a limit held stands for no call, so it spends
    // no place.
    pub(super) fn triage_day(&self, now: Timestamp) -> (u32, Option<Timestamp>) {
        let (used, oldest) = self.triage_count(now, None);
        let cap = self.settings.triage.per_day;
        let opens = (cap > 0 && used >= cap)
            .then(|| oldest.map(|at| Timestamp(at.0.saturating_add(DAY))))
            .flatten();
        (used, opens)
    }

    // How many of the day's calls belong to `login`, logins compared ignoring
    // case. An author the listing gives no login for, which is what an account
    // deleted since the issue went up leaves, is counted with no one: their
    // records share no bucket, so they cannot spend each other's allowance.
    fn triage_author_calls(&self, login: &str, now: Timestamp) -> u32 {
        if login.is_empty() {
            return 0;
        }
        self.triage_count(now, Some(login)).0
    }

    // The day's triage calls, and the oldest: every call saved in records whose
    // moment falls in the 24 hours back from `now`, and every call saved for an
    // issue with no record yet. With `login`, only that author's. A record a limit
    // held, or made from a comment found on the issue, holds no call.
    fn triage_count(&self, now: Timestamp, login: Option<&str>) -> (u32, Option<Timestamp>) {
        let from = now.0.saturating_sub(DAY);
        let Some(triage) = self.state.triage.as_ref() else {
            return (0, None);
        };
        let mut used = 0;
        let mut oldest = None;
        let mut count = |at: &Timestamp| {
            if at.0 <= from {
                return;
            }
            used += 1;
            oldest = Some(oldest.map_or(*at, |old: Timestamp| old.min(*at)));
        };
        for record in &triage.issues {
            if record.held.is_some()
                || !login.is_none_or(|login| record.author.eq_ignore_ascii_case(login))
            {
                continue;
            }
            for at in &record.calls {
                count(at);
            }
        }
        // A call with no record to carry it is kept against the issue until a
        // record takes it, so the caps see the ones a first pass made and never
        // posted, across a restart as well.
        for made in &triage.unrecorded {
            if (triage.issues.iter()).any(|record| record.issue == made.issue)
                || !login.is_none_or(|login| made.author.eq_ignore_ascii_case(login))
            {
                continue;
            }
            for at in &made.calls {
                count(at);
            }
        }
        (used, oldest)
    }

    // Records that a triager call on `issue` ran, at the moment it ended, so the
    // next poll and a restart see the place it spent and a round's second call is
    // counted before it is admitted. A first pass has no record until its comment
    // is recorded, so its call is saved against the issue until then.
    pub(in crate::runner) fn triage_call_made(&mut self, issue: u64) -> Result<(), StateError> {
        let now = self.ports.clock.now();
        let author = self
            .triage_tries
            .get(&issue)
            .and_then(|tries| tries.author.clone())
            .unwrap_or_default();
        let mut next = self.state.clone();
        let Some(triage) = next.triage.as_mut() else {
            return Ok(());
        };
        if (triage.issues.iter()).any(|record| record.issue == issue) {
            let (_, made) = triage.take_calls(issue);
            if let Some(record) = triage.issues.iter_mut().find(|t| t.issue == issue) {
                for at in made.into_iter().chain([now]) {
                    record.add_call(at);
                }
            }
        } else {
            triage.record_call(issue, &author, now);
        }
        self.save(next)
    }

    // What the limits that hold an issue say about it, in their order, or none
    // when it may have its call. The day's cap first, since it asks nothing of
    // the forge, then the author's, then the account's age. The caller has
    // already said the limits reach the issue.
    pub(super) fn triage_held(&mut self, due: &DueStatus) -> Option<Held> {
        let now = self.ports.clock.now();
        let per_day = self.settings.triage.per_day;
        if per_day > 0 && self.triage_day(now).0 >= per_day {
            return Some(Held::Day);
        }
        // The author's cap and the account's age decide which of an author's
        // issues kelpie takes on. They reach an issue kelpie has already called
        // on no more than the account's age does: a hold is final, and calls
        // that came to nothing are no reason to label an issue over for good.
        if self.triage_spent(due.issue) {
            return None;
        }
        let per_author = self.settings.triage.per_author_per_day;
        if per_author > 0 && self.triage_author_calls(&due.author, now) >= per_author {
            return Some(Held::Final(TriageHeld::AuthorCap));
        }
        if self.triage_new_account(due, now) {
            return Some(Held::Final(TriageHeld::NewAccount));
        }
        None
    }

    // Whether `issue` has spent a triage call of its own: the ones its record
    // keeps, or the ones saved for it while it has no record.
    fn triage_spent(&self, issue: u64) -> bool {
        (self.triage_record(issue)).is_some_and(|record| !record.calls.is_empty())
            || (self.state.triage.as_ref()).is_some_and(|triage| {
                (triage.unrecorded.iter()).any(|made| made.issue == issue && !made.calls.is_empty())
            })
    }

    // Whether a call of the author's round waits for a free place: the day's cap
    // alone, as the author's share and the account's age decide which issues kelpie
    // takes on and this one is taken already. A round is read from the records at
    // every poll, so a call that waits is simply made at the next one.
    pub(super) fn triage_round_held(&mut self) -> bool {
        let now = self.ports.clock.now();
        let per_day = self.settings.triage.per_day;
        per_day > 0 && self.triage_day(now).0 >= per_day
    }

    // Whether the author's account is younger than `triage.min_account_days`
    // and the forge counts them as having no history in the repo. An account
    // whose age cannot be read, and an author the listing gives no login for,
    // are held by nothing.
    fn triage_new_account(&mut self, due: &DueStatus, now: Timestamp) -> bool {
        let youngest = self.settings.triage.min_account_days;
        if youngest == 0 {
            return false;
        }
        let no_history = due.author_association.is_some_and(|a| a.no_history());
        if !no_history {
            return false;
        }
        match self.triage_account_created(&due.author) {
            Some(at) => now.0.saturating_sub(at.0) < u64::from(youngest).saturating_mul(DAY),
            None => false,
        }
    }

    // When the account `login` was created, read once per author per runner
    // start, and `None` when the forge cannot say. An unreadable age is kept as
    // unreadable, so a poll does not ask again.
    pub(super) fn triage_account_created(&mut self, login: &str) -> Option<Timestamp> {
        if login.is_empty() {
            return None;
        }
        let key = login.to_lowercase();
        if let Some(age) = self.triage_run.accounts.get(&key) {
            return *age;
        }
        let age = (self.ports.forge.account_created(login)).ok();
        self.triage_run.accounts.insert(key, age);
        age
    }

    // Labels `issue` `needs-triage` with no call and no comment, and records that
    // `limit` held it. The hold is final: its label and record keep it out of the
    // due list, and `shep kelpie triage` can still be asked of it. A label the forge
    // refuses is a failed triage, tried at the next poll, and nothing is recorded.
    pub(super) fn triage_hold(
        &mut self,
        issue: u64,
        author: &str,
        limit: TriageHeld,
    ) -> Result<(), StateError> {
        if let Err(why) = self.make_missing_labels([NEEDS_TRIAGE]) {
            self.triage_failed(issue, &format!("cannot make `{TRIAGE}`: {why}"), true);
            return Ok(());
        }
        if let Err(e) = self.label_issue(issue, TRIAGE, true) {
            let why = format!("cannot label it `{TRIAGE}`: {e}");
            self.triage_failed(issue, &why, !forge_is_busy(&e));
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(triage) = next.triage.as_mut() {
            triage.issues.retain(|t| t.issue != issue);
            triage.issues.push(Triaged {
                issue,
                comment: 0,
                step: TriageStep::Done,
                at: self.ports.clock.now(),
                author: author.to_owned(),
                held: Some(limit),
                calls: Vec::new(),
                labels: Vec::new(),
                duplicate_of: None,
                ready_body: None,
                seen: Vec::new(),
                asked: None,
                questions: None,
                local: None,
                spam: None,
                begun: None,
            });
            triage.prune();
        }
        self.save(next)?;
        self.triage_due.retain(|d| d.issue != issue);
        self.triage_queue.retain(|&n| n != issue);
        self.triage_tries.remove(&issue);
        self.notes.push(format!(
            "triage #{issue}: {}, so it is labelled `{TRIAGE}` with no call and no comment",
            limit.said()
        ));
        Ok(())
    }
}
