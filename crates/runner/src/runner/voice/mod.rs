//! What kelpie writes on GitHub for the maintainer, through its App
//!
//! Where an App covers the project's repo, a ruling, a notice and each
//! review round of kelpie's own reviewers are comments on the work item's
//! pull request, or on its issue while it has none. A ruling mentions the
//! maintainer, so GitHub notifies them, and ends with how to answer it. A
//! ruling the project manager, the terminal or a withdrawal settles has its
//! comment edited to say so (see [`settled_comment`]). A failed post is told
//! once in the runner's log and never fails the step it belongs to. Where no
//! App covers the repo nothing here posts.

use std::sync::Arc;

use self::card::ruling_comment;
use super::Runner;
use super::agent_fit;
use super::gate::short;
use super::ruling::stuck_comment;
use crate::ports::{Finding, ForgeError, NewLabel, Severity, Voice};
use crate::settings::ForgeSlug;
use crate::state::{Heard, MergeNotice, ProjectState, Ruling, RulingKind};
use crate::ticket::TicketId;

/// Where a ruling's posted comment is kept in the state
pub(super) enum Placed {
    /// It mentions a login, whose reply in the thread answers it
    Heard(Heard),
    /// It mentions no one: the id of the comment
    Unmentioned(u64),
}

impl Placed {
    /// The id of the comment, none where it is not known
    pub(super) fn comment(&self) -> u64 {
        match self {
            Self::Heard(heard) => heard.ruling_comment,
            Self::Unmentioned(comment) => *comment,
        }
    }

    /// Keeps it in `state` for ruling `id`
    pub(super) fn keep(self, state: &mut ProjectState, id: TicketId) {
        match self {
            Self::Heard(heard) => state.heard.insert(id, heard).map(drop),
            Self::Unmentioned(comment) => state.unmentioned.insert(id, comment).map(drop),
        };
    }
}

/// What posting a ruling's comment through the App came to
pub(super) struct AppPost {
    /// Why the post failed, if it did
    pub failed: Option<String>,
    /// Where the comment is kept, when it went up
    pub placed: Option<Placed>,
    /// Why no one could be named to mention, when the comment went up without a mention
    pub unasked: Option<ForgeError>,
}

/// Posts `ruling`'s comment on `thread` as the App, mentioning the login `mention` names
///
/// A ruling still goes up when no one can be named to mention, as the record on the thread, but it
/// notifies no one and only the terminal answers it.
pub(super) fn post_app_ruling(
    voice: &dyn Voice,
    remote: &ForgeSlug,
    thread: u64,
    ruling: &Ruling,
    mention: Result<String, ForgeError>,
) -> AppPost {
    let (mention, unasked) = match mention {
        Ok(login) => (Some(login), None),
        Err(e) => (None, Some(e)),
    };
    let body = ruling_comment(mention.as_deref(), ruling);
    let posted = match voice.post(remote, thread, &body) {
        Err(e) => {
            return AppPost {
                failed: Some(e.to_string()),
                placed: None,
                unasked: None,
            };
        }
        Ok(posted) => posted,
    };
    // Its answer is read from the comment's own time and id, the forge's.
    let placed = Some(match mention {
        Some(mentions) => Placed::Heard(Heard {
            mentions,
            time: posted.created,
            comment: posted.id,
            ruling_comment: posted.id,
        }),
        // No one is read for an answer, but the comment is still edited
        // when the ruling is settled.
        None => Placed::Unmentioned(posted.id),
    });
    AppPost {
        failed: None,
        placed,
        unasked,
    }
}

impl Runner {
    /// What a ruling posted without a mention is told as, with the reason said once a run in the log
    pub(super) fn tell_unmentioned(&self, id: TicketId, thread: u64, why: &ForgeError) -> String {
        if !self.unmentioned_told.replace(true) {
            self.notes.push(format!(
                "ruling {id} is posted on #{thread} without a mention, so it notifies \
                 no one and only `shep kelpie rule` answers it: {why}; set \
                 `git.maintainer` to your GitHub login"
            ));
        }
        format!(
            "posted without mentioning the maintainer, since the login `gh` acts as \
             could not be read: {why}. Only `shep kelpie rule` answers it"
        )
    }

