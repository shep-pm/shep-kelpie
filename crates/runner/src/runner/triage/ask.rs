//! The author's round: the questions, the answers
//!
//! An open question goes to the issue's author before the maintainer. The first
//! triage comment lists the questions with the answer the research recommends,
//! and holds a prompt for the author's terminal in a collapsed section
//! ([`super::local`]). Whatever the author writes next is their answers; kelpie
//! reads no word in it. No reply `triage.wait_days` on, and the maintainer
//! triages the issue as it stands. Only the maintainer's issues, and any under
//! `pick = "all"`, are asked ([`Runner::askable`]). While the wait is open no
//! status label goes on ([`Runner::finish_triage`]); [`super::heard`] hears the author.

use std::collections::BTreeMap;

use super::super::Runner;
use super::super::contributions::post::{forge_is_busy, refused_text};
use super::TRIAGE_LABELS;
use super::research::Question;
use crate::board::TRIAGE;
use crate::ports::{AuthorAssociation, OpenIssue, Timestamp};
use crate::settings::{AuthorRole, TriagePick};
use crate::state::{StateError, TriageStep, Triaged};

/// What a triager call on an issue is for
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::runner) enum Purpose {
    /// The first pass, the one that asks
    #[default]
    First,
    /// The pass that reads the author's answers
    Answers,
    /// The pass that reads the issue again after its author's own session
    /// rewrote its body
    LocalRead,
}

// The lines around the text kelpie writes under an issue's own
const UNDER_OPEN: &str = "<!-- kelpie:under -->";
const UNDER_CLOSE: &str = "<!-- /kelpie:under -->";

/// `body` with `text` as its one kelpie section. It goes in place of an
/// earlier section, or else under the body.
pub(in crate::runner) fn under_text(body: &str, text: &str) -> String {
    let section = format!("{UNDER_OPEN}\n{}\n{UNDER_CLOSE}", text.trim());
    let earlier = body.find(UNDER_OPEN).and_then(|start| {
        let len = body[start..].find(UNDER_CLOSE)?;
        Some((start, start + len + UNDER_CLOSE.len()))
    });
    match earlier {
        Some((start, end)) => format!("{}{section}{}", &body[..start], &body[end..]),
        None => format!("{}\n\n{section}", body.trim()),
    }
}

/// What writing the settled answers under the issue author's text came to
pub(in crate::runner) enum Write {
    /// It went on, and its step is saved
    Written,
    /// The forge refused it, so the issue ends `needs-triage` like any other,
    /// and the issue is told the write did not happen
    Refused,
    /// Nothing more this pass: the maintainer's own label stood, or the write
    /// failed and is tried at the next poll
    Held,
}

/// What the answers pass can put under the issue author's text: the whole body
/// to write, nothing to write, or a read of the issue that failed and is tried
/// again. The last is not the first: the round is still open and its comment is
/// not worded from a settled body kelpie does not have.
pub(in crate::runner) enum Settled {
    /// The author's text with what is settled under it
    Body(String),
    /// Nothing goes in the body: the pass settled no body, or what it settled
    /// names something of this machine
    Not,
    /// The issue's body could not be read, so the pass is tried again
    Held,
}

/// What the log says when the wait on an issue's author ends because it closed
pub(super) fn closed_note(issue: u64) -> String {
    format!(
        "triage #{issue}: it is closed, so the wait on its author ends with no post and no label"
    )
}

impl Runner {
    // Whether `author` is one kelpie asks to settle its open questions itself:
    // the maintainer on their own issue, and any author under `pick = "all"`.
    // Anyone else gets `needs-triage` with no ask.
    pub(in crate::runner) fn askable(&self, author: &str) -> bool {
        if self.settings.triage.pick == TriagePick::All {
            return true;
        }
        self.own_author(author)
    }

