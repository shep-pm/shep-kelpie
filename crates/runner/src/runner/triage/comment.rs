//! The comment that says what the research found, and the labels after it
//!
//! The representative words the facts, kelpie's code appends [`MARKER`] and
//! posts the comment as the App, and then puts the labels on. A maintainer's
//! issue whose rewrite the issue check passes takes the ready path ([`ready`]);
//! any other ends `needs-triage`. The record in `triage.issues` is saved before
//! each next write to the forge, so a restart posts nothing twice; a post the
//! forge took whose record did not save is caught by the App's marker. A failed
//! step is logged and tried at the next poll, and five failures give the issue
//! up until kelpie restarts.

use super::super::Runner;
use super::super::contributions::post::forge_is_busy;
use super::super::representative::IssuePost;
use super::addressed::Cover;
use super::ask::{self, Purpose};
use super::facts::{Reader, Wording, duplicate_facts, facts};
use super::marker::{MARKER, duplicate_marker};
use super::ready;
use super::research::{Addressed, Found, Question};
use crate::board::{self, TRIAGE};
use crate::local_paths::Leak;
use crate::ports::{NewLabel, Timestamp};
use crate::settings::AuthorRole;
use crate::state::{CheckTriage, StateError, TriageLocal, TriageQuestions, TriageStep, Triaged};

/// How many times a step may fail for an issue before kelpie stops trying it
pub(super) const TRIES: u32 = 5;

// Made on a repo that lacks it before the first issue is labelled with it
pub(super) const NEEDS_TRIAGE: NewLabel<'static> = NewLabel {
    name: TRIAGE,
    color: "d876e3",
    description: "Kelpie researched this issue; it waits on the maintainer",
};

// The label made for a priority on a repo that has no label for it, under
// the name the board ranks by
fn priority_label(priority: &str) -> Option<NewLabel<'static>> {
    let (name, description) = match priority {
        "P1" => ("priority: P1", "Next up"),
        "P2" => ("priority: P2", "After P1"),
        "P3" => ("priority: P3", "Wanted, once higher priorities are done"),
        "P4" => ("priority: P4", "Wanted someday, after everything above it"),
        _ => return None,
    };
    Some(NewLabel {
        name,
        color: "c5def5",
        description,
    })
}

/// What kind of post the representative is asked for
const KIND: &str = "Write the first triage comment on this issue, from what kelpie found in the \
    repo. Greet the author, say what was found, and ask only the questions listed. It is a \
    comment, not a review: no verdict, and nothing promised.";

/// What kind of post the representative is asked for on an issue whose author
/// is asked to settle the open questions: kelpie lists them under the words,
/// with a collapsed prompt for the author's own terminal
const ASK_KIND: &str = "Write the lead-in to the first triage comment on this issue, from what \
    kelpie found in the repo. Greet the author, say what was found, and say kelpie asks the open \
    questions below and they can answer them all in one comment, or settle them in their own \
    terminal with the prompt kelpie put in the collapsed section, and that the maintainer \
    triages what is still open. Do not restate or renumber the questions or the prompt: kelpie \
    posts them under your words. It is a comment, not a review: no verdict, and nothing \
    promised.";

/// What kind of post the representative is asked for on the second pass, the
/// one that reads the author's answers
const ANSWERS_KIND: &str = "Write the triage comment that reads this issue again after its \
    author answered kelpie's questions. Greet the author, say what kelpie now found, and ask \
    only the questions still open. It is the last round: what is still open goes to the \
    maintainer. It is a comment, not a review: no verdict, and nothing promised.";

/// What kind of post the representative is asked for on an issue that
/// something else already covers
const DUPLICATE_KIND: &str = "Write the short comment that closes this issue as a duplicate. Greet \
    the author, name the open issue or pull request that already covers it and say why in one \
    sentence, say kelpie is closing the issue as a duplicate of it, and thank them in one plain \
    line that names what they reported, never as the opener. It is a comment, not a review: no \
    verdict, and nothing promised.";

/// What the representative is told of a comment posted to an author who is not
/// the maintainer
const PLAIN_FACTS: &str = "\nThe author is not the maintainer: write in plain language, with no \
    file path, no code term and no mention of logs, config or deployments, and end with one line \
    on what happens next.\n";

/// A research that read, whose comment is not yet on the issue
#[derive(Debug)]
pub(super) struct Waiting {
    pub(super) author: String,
    pub(super) said: String,
    pub(super) found: Found,
}