    /// The login a ruling mentions where it is known without asking the forge
    pub(super) fn known_maintainer(&self) -> Option<String> {
        match &self.settings.git.maintainer {
            Some(login) => Some(login.as_str().to_owned()),
            None => self.gh_login.get().cloned(),
        }
    }

    /// Whether kelpie's App covers the project's repo
    pub(super) fn app_covers(&self) -> bool {
        self.ports.voice.covers(&self.remote)
    }

    /// The login a ruling mentions: `git.maintainer`, else the login `gh`
    /// acts as, asked once a run and kept
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the setting is absent and `gh` could not say who
    /// it acts as: who to mention is not known.
    pub(super) fn maintainer(&self) -> Result<String, ForgeError> {
        if let Some(login) = self.known_maintainer() {
            return Ok(login);
        }
        // An answer that failed is asked again at the next ruling.
        let login = self.ports.forge.viewer()?;
        let _ = self.gh_login.set(login.clone());
        Ok(login)
    }

    /// Keeps the id of comment `id`, which kelpie posted on `number` as the login `gh` acts as,
    /// in the state file at once, so a crash does not forget it
    pub(super) fn keep_comment_id(&self, number: u64, id: u64) {
        self.posted.borrow_mut().push(id);
        let mut kept = self.state.clone();
        kept.note_posted(&self.posted.borrow());
        if let Err(e) = self.store.save(&kept) {
            self.notes.push(format!(
                "cannot save the id of comment {id} on #{number}: {e}"
            ));
        }
    }

    /// Posts `body` on issue or pull request `number` as the login `gh` acts
    /// as, and keeps the comment's id in the state file at once: a comment
    /// kelpie wrote is never an answer to a ruling, though it is the
    /// maintainer's own login's, and a crash must not forget it. The window is
    /// the forge having the comment and the file not yet having its id, a
    /// crash between the two or a write that fails, which is told in the log.
    pub(super) fn comment_as_gh(&self, number: u64, body: &str) -> Result<(), ForgeError> {
        self.comment_as_gh_id(number, body).map(drop)
    }

    /// As [`Self::comment_as_gh`], and returns the comment's id
    pub(super) fn comment_as_gh_id(&self, number: u64, body: &str) -> Result<u64, ForgeError> {
        let id = self.ports.forge.comment(&self.remote, number, body)?;
        self.keep_comment_id(number, id);
        Ok(id)
    }

    /// What writes to the repo as the App where one covers it, which needs no lock
    pub(super) fn writer(&self) -> Writer {
        Writer {
            forge: Arc::clone(&self.ports.forge),
            tickets: Arc::clone(&self.ports.tickets),
            voice: Arc::clone(&self.ports.voice),
            remote: self.remote.clone(),
            source: self.settings.tickets.source,
            app: self.app_covers(),
        }
    }