    // What `author` may be asked: the role `triage.authors` lists them in, else
    // the maintainer's own, else the one their association implies. The
    // association says how they relate to the repo, not what they can answer,
    // which is why a listed login wins over it.
    pub(in crate::runner) fn author_role(
        &self,
        author: &str,
        association: Option<AuthorAssociation>,
    ) -> AuthorRole {
        if let Some(role) = self.settings.triage.role_of(author) {
            return role;
        }
        match self.own_author(author) {
            true => AuthorRole::Maintainer,
            false => AuthorRole::of(association),
        }
    }

    // How long kelpie waits for the author's answers, in seconds
    pub(super) fn wait_seconds(&self) -> u64 {
        u64::from(self.settings.triage.wait_days.get()).saturating_mul(86_400)
    }

    // The issues whose triage waits on the author, each with when the wait
    // began. Their time is what the thread read reaches back to, so a reply is
    // not missed, and what silence is measured from.
    pub(in crate::runner) fn triage_waits(&self) -> Vec<(u64, Timestamp)> {
        self.state.triage.as_ref().map_or_else(Vec::new, |triage| {
            triage
                .issues
                .iter()
                .filter_map(|record| answers_wait(record).map(|(_, at)| (record.issue, at)))
                .collect()
        })
    }

    // The issues whose triage waits on the author, in the order their records
    // hold them
    pub(super) fn triage_waiting(&self) -> Vec<Triaged> {
        self.state.triage.as_ref().map_or_else(Vec::new, |triage| {
            triage
                .issues
                .iter()
                .filter(|record| {
                    record.asked.is_some() || record.questions.is_some() || record.local.is_some()
                })
                .cloned()
                .collect()
        })
    }

    // Ends each wait the forge says is over: the issue is closed, or carries a
    // triage label the maintainer put on by hand. Neither is a word from the
    // author, so nothing is posted and no label kelpie owed goes on.
    pub(super) fn settle_triage_waits(&mut self, open: &[OpenIssue]) -> Result<(), StateError> {
        for record in self.triage_waiting() {
            let issue = record.issue;
            // A call on the issue is in flight. Ending its wait under the call
            // would let the call land on a record already saved finished and put
            // a comment and labels on an issue that is closed or already labelled
            // by hand. The next poll settles it once the call has landed.
            if self.flights.triaging() == Some(issue) {
                continue;
            }
            let Some(this) = open.iter().find(|i| i.number == issue) else {
                self.triage_wait_stood(issue, closed_note(issue))?;
                continue;
            };
            // The listing is where a restart gets the author's login back, which
            // is what tells their words from anyone else's.
            self.triage_tries.entry(issue).or_default().author = Some(this.author.clone());
            let seen = self.triage_seen(issue);
            let stood = (this.labels.iter()).find(|l| {
                TRIAGE_LABELS.contains(&l.as_str())
                    && !seen.iter().any(|s| s.as_str() == l.as_str())
            });
            if let Some(label) = stood {
                self.triage_wait_stood(
                    issue,
                    format!(
                        "triage #{issue}: `{label}` went on by hand, so the wait on its author \
                         ends with no post and no label"
                    ),
                )?;
            }
        }
        Ok(())
    }

    // Saves the record as finished with nothing owed: the wait ended because
    // something outside it did, so kelpie posts nothing and puts on no label.
    pub(in crate::runner) fn triage_wait_stood(
        &mut self,
        issue: u64,
        note: String,
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let mut held = false;
        if let Some(record) = next
            .triage
            .as_mut()
            .and_then(|t| t.issues.iter_mut().find(|t| t.issue == issue))
        {
            record.asked = None;
            record.questions = None;
            record.local = None;
            record.labels.clear();
            record.ready_body = None;
            record.step = TriageStep::Done;
            held = true;
        }
        if held && let Some(triage) = next.triage.as_mut() {
            triage.prune();
        }
        self.save(next)?;
        self.triage_tries.remove(&issue);
        self.triage_due.retain(|d| d.issue != issue);
        self.triage_queue.retain(|&n| n != issue);
        self.notes.push(note);
        Ok(())
    }