/// What the runner keeps of one issue's triage that has not gone through, in
/// memory only
#[derive(Debug, Default)]
pub(in crate::runner) struct Tries {
    failures: u32,
    told: bool,
    // Whether its comments were read for the App's marker; a post that may have
    // gone up unrecorded clears it
    pub(super) read: bool,
    // What the research found, kept until the comment is recorded so that a
    // failed post is tried again without another call
    pub(super) waiting: Option<Waiting>,
    // The comment as worded, kept until the forge takes it
    pub(super) body: Option<String>,
    // The next wording call gets the guard's reason.
    pub(super) refused: Option<Leak>,
    pub(super) refused_bodies: Vec<String>,
    // The labels the research asked for that the repo has, put on after the
    // comment; the record keeps them from there
    labels: Vec<String>,
    // The agent-ready body the reply gave for a maintainer's issue the pass
    // rewrites into one the check passes, kept until the record carries it
    ready_body: Option<String>,
    // The labels the issue carried when its pass began, so a triage label it
    // carries later came by hand and stands; kept once, whatever retry runs
    pub(in crate::runner) seen: Option<Vec<String>>,
    // The triage label that came by hand while the call ran, which stands: the
    // comment is the pass's last write, and nothing is labelled or rewritten
    stood: Option<String>,
    // The other item this issue is being closed as a duplicate of, kept with
    // the comment so the post is followed by the close and not by labels
    duplicate: Option<Addressed>,
    // The numbers the research was shown, which is all an `addressed_by` may
    // name, kept so the cover can be read again before the post
    shown: Vec<u64>,
    // The login of whoever opened the issue, kept so what they write while the
    // triage waits is told from what anyone else writes
    pub(in crate::runner) author: Option<String>,
    // What the author may be asked, which also says what text posted to them
    // may name; a pass that lost it to a restart is worded for a reporter
    pub(super) role: Option<AuthorRole>,
    // That the triage comment asks its author the open questions, and the
    // hash of the body its prompt was written against
    pub(in crate::runner) ask: bool,
    local_seen: Option<u64>,
    // The questions the pass left open, which the answers pass falls back on
    // when the comment that holds them cannot be read
    pub(in crate::runner) open: Vec<Question>,
    // What the triager call on this issue is for: the first pass, the pass
    // that reads the author's answers, or the one that reads a rewritten body
    pub(in crate::runner) purpose: Purpose,
    // The author's answers, heard while the questions wait and kept until the
    // second call is made with them
    pub(in crate::runner) answers: Option<String>,
    // That the body the author's own session was told to rewrite has changed, so
    // the triager reads the issue again
    pub(in crate::runner) local_edit: bool,
    // The hash of the body the pass that reads a rewritten body was made on,
    // which its comment is not owed when the body changes again under the call
    pub(in crate::runner) local_read: Option<u64>,
    // The issue body with the settled answers under its author's text, kept
    // until the record carries it
    pub(in crate::runner) answers_body: Option<String>,
    // The hash of the body the current call was made from
    pub(super) begun: Option<u64>,
    // The questions an earlier triage round on this issue asked and its author
    // answered, which a pass asked again over the issue may not ask a second
    // time
    pub(in crate::runner) settled: Vec<String>,
}

impl Tries {
    // What an issue asked for by hand starts with: its comments need no read
    // before the first call
    pub(super) fn asked() -> Self {
        Self {
            read: true,
            ..Self::default()
        }
    }
}

impl Runner {
    // The issue's record, if the state holds one
    pub(super) fn triage_record(&self, issue: u64) -> Option<&Triaged> {
        let triage = self.state.triage.as_ref()?;
        triage.issues.iter().find(|t| t.issue == issue)
    }

    // Whether `issue` was given up on
    pub(super) fn triage_given_up(&self, issue: u64) -> bool {
        self.triage_tries
            .get(&issue)
            .is_some_and(|t| t.failures >= TRIES)
    }

    // Says in the log that `why` went wrong for `issue`, once, and again when
    // it is given up. A failure the forge being busy explains does not count,
    // since asking again later may work.
    pub(in crate::runner) fn triage_failed(&mut self, issue: u64, why: &str, counts: bool) {
        let tries = self.triage_tries.entry(issue).or_default();
        if counts {
            tries.failures += 1;
        }
        let line = if tries.failures >= TRIES {
            format!(
                "triage #{issue}: {why}; it failed {TRIES} times, so it is not tried again \
                 until kelpie restarts"
            )
        } else if !tries.told {
            tries.told = true;
            format!("triage #{issue}: {why}; it is tried at the next poll")
        } else {
            return;
        };
        self.notes.push(line);
    }