    /// Makes each of `wanted` the repo lacks, as the App where one covers it
    pub(super) fn make_missing_labels<'a>(
        &self,
        wanted: impl IntoIterator<Item = NewLabel<'a>>,
    ) -> Result<(), String> {
        self.writer().make_missing_labels(wanted)
    }

    /// Opens an issue, as the App where one covers the repo
    pub(super) fn open_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.writer().open_issue(title, body, labels)
    }

    /// Files a ticket in the project's own source, and returns its id
    ///
    /// A GitHub project's goes out as the App where one covers the repo. A local
    /// project's is a file in the store, which no App reaches.
    pub(super) fn file_ticket(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.writer().file_ticket(title, body, labels)
    }

    /// Comments on `ticket`: a GitHub issue as the App where one covers the
    /// repo, any other as the ticket port writes it
    pub(super) fn comment_on_ticket(&self, ticket: TicketId, body: &str) -> Result<(), ForgeError> {
        match ticket.github() {
            Some(number) => self.comment_on_issue(number, body),
            None => (self.ports.tickets).comment_ticket(&self.remote, ticket, body),
        }
    }

    /// Comments on issue `number`, as the App where one covers the repo
    pub(super) fn comment_on_issue(&self, number: u64, body: &str) -> Result<(), ForgeError> {
        match self.app_covers() {
            true => self.ports.voice.comment(&self.remote, number, body),
            false => self.comment_as_gh(number, body),
        }
    }

    /// Adds `label` to issue `number`, or takes it off, as the App where one
    /// covers the repo
    pub(super) fn label_issue(&self, number: u64, label: &str, on: bool) -> Result<(), ForgeError> {
        match self.app_covers() {
            true => (self.ports.voice).set_issue_label(&self.remote, number, label, on),
            // `gh` labels a GitHub issue, whose number is a ticket id.
            false => (self.ports.tickets).set_issue_label(
                &self.remote,
                TicketId::from_issue(number).ok_or(ForgeError::Unreadable(format!(
                    "{number} is not an issue number"
                )))?,
                label,
                on,
            ),
        }
    }

    /// Posts `text` on `thread` as the App, where one covers the repo, and
    /// notes why it could not, once
    pub(super) fn notice_on(&mut self, thread: u64, text: &str) {
        if !self.app_covers() {
            return;
        }
        if let Err(e) = self.ports.voice.comment(&self.remote, thread, text) {
            self.notes
                .push(format!("cannot post a notice on #{thread}: {e}"));
        }
    }

    /// Posts a review round that ended on the pull request, where an App covers the repo
    pub(super) fn post_round(&mut self, round: &Round<'_>) {
        if !self.app_covers() {
            return;
        }
        let (number, text) = (round.number, round_comment(round));
        if let Err(e) = self.ports.voice.comment(&self.remote, number, &text) {
            self.notes.push(format!(
                "cannot post {}'s round {} on #{number}: {e}",
                round.reviewer, round.round
            ));
        }
    }
}

/// A review round of kelpie's own reviewer that ended, to post
pub(super) struct Round<'a> {
    /// The pull request it read
    pub(super) number: u64,
    /// Who read it
    pub(super) reviewer: &'a str,
    /// The round
    pub(super) round: u32,
    /// The head it read, when kelpie knows
    pub(super) head: Option<&'a str>,
    /// What it found
    pub(super) findings: &'a [Finding],
}

fn round_comment(round: &Round<'_>) -> String {
    let Round {
        reviewer,
        round: number,
        head,
        findings,
        ..
    } = round;
    let at = head.map_or_else(String::new, |head| format!(" at {}", short(head)));
    let read = format!("{reviewer} read this pull request{at} in review round {number}");
    if findings.is_empty() {
        return format!("{read} and found nothing.");
    }
    let count = match findings.len() {
        1 => "1 thing".to_owned(),
        n => format!("{n} things"),
    };
    let lines: Vec<String> = findings.iter().map(finding_line).collect();
    format!("{read} and found {count}:\n\n{}", lines.join("\n"))
}

pub(in crate::runner) fn finding_line(finding: &Finding) -> String {
    let severity = match finding.severity {
        Severity::High => "HIGH",
        Severity::Medium => "MEDIUM",
        Severity::Low => "LOW",
    };
    let place = match finding.line {
        0 => finding.file.clone(),
        line => format!("{}:{line}", finding.file),
    };
    let (what, why) = (finding.what.trim(), finding.why.trim());
    let what = what.strip_suffix('.').unwrap_or(what);
    format!("- {severity} `{place}`: {what}. {why}")
}

/// The text of an automatic merge's notice
pub(super) fn merged_notice(notice: &MergeNotice) -> String {
    let MergeNotice {
        issue,
        pull_request,
        head,
    } = notice;
    format!(
        "Pull request #{pull_request} for issue #{issue} merged into main at {}, every gate \
         passed. Nothing to answer.",
        short(head)
    )
}