    // Ends the wait as `none`: the questions and the prompt come off the record,
    // and what the pass owed goes on, which is its labels and then
    // `needs-triage`. The triage comment stays as the maintainer's starting
    // point: nothing new is posted.
    pub(in crate::runner) fn triage_none(&mut self, issue: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        if let Some(record) = next
            .triage
            .as_mut()
            .and_then(|t| t.issues.iter_mut().find(|t| t.issue == issue))
        {
            record.asked = None;
            record.questions = None;
            record.local = None;
        }
        self.save(next)?;
        if let Some(tries) = self.triage_tries.get_mut(&issue) {
            tries.answers = None;
            tries.local_edit = false;
            tries.local_read = None;
            tries.open.clear();
        }
        self.finish_triage(issue)
    }

    // Saves the record as commented with its labels still owed, which is what
    // the answers pass leaves behind whether its write went on or was refused:
    // the body is no longer owed, the type and priority the pass found are.
    // `triage_stepped` would clear them with the body, so this keeps them.
    pub(super) fn triage_answers_commented(&mut self, issue: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        if let Some(record) = next
            .triage
            .as_mut()
            .and_then(|t| t.issues.iter_mut().find(|t| t.issue == issue))
        {
            record.step = TriageStep::Commented;
            record.ready_body = None;
        }
        self.save(next)
    }