    // The login of the App that covers the repo, as its comments name it
    pub(super) fn app_login(&self) -> Option<String> {
        self.ports.app_reads.as_ref()?.login(&self.remote)
    }

    // Posts what the research found: the labels the repo has are read first,
    // so the comment names only those, then the representative words the
    // facts, and the comment goes up with the marker under it.
    pub(in crate::runner) fn post_triage(
        &mut self,
        issue: u64,
        author: &str,
        said: &str,
        shown: &[u64],
        found: &Found,
        purpose: Purpose,
    ) -> Result<(), StateError> {
        let found = self.scrub_triage(issue, found);
        let tries = self.triage_tries.entry(issue).or_default();
        tries.shown = shown.to_owned();
        tries.purpose = purpose;
        tries.author = Some(author.to_owned());
        tries.open = found.open.clone();
        tries.waiting = Some(Waiting {
            author: author.to_owned(),
            said: said.to_owned(),
            found: found.clone(),
        });
        self.say_triage(issue)
    }

    // Retries the comment of an issue whose research read: the wording kept is
    // posted, or else it is worded again from what the research found
    pub(super) fn retry_triage(&mut self, issue: u64) -> Result<(), StateError> {
        let Some(tries) = self.triage_tries.get(&issue) else {
            return Ok(());
        };
        if tries.body.is_some() {
            return self.send_triage(issue);
        }
        self.say_triage(issue)
    }

    // The board's label for `priority`, made on the repo, or none with a note
    // where the forge would not make it
    fn made_priority(&self, issue: u64, priority: &str) -> Option<String> {
        let label = priority_label(priority)?;
        match self.make_missing_labels([label]) {
            Ok(()) => Some(label.name.to_owned()),
            Err(why) => {
                self.notes.push(format!(
                    "triage #{issue}: {why}, so it goes on with no priority"
                ));
                None
            }
        }
    }