/// What a merge ruling warns of, in words fit for any reader of its thread
///
/// Nothing for any other kind of ruling.
pub(super) fn merge_notes(kind: &RulingKind) -> Vec<&'static str> {
    let RulingKind::Merge {
        unreviewed,
        open_threads,
        unread_head,
        note_fix,
        late_fix,
        nits,
        ..
    } = kind
    else {
        return Vec::new();
    };
    let flags = [
        (
            unreviewed.is_some(),
            "No reviewer read it in its last review.",
        ),
        (
            open_threads.is_some(),
            "Review bot threads are still open on it.",
        ),
        (
            *unread_head,
            "No review round is on record as reading this head.",
        ),
        (
            *note_fix,
            "This head is the worker's fix for a note or comment, and no reviewer read it.",
        ),
        (
            *late_fix,
            "This head is the worker's fix for a review bot's late review, and no \
             reviewer read it.",
        ),
        (
            nits.is_some(),
            "Review bots left nits on it, which never hold a merge.",
        ),
    ];
    (flags.into_iter())
        .filter_map(|(set, line)| set.then_some(line))
        .collect()
}

/// What a ruling says to a reader of its thread
///
/// Nothing the forge or a reviewer said, which is not written for strangers.
/// Under `gh` the merge and follow-up rulings post no comment; through the
/// App every kind does, since the thread is where the maintainer answers.
pub(super) fn said(kind: &RulingKind) -> String {
    match kind {
        RulingKind::Merge { head, .. } => {
            let mut said = format!("Merge this pull request at {} into main?", short(head));
            for line in merge_notes(kind) {
                said.push(' ');
                said.push_str(line);
            }
            said
        }
        RulingKind::Stuck(reason) => stuck_comment(reason),
        RulingKind::Question { asked, .. } => asked.clone(),
        RulingKind::AgentFiles { files, .. } => format!(
            "This pull request changes agents' own files: {}.",
            files.join(", ")
        ),
        RulingKind::ForeignChange { description, .. } => {
            format!("This pull request was changed: {description}.")
        }
        RulingKind::AgentFit(fit) => agent_fit::said(fit),
        // Never posted on the pull request, so nothing here names the contributor
        // or what the review found.
        RulingKind::Contribution(_) => "This pull request waits on the maintainer.".to_owned(),
        RulingKind::FollowUp { findings, refused } => {
            let held = match findings.len() {
                1 => "1 confirmed finding".to_owned(),
                n => format!("{n} confirmed findings"),
            };
            match refused {
                Some(_) => format!(
                    "This pull request merged with {held} left unfixed, and the forge would \
                     not take them as issues. Try again?"
                ),
                None => {
                    format!(
                        "This pull request merged with {held} left unfixed. File them as issues?"
                    )
                }
            }
        }
    }
}

/// `text` as plain words in a comment, so it cannot mention, link or format:
/// markdown's marks are escaped, `://` and `www.` are broken, and an `@` is
/// followed by a zero-width space
pub(in crate::runner) fn inert(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '<' | '>' | '#' | '|' | '~' | '&'
            | '!' => {
                out.push('\\');
                out.push(c);
            }
            '@' => out.push_str("@\u{200b}"),
            _ => out.push(c),
        }
    }
    out.replace("://", ":\u{200b}//")
        .replace("www.", "www\u{200b}.")
        .replace("WWW.", "WWW\u{200b}.")
}

mod card;
mod writer;
pub(super) use self::writer::Writer;

pub(super) use self::card::{
    ASK_THE_MAINTAINER, BOARD, PROBLEM, RECOMMENDS, SAYS, answer_for, asks_the_maintainer, brief,
    chosen, effect_of, named, phrases, settled_comment, thumbs, ticked,
};

#[cfg(test)]
mod tests;