    // Writes the settled answers under `issue`'s author's text and saves its
    // step, unless a triage label the maintainer put on by hand stands, which
    // leaves the record with no further label and no write.
    pub(super) fn answers_write(&mut self, issue: u64, body: &str) -> Result<Write, StateError> {
        match self.triage_before_write(issue) {
            Ok((None, true)) => {
                self.triage_body_moved(issue)?;
                self.triage_answers_commented(issue)?;
                return Ok(Write::Refused);
            }
            Ok((Some(label), _)) => {
                self.notes.push(format!(
                    "triage #{issue}: `{label}` went on by hand, so that stands: kelpie writes \
                     no answers into its body and labels it no further"
                ));
                self.triage_wait_stood(
                    issue,
                    format!("triage #{issue}: `{label}` stands, and its answers are not written"),
                )?;
                return Ok(Write::Held);
            }
            Ok((None, false)) => {}
            Err(e) => {
                let why = format!("cannot read its labels: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return Ok(Write::Held);
            }
        }
        match self.ports.voice.edit_issue_body(&self.remote, issue, body) {
            Ok(()) => {
                self.triage_body_written(issue, body)?;
                self.triage_answers_commented(issue)?;
                Ok(Write::Written)
            }
            Err(e) if refused_text(&e) => {
                self.notes.push(format!(
                    "triage #{issue}: its answers were refused: {e}; they are not written, and \
                     the issue ends `{TRIAGE}"
                ));
                // The step moves to `commented` here rather than through
                // `triage_ready_dropped`, which would leave it at `answers` for
                // `finish_triage` to step with `triage_stepped`, and that clears
                // the labels the pass found with the body.
                self.triage_answers_commented(issue)?;
                // The comment said the answers would go under the author's text.
                // Say on the issue that they did not, rather than leave that
                // promise there unkept.
                self.notice_on(
                    issue,
                    "Kelpie did not write the answers into this issue's body: the text it was \
                     given was refused. The body stays as its author wrote it, and the maintainer \
                     triages it next.",
                );
                Ok(Write::Refused)
            }
            Err(e) => {
                let why = format!("cannot write its body: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                Ok(Write::Held)
            }
        }
    }

    // The issue's body with the settled answers under its author's own text.
    // [`Settled::Held`] is the forge failing to read it, which is tried again:
    // the round stays open and no comment is worded without the body. The
    // author's text is never written over.
    pub(in crate::runner) fn settled_answers(&mut self, issue: u64, settled: &str) -> Settled {
        let read =
            super::ticket(issue).and_then(|ticket| self.ports.tickets.issue(&self.remote, ticket));
        let body = match read {
            Ok(found) => found.body,
            Err(e) => {
                let why = format!("cannot read its body: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return Settled::Held;
            }
        };
        let whole = under_text(&body, settled);
        if let Some(leak) = self.local.find(&whole, crate::local_paths::Surface::Prose) {
            self.notes.push(format!(
                "triage #{issue}: the answers it would write name {leak}, so they are not \
                 written and the issue ends `{TRIAGE}"
            ));
            return Settled::Not;
        }
        Settled::Body(whole)
    }

    // The questions kelpie posted for `issue` and its author's answers, which
    // the second pass reads fenced with the issue. `None` when neither can be
    // had, which is no call this poll.
    pub(super) fn answers_round(&mut self, issue: u64) -> Option<(String, String)> {
        let (comment, _) = answers_wait(self.triage_record(issue)?)?;
        let answers = self.triage_tries.get(&issue)?.answers.clone()?;
        let read = super::ticket(issue)
            .and_then(|ticket| self.ports.tickets.issue_comments(&self.remote, ticket));
        let round = match read {
            Ok(comments) => (comments.iter().find(|c| c.id == comment)).map(|c| c.body.clone()),
            Err(e) => {
                let why = format!("cannot read its questions: {e}");
                self.triage_failed(issue, &why, !forge_is_busy(&e));
                return None;
            }
        };
        // A questions comment that cannot be read is stood for by the questions
        // the pass still holds. With neither, there is nothing to put beside the
        // issue, and no call this poll.
        let asked = match round.or_else(|| {
            let open = self
                .triage_tries
                .get(&issue)
                .map(|t| t.open.clone())
                .unwrap_or_default();
            (!open.is_empty()).then(|| questions_block(&open))
        }) {
            Some(asked) => asked,
            None => {
                self.notes.push(format!(
                    "triage #{issue}: its questions comment is gone and its questions were lost, \
                     so its answers are not read and the second pass waits for the next poll"
                ));
                return None;
            }
        };
        let answers = confirmed(&answers, &recommended_in(&asked));
        Some((asked, answers))
    }
}

/// What the ask comment says under its questions, so an author knows one word
/// confirms them
pub(super) const REPLY_HINT: &str = "Reply `ok` to take every recommended answer, and `Qn: ...` \
    to change one, such as `ok` then `Q2: ...` on the next line.";

// Whether `body` is a triage comment that carries the questions for the author:
// the first heading [`questions_block`] writes. A question's own text cannot
// pass for it: [`unfenced`] took the mark out of the text the block holds.
pub(super) fn holds_questions(body: &str) -> bool {
    body.contains("\u{2753} Q1 - ")
}

// The comment whose questions the author answers and when the wait on them
// began. A record saved by a kelpie that asked for a word holds an `asked`
// instead of `questions`, and the triage comment it was saved with is the one
// its author answers.
pub(super) fn answers_wait(record: &Triaged) -> Option<(u64, Timestamp)> {
    (record.questions.map(|q| (q.comment, q.at)))
        .or_else(|| record.asked.map(|a| (record.comment, a.at)))
}

// The questions as the grilling skill shapes a round, without its bold: each
// numbered, with the answer the research recommends. Code writes them; the
// representative words only the lead-in. A fence in a question is dropped, as
// it would swallow the collapsed prompt, and so are the marks [`recommended_in`] reads by.
pub(super) fn questions_block(questions: &[Question]) -> String {
    let round: Vec<String> = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let question = format!("❓ Q{} - {}", i + 1, unfenced(&q.question));
            match unfenced(&q.recommended).as_str() {
                "" => question,
                recommended => format!("{question}\n\n➡️ {recommended}"),
            }
        })
        .collect();
    round.join("\n\n---\n\n")
}

// The recommended answer of each question in `asked`, by its number, read back
// from the shape [`questions_block`] wrote: the comment itself, or the block
// alone. A question without one has no entry. The last answer ends where the
// reply hint, or else the collapsed section, begins.
fn recommended_in(asked: &str) -> BTreeMap<usize, String> {
    let mut found = BTreeMap::new();
    let mut at = 0;
    for n in 1.. {
        let marker = format!("❓ Q{n} - ");
        let Some(start) = asked[at..].find(&marker).map(|i| at + i) else {
            break;
        };
        let after = start + marker.len();
        let end =
            (asked[after..].find(&format!("❓ Q{} - ", n + 1))).map_or(asked.len(), |i| after + i);
        let section = &asked[start..end];
        let section = (section.split(&format!("\n\n{REPLY_HINT}")).next()).unwrap_or(section);
        let section = section.split("\n\n<details>").next().unwrap_or(section);
        let section = section.trim_end();
        let section = section.strip_suffix("---").unwrap_or(section).trim_end();
        if let Some((_, recommended)) = section.split_once("\n\n➡️ ") {
            found.insert(n, recommended.trim().to_owned());
        }
        at = end;
    }
    found
}

// The author's answers with a reply of `ok` expanded: a line that is only `ok`
// takes every question's recommended answer, and a `Qn: ...` line changes
// question `n` alone. Without an `ok` line the answers are as written. Any
// other line is kept after the expanded answers.
fn confirmed(answers: &str, recommended: &BTreeMap<usize, String>) -> String {
    let mut ok = false;
    let mut changed: BTreeMap<usize, String> = BTreeMap::new();
    let mut rest: Vec<&str> = Vec::new();
    let mut under: Option<usize> = None;
    for line in answers.lines() {
        if says_ok(line) {
            ok = true;
            under = None;
        } else if let Some((n, text)) = numbered(line) {
            changed.insert(n, text.to_owned());
            under = Some(n);
        } else if let Some(n) = under {
            let text = changed.entry(n).or_default();
            text.push('\n');
            text.push_str(line);
        } else {
            rest.push(line);
        }
    }
    if !ok {
        return answers.to_owned();
    }
    let mut all = recommended.clone();
    all.extend(changed);
    // With no recommended answer to put in its place, `ok` stays as written
    // for the second pass to read, rather than leave the reply empty.
    if all.is_empty() {
        return answers.to_owned();
    }
    let mut lines: Vec<String> = all
        .into_iter()
        .map(|(n, text)| format!("Q{n}: {}", text.trim()))
        .collect();
    let rest = rest.join("\n");
    if !rest.trim().is_empty() {
        lines.push(rest.trim().to_owned());
    }
    lines.join("\n")
}

// `Qn: text` as a line of an answer: the question's number and what follows
fn numbered(line: &str) -> Option<(usize, &str)> {
    let (head, text) = line.trim_start().split_once(':')?;
    let n = head.strip_prefix(['Q', 'q'])?;
    Some((n.parse().ok()?, text.trim()))
}

// A line that takes every recommended answer: `ok` alone, any case, a trailing
// `.` or `!` allowed
fn says_ok(line: &str) -> bool {
    line.trim()
        .trim_end_matches(['.', '!'])
        .eq_ignore_ascii_case("ok")
}

// Whether the author's reply answers question `n` of the round it follows, read
// the way [`confirmed`] reads it: a line that is only `ok` takes every question
// of the round, a `Qn: ...` line takes that one. A reply in neither form answers
// no question kelpie can name, whatever the second pass makes of it.
pub(super) fn answers_one(reply: &str, n: usize) -> bool {
    (reply.lines()).any(|line| says_ok(line) || numbered(line).is_some_and(|(m, _)| m == n))
}

// The text a question is compared by: as [`questions_block`] writes it, its first
// line only, in one case. Both sides of the settled filter go through it, so what
// was read back off a comment equals what the triager named.
pub(super) fn question_key(text: &str) -> String {
    let shaped = unfenced(text);
    (shaped.lines().next())
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}

fn unfenced(text: &str) -> String {
    ["```", "~~~", "❓", "➡️", "<details>"]
        .iter()
        .fold(text.to_owned(), |text, mark| text.replace(mark, ""))
        .trim()
        .to_owned()
}