    // Puts the labels the pass found on `issue`; `false` when a write failed.
    // The pass has a priority only for an issue that began with none. One put
    // on by hand since stands.
    pub(super) fn put_pass_labels(&mut self, issue: u64, labels: &[String]) -> bool {
        let Some(ours) = labels.iter().find(|l| board::is_priority_label(l)) else {
            return self.put_labels(issue, labels);
        };
        let carried = match self.labels_now(issue) {
            Ok(carried) => carried,
            Err(e) => {
                let why = format!("cannot read its labels: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return false;
            }
        };
        let hand = (carried.iter())
            .find(|l| board::is_priority_label(l) && !board::same_priority(l, ours));
        if let Some(hand) = hand {
            self.notes.push(format!(
                "triage #{issue}: `{hand}` went on by hand while the pass ran, so that stands: \
                 kelpie does not put `{ours}` on"
            ));
        }
        let on = carried.iter().any(|l| board::is_priority_label(l));
        let put: Vec<String> = (labels.iter())
            .filter(|l| !(on && board::is_priority_label(l)))
            .cloned()
            .collect();
        self.put_labels(issue, &put)
    }

    // Puts each of `labels` on `issue`, stopping at the first the forge refuses
    fn put_labels(&mut self, issue: u64, labels: &[String]) -> bool {
        for label in labels {
            if let Err(e) = self.label_issue(issue, label, true) {
                let why = format!("cannot label it `{label}`: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return false;
            }
        }
        true
    }

    // Words the comment from the waiting research and posts it
    fn say_triage(&mut self, issue: u64) -> Result<(), StateError> {
        // The call can run for minutes, and the author's session can rewrite the
        // body again while it runs. A comment, labels or a ready write from that
        // read would describe a body the issue no longer holds.
        if self.local_read_stale(issue)? {
            return Ok(());
        }
        let Some(waiting) = self
            .triage_tries
            .get(&issue)
            .and_then(|t| t.waiting.as_ref())
        else {
            return Ok(());
        };
        let (author, said, found) = (
            waiting.author.clone(),
            waiting.said.clone(),
            waiting.found.clone(),
        );
        let shown = self
            .triage_tries
            .get(&issue)
            .map_or_else(Vec::new, |t| t.shown.clone());
        let (author, said, found) = (author.as_str(), said.as_str(), &found);
        // A pass asked again over an issue kelpie already triaged does not ask
        // its author again a question the earlier rounds heard them answer.
        let settled = (self.triage_tries.get(&issue)).map_or_else(Vec::new, |t| t.settled.clone());
        let found = match settled.is_empty() {
            true => found.clone(),
            false => Found {
                open: (found.open.iter())
                    .filter(|q| !settled.iter().any(|s| s == &ask::question_key(&q.question)))
                    .cloned()
                    .collect(),
                ..found.clone()
            },
        };
        let found = &found;
        // The research named something that already covers the issue. Kelpie
        // closes it as a duplicate when the forge still agrees the item is
        // open, and words an ordinary triage comment when it does not.
        if let Some(named) = found.addressed_by.clone() {
            match self.cover_of(issue, &named, &shown) {
                Cover::Yes => return self.say_duplicate(issue, author, said, &named),
                Cover::No(why) => {
                    self.notes.push(format!(
                        "triage #{issue}: its reply names #{} as covering it, and {why}; it is \
                         triaged as any other issue",
                        named.number
                    ));
                }
                Cover::Unknown => return Ok(()),
            }
        }
        let have = match self.ports.forge.repo_labels(&self.remote) {
            Ok(have) => have,
            Err(e) => {
                let why = format!("cannot read the repo's labels: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return Ok(());
            }
        };
        // The reply names a type `bug` and a priority `P2`. The board reads the label
        // `priority: P2`, so a priority is looked for under that name first, then the
        // bare `P2`; a repo with neither gets the board's name made. A type the repo
        // does not have is never put on. A priority the issue began with stands.
        let mut labels: Vec<String> = Vec::new();
        if let Some(kind) = found.kind {
            labels.extend(board::repo_label(&have, kind).map(str::to_owned));
        }
        let ranked = (self.triage_seen(issue).iter()).any(|l| board::is_priority_label(l));
        if let Some(priority) = found.priority.filter(|_| !ranked) {
            let named = board::repo_priority(&have, priority).map(str::to_owned);
            labels.extend(named.or_else(|| self.made_priority(issue, priority)));
        }
        labels.extend(self.kind_label(issue, found.work, &have));
        // A triage label the maintainer put on while the call ran stands, and
        // it is read for before the comment is worded: the comment never
        // promises a rewrite or a `ready-for-agent` that kelpie will not do.
        let stood = match self.triage_labelled_by_hand(issue) {
            Ok(label) => label,
            Err(e) => {
                let why = format!("cannot read its labels: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return Ok(());
            }
        };
        // The first pass asks its author when the reply left a question only
        // they can answer and they are one kelpie asks. Neither the answers pass
        // nor the pass that reads a body the author rewrote asks again: one
        // round of questions is the whole of it.
        let purpose = self.triage_tries.get(&issue).map(|t| t.purpose);
        let answers = purpose == Some(Purpose::Answers);
        let local = purpose == Some(Purpose::LocalRead);
        let ask = purpose == Some(Purpose::First)
            && stood.is_none()
            && !found.open.is_empty()
            && self.askable(author);
        let ready = ready::readiness(self, issue, author, found, &labels);
        // A rewrite the check still refuses is written as settled answers are,
        // and the issue ends `needs-triage`.
        let (ready_body, rewrite) = match (stood.as_ref(), ready) {
            (Some(label), _) => {
                self.notes.push(format!(
                    "triage #{issue}: `{label}` went on while the triager ran, so that stands: \
                     kelpie labels it no further and writes no new body"
                ));
                (None, None)
            }
            (None, ready::Ready::Body(body)) => (Some(body), None),
            (None, ready::Ready::Rewrite(body)) => (None, Some(body)),
            (None, ready::Ready::Local(leak)) => {
                self.notes.push(format!(
                    "triage #{issue}: its rewritten body names {leak}, so it is not written \
                     and the issue ends `{TRIAGE}"
                ));
                (None, None)
            }
            (None, ready::Ready::Not) => (None, None),
        };
        // An outside issue the answers pass has settled keeps its author's text:
        // what is settled goes under it, never in place of it. A body kelpie
        // could not read is not stood for by a comment without it: the pass is
        // tried again at the next poll with its round still open.
        let answers_body = match (
            ready_body.as_ref().or(rewrite.as_ref()),
            answers,
            stood.as_ref(),
        ) {
            (None, true, None) if found.open.is_empty() => match found
                .body
                .as_deref()
                .map(|settled| self.settled_answers(issue, settled))
            {
                Some(ask::Settled::Body(body)) => Some(body),
                Some(ask::Settled::Not) | None => None,
                Some(ask::Settled::Held) => return Ok(()),
            },
            _ => None,
        };
        // A rewrite the check refuses goes under the author's text, never in place of it.
        let rewrite = match rewrite.map(|body| self.settled_answers(issue, &body)) {
            Some(ask::Settled::Body(whole)) => Some(whole),
            Some(ask::Settled::Not) | None => None,
            Some(ask::Settled::Held) => return Ok(()),
        };
        let rewritten = rewrite.is_some();
        let answers_body = answers_body.or(rewrite);
        let kind = match (answers, local, ask) {
            (true, _, _) => ANSWERS_KIND,
            (false, true, _) => super::local::LOCAL_READ_KIND,
            (false, false, true) => ASK_KIND,
            (false, false, false) => KIND,
        };
        // Only the maintainer is told the files, tests and criteria: text posted
        // to anyone else holds no file path or code term, so the words and the
        // prompt for their own terminal are made from a report without them.
        let plain = self.posts_plain(issue);
        let found = &match plain {
            true => Found {
                files: Vec::new(),
                tests: Vec::new(),
                criteria: Vec::new(),
                ..found.clone()
            },
            false => found.clone(),
        };
        // The comment that hands an outside issue over mentions the maintainer;
        // the maintainer's own issue and the comment that asks do not.
        let maintainer = self.maintainer();
        let own = (maintainer.as_ref()).is_ok_and(|login| login.eq_ignore_ascii_case(author));
        let mention = match (ask || own, &maintainer) {
            (true, _) => None,
            (false, Ok(login)) => Some(login.clone()),
            (false, Err(why)) => {
                self.unmentioned_triage(issue, &why.to_string());
                None
            }
        };
        let reader = match (own, mention.is_some()) {
            (true, _) => Reader::Maintainer,
            (false, true) => Reader::Mentioned,
            (false, false) => Reader::Other,
        };
        let wording = Wording {
            ready: ready_body.is_some(),
            ask,
            answers,
            local,
            settled: answers_body.is_some() && !rewritten,
            rewrite: rewritten,
            stood: stood.is_some(),
        };
        let mut facts = facts(found, &labels, wording, reader);
        if plain {
            facts.push_str(PLAIN_FACTS);
        }
        let earlier: Vec<u64> = self
            .triage_record(issue)
            .map(|t| t.comment)
            .into_iter()
            .collect();
        let post = IssuePost {
            kind,
            facts: &facts,
            earlier: &earlier,
            author,
            said,
        };
        let Some(words) = self.word_triage(issue, &post) else {
            return Ok(());
        };
        // The comment that asks lists the questions under the words and holds the
        // prompt for the author's own terminal, collapsed.
        let (body, local_seen) = match ask {
            true => match self.ask_comment(issue, &words, found) {
                Some((body, seen)) => (body, Some(seen)),
                None => return Ok(()),
            },
            false => {
                let cc = (mention.as_deref()).map_or_else(String::new, |l| format!("cc @{l}\n\n"));
                (format!("{words}\n\n{cc}{MARKER}"), None)
            }
        };
        let tries = self.triage_tries.entry(issue).or_default();
        tries.body = Some(body);
        tries.local_seen = local_seen;
        tries.labels = match stood {
            Some(_) => Vec::new(),
            None => labels,
        };
        tries.ready_body = ready_body;
        tries.stood = stood;
        tries.duplicate = None;
        tries.ask = ask;
        tries.answers_body = answers_body;
        self.send_triage(issue)
    }

    // Says in the log, once a run, that a comment that hands `issue` over goes
    // up without the maintainer's mention, since no login could be read
    fn unmentioned_triage(&self, issue: u64, why: &str) {
        if self.triage_unmentioned_told.replace(true) {
            return;
        }
        self.notes.push(format!(
            "triage #{issue}: its comment is posted without a mention of the maintainer, since \
             no login could be read: {why}; set `git.maintainer` to your GitHub login"
        ));
    }

    // Whether what is posted to `issue`'s author is worded plain, with no path
    // or code term: it is for anyone but the maintainer, and for a pass that
    // lost its role to a restart
    fn posts_plain(&self, issue: u64) -> bool {
        let role = (self.triage_tries.get(&issue)).and_then(|t| t.role);
        role.is_none_or(|role| role != AuthorRole::Maintainer)
    }

    // Words the short comment that names the other item, and posts it ahead of
    // the close. No label is asked for: a closed duplicate carries none.
    fn say_duplicate(
        &mut self,
        issue: u64,
        author: &str,
        said: &str,
        named: &Addressed,
    ) -> Result<(), StateError> {
        let mut facts = duplicate_facts(named);
        if self.posts_plain(issue) {
            facts.push_str(PLAIN_FACTS);
        }
        let earlier: Vec<u64> = self
            .triage_record(issue)
            .map(|t| t.comment)
            .into_iter()
            .collect();
        let post = IssuePost {
            kind: DUPLICATE_KIND,
            facts: &facts,
            earlier: &earlier,
            author,
            said,
        };
        let Some(words) = self.word_triage(issue, &post) else {
            return Ok(());
        };
        let tries = self.triage_tries.entry(issue).or_default();
        tries.body = Some(format!("{words}\n\n{}", duplicate_marker(named.number)));
        tries.labels.clear();
        tries.duplicate = Some(named.clone());
        self.send_triage(issue)
    }

    // Posts the comment kept for `issue`, records it, and goes on to what the
    // record says is left: the close of a duplicate, or its labels
    pub(super) fn send_triage(&mut self, issue: u64) -> Result<(), StateError> {
        let Some(body) = self.triage_tries.get(&issue).and_then(|t| t.body.clone()) else {
            return Ok(());
        };
        // The same read again for a pass that was worded but could not be
        // posted: the body it was worded on may not be the one the issue holds.
        if self.local_read_stale(issue)? {
            return Ok(());
        }
        if self.repeats_refused_triage(issue, &body) {
            return Ok(());
        }
        // The wording call can run for minutes, so a duplicate's cover is read
        // again against the forge right before the post. A cover that went in
        // the meantime is not posted: the comment is worded again at the next
        // poll, and by then as an ordinary one.
        let named = self
            .triage_tries
            .get(&issue)
            .and_then(|t| t.duplicate.clone());
        if let Some(named) = named {
            let shown = self
                .triage_tries
                .get(&issue)
                .map_or_else(Vec::new, |t| t.shown.clone());
            match self.cover_of(issue, &named, &shown) {
                Cover::Yes => {}
                Cover::No(why) => {
                    self.notes.push(format!(
                        "triage #{issue}: its reply names #{} as covering it, and {why}; its \
                         comment is not posted, and it is triaged at the next poll",
                        named.number
                    ));
                    if let Some(tries) = self.triage_tries.get_mut(&issue) {
                        tries.body = None;
                        tries.duplicate = None;
                    }
                    return Ok(());
                }
                Cover::Unknown => return Ok(()),
            }
        }
        // The forge may take a post whose answer is lost, so the marker is read again.
        if let Some(tries) = self.triage_tries.get_mut(&issue) {
            tries.read = false;
        }
        match self.ports.voice.post(&self.remote, issue, &body) {
            Ok(posted) => {
                let cover = self
                    .triage_tries
                    .get(&issue)
                    .and_then(|t| t.duplicate.as_ref().map(|t| t.number));
                let asked = self.triage_tries.get(&issue).is_some_and(|t| t.ask);
                self.record_triage(issue, posted.id, posted.created, cover, asked)?;
                match cover {
                    Some(of) => self.close_duplicate(issue, of),
                    None => self.finish_triage(issue),
                }
            }
            Err(e) => {
                let why = format!("cannot post the comment: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                self.keep_triage_refusal(issue, &e, body);
                Ok(())
            }
        }
    }

    // Records `comment` as `issue`'s triage comment, in place of any record it
    // had, at the step that says the forge has it. A comment naming the other
    // item it duplicates is recorded at `closed`, saved before the close, so a
    // restart closes it without wording or posting the comment again
    pub(super) fn record_triage(
        &mut self,
        issue: u64,
        comment: u64,
        at: Timestamp,
        duplicate_of: Option<u64>,
        asked: bool,
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(triage) = next.triage.as_mut() else {
            return Ok(());
        };
        // The calls the issue's earlier record kept go into the new one: the
        // caps count every triager call, the first pass's and the author's
        // round's alike.
        let kept = triage
            .issues
            .iter()
            .find(|t| t.issue == issue)
            .map_or_else(Vec::new, |t| t.calls.clone());
        // The calls saved for the issue while it had no record join them, and
        // stop being kept on their own.
        let (_, made) = triage.take_calls(issue);
        triage.issues.retain(|t| t.issue != issue);
        let labels = (self.triage_tries.get(&issue)).map_or_else(Vec::new, |t| t.labels.clone());
        let ready_body = (self.triage_tries.get(&issue)).and_then(|t| t.ready_body.clone());
        // The labels the pass began with go into the record with it: a pass
        // that resumes after a restart has no in-memory reading left, and
        // without these it takes kelpie's own `needs-triage` for the
        // maintainer's.
        let mut seen = (self.triage_tries.get(&issue))
            .and_then(|t| t.seen.clone())
            .unwrap_or_default();
        // Where the comment opens a wait, the queue label joins the labels the
        // pass began with: the listing they came from may predate the record. No
        // label that says what to do with the issue is joined, so one put on by
        // hand during the pass stands, and ends the wait.
        if asked
            && !(seen.iter()).any(|s| s == TRIAGE)
            && self
                .triage_labels_now(issue)
                .is_ok_and(|now| (now.iter()).any(|l| l.as_str() == TRIAGE))
        {
            seen.push(TRIAGE.to_owned());
        }
        let stood = (self.triage_tries.get(&issue)).is_some_and(|t| t.stood.is_some());
        // The wait goes into the record with the comment, measured from when
        // it went up and not from when kelpie recorded it: an answer written
        // while the post was still being read back is heard. `asked` says the
        // comment carries the questions, from the pass or from its own text.
        let seen_body = (self.triage_tries.get(&issue)).and_then(|t| t.local_seen);
        let questions = asked.then_some(TriageQuestions { comment, at });
        let local = seen_body.map(|body| TriageLocal { at, body });
        let answers_body = (self.triage_tries.get(&issue)).and_then(|t| t.answers_body.clone());
        let step = match (duplicate_of, stood, answers_body.as_ref()) {
            (Some(_), _, _) => TriageStep::Closed,
            (None, true, _) => TriageStep::Done,
            (None, false, Some(_)) => TriageStep::Answers,
            (None, false, None) => TriageStep::Commented,
        };
        // The calls the issue already made stay with its record, and the ones
        // made since it was saved join them: the caps count every triager call,
        // the first pass's and the author's round's alike. A record made from a
        // marker found in the issue's comments has none, since no call ran for it.
        let mut record = Triaged {
            issue,
            comment,
            step,
            at: self.ports.clock.now(),
            author: (self.triage_tries.get(&issue))
                .and_then(|t| t.author.clone())
                .unwrap_or_default(),
            held: None,
            calls: kept,
            labels,
            duplicate_of,
            ready_body: ready_body.or(answers_body),
            seen: seen.clone(),
            asked: None,
            questions,
            local,
            spam: None,
            begun: (self.triage_tries.get(&issue)).and_then(|t| t.begun),
        };
        for at in made {
            record.add_call(at);
        }
        triage.issues.push(record);
        // The triager ran on the issue as the check last read it, so its end
        // is checked again.
        if let Some(kept) = next.checks.iter_mut().find(|c| c.issue == issue) {
            kept.triage = CheckTriage::Ran;
        }
        // A record saved finished needs the cap kept, as `triage_stepped` does
        // for the steps that finish later.
        if step == TriageStep::Done
            && let Some(triage) = next.triage.as_mut()
        {
            triage.prune();
        }
        self.save(next)?;
        self.triage_due.retain(|d| d.issue != issue);
        self.triage_queue.retain(|&n| n != issue);
        if let Some(tries) = self.triage_tries.get_mut(&issue) {
            tries.body = None;
            tries.waiting = None;
            tries.duplicate = None;
            tries.ready_body = None;
            tries.stood = None;
            tries.ask = false;
            tries.local_seen = None;
            tries.answers_body = None;
            tries.local_edit = false;
            tries.local_read = None;
            // The labels the record now holds as begun-with are the ones the
            // pass reads from here on: the in-memory reading must not keep an
            // older, emptier one.
            tries.seen = Some(seen.clone());
        }
        Ok(())
    }

    pub(super) fn triage_stepped(
        &mut self,
        issue: u64,
        step: TriageStep,
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let held = next
            .triage
            .as_mut()
            .and_then(|t| t.issues.iter_mut().find(|t| t.issue == issue));
        let Some(held) = held else {
            return Ok(());
        };
        held.step = step;
        if step != TriageStep::BodyWritten {
            held.labels.clear();
        }
        held.ready_body = None;
        if matches!(step, TriageStep::Done | TriageStep::Ready)
            && let Some(triage) = next.triage.as_mut()
        {
            triage.prune();
        }
        self.save(next)
    }

    // Does what `issue`'s record says is left: the duplicate's close, the research's
    // labels then `needs-triage`, the answers pass's write, or the ready path's
    // body write then labels, `ready-for-agent` last, each saved before the next
    // write. No label goes on while the author's ask or questions are open.
    pub(super) fn finish_triage(&mut self, issue: u64) -> Result<(), StateError> {
        loop {
            let Some(record) = self.triage_record(issue).cloned() else {
                return Ok(());
            };
            if record.asked.is_some() || record.questions.is_some() || record.local.is_some() {
                return Ok(());
            }
            match record.step {
                TriageStep::Done | TriageStep::Ready | TriageStep::Taken => return Ok(()),
                TriageStep::Closed => {
                    let Some(of) = self.triage_record(issue).and_then(|t| t.duplicate_of) else {
                        return Ok(());
                    };
                    self.close_duplicate(issue, of)?;
                    return Ok(());
                }
                TriageStep::Commented => {
                    let owed = self.triage_record(issue).and_then(|t| t.ready_body.clone());
                    match owed {
                        Some(body) => match self.ready_write(issue, &body)? {
                            ready::ReadyWrite::Written | ready::ReadyWrite::Refused => {}
                            ready::ReadyWrite::Held => return Ok(()),
                        },
                        None => {
                            let labels = self
                                .triage_record(issue)
                                .map_or_else(Vec::new, |t| t.labels.clone());
                            if !self.put_pass_labels(issue, &labels) {
                                return Ok(());
                            }
                            self.triage_stepped(issue, TriageStep::Labelled)?;
                        }
                    }
                }
                TriageStep::Answers => {
                    let Some(body) = self.triage_record(issue).and_then(|t| t.ready_body.clone())
                    else {
                        // Nothing is owed in the body. The labels the pass found
                        // are, and `triage_stepped` would clear them.
                        self.triage_answers_commented(issue)?;
                        continue;
                    };
                    match self.answers_write(issue, &body)? {
                        ask::Write::Written | ask::Write::Refused => {}
                        ask::Write::Held => return Ok(()),
                    }
                }
                TriageStep::BodyWritten => {
                    match self.label_ready(issue)? {
                        ready::ReadyLabels::On => self.triage_stepped(issue, TriageStep::Ready)?,
                        ready::ReadyLabels::Stood => {}
                        ready::ReadyLabels::Held => return Ok(()),
                    }
                    self.triage_tries.remove(&issue);
                    return Ok(());
                }
                TriageStep::Labelled => {
                    // The labels before this one took time, so a triage label
                    // put on by hand since the pass began stands, and
                    // `needs-triage` goes on nothing over it. One already
                    // there is this step's own, from a lost answer.
                    match self.triage_labelled_by_hand_but(issue, TRIAGE) {
                        Ok(Some(label)) => {
                            self.notes.push(format!(
                                "triage #{issue}: `{label}` went on by hand while its labels \
                                 went on, so that stands: kelpie adds no `{TRIAGE}`"
                            ));
                            self.triage_stepped(issue, TriageStep::Done)?;
                            self.triage_tries.remove(&issue);
                            return Ok(());
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let why = format!("cannot read its labels: {e}");
                            self.triage_failed(issue, &why, !forge_is_busy(&e));
                            return Ok(());
                        }
                    }
                    if let Err(why) = self.make_missing_labels([NEEDS_TRIAGE]) {
                        self.triage_failed(issue, &format!("cannot make `{TRIAGE}`: {why}"), true);
                        return Ok(());
                    }
                    if let Err(e) = self.label_issue(issue, TRIAGE, true) {
                        let why = format!("cannot label it `{TRIAGE}`: {e}");
                        self.triage_failed(issue, &why, !forge_is_busy(&e));
                        return Ok(());
                    }
                    self.triage_stepped(issue, TriageStep::Done)?;
                    self.triage_tries.remove(&issue);
                    return Ok(());
                }
            }
        }
    }
}
