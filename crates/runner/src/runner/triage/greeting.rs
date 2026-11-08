//! What each triage pass asks the representative for, and who its comment greets
//!
//! The kind lines say what a comment is for. None of them names a greeting:
//! the greeting is the representative call's rule for a thread's first post,
//! and `Runner::triage_opening` says who it greets. Kelpie's own App is nobody
//! to greet, and the report it opened is nobody's to thank. A post on a thread
//! kelpie has posted on before greets nobody and opens with what changed.

use super::super::Runner;
use super::super::representative::Opening;

/// What kind of post the representative is asked for
pub(super) const KIND: &str = "Write the first triage comment on this issue, from what kelpie \
    found in the repo. Say what was found, and ask only the questions listed. Name a label, a \
    file or a criterion only where it changes what the reader does next. It is a comment, not a \
    review: no verdict, and nothing promised. It runs about 20 to 60 words.";

/// What kind of post the representative is asked for on an issue whose author
/// is asked to settle the open questions: kelpie lists them under the words,
/// with a collapsed prompt for the author's own terminal, and says under them
/// how to answer, so the lead-in says none of that
pub(super) const ASK_KIND: &str = "Write the lead-in to the first triage comment on this issue, \
    from what kelpie found in the repo. Say what was found, and say kelpie asks the open \
    questions below. Do not restate or renumber the questions or the prompt: kelpie posts them \
    under your words. Name a label, a file or a criterion only where it changes what the reader \
    does next. It is a comment, not a review: no verdict, and nothing promised. The lead-in runs \
    about 20 to 60 words.";

/// What kind of post the representative is asked for on the second pass, the
/// one that reads the author's answers
pub(super) const ANSWERS_KIND: &str = "Write the triage comment that reads this issue again \
    after its author answered kelpie's questions. Say what kelpie now found, and ask only the \
    questions still open. It is the last round: what is still open goes to the maintainer. Name \
    a label, a file or a criterion only where it changes what the reader does next. It is a \
    comment, not a review: no verdict, and nothing promised. It runs about 20 to 60 words.";

/// What kind of post the representative is asked for on the pass that reads a
/// body its author's own session rewrote
pub(super) const LOCAL_READ_KIND: &str = "Write the triage comment on this issue, which kelpie \
    read again after its author's own session rewrote its body from kelpie's prompt. Say what \
    kelpie now found, and ask only the questions still open. This is the last round: what is \
    still open goes to the maintainer. Name a label, a file or a criterion only where it changes \
    what the reader does next. It is a comment, not a review: no verdict, and nothing promised. \
    It runs about 20 to 60 words.";

/// What kind of post the representative is asked for on an issue that
/// something else already covers
pub(super) const DUPLICATE_KIND: &str = "Write the short comment that closes this issue as a \
    duplicate. Name the open issue or pull request that already covers it and say why in one \
    sentence, and say kelpie is closing the issue as a duplicate of it. It is a comment, not a \
    review: no verdict, and nothing promised.";

impl Runner {
    // How a triage comment opens: the login it greets by name, and whether it
    // may thank the author for the report. The login kelpie asks is the one to
    // greet on an issue its own App opened; the App is nobody to greet, and its
    // own report is nobody's to thank.
    pub(super) fn triage_opening<'a>(
        &self,
        author: &'a str,
        asked: Option<&'a str>,
    ) -> Opening<'a> {
        let own = super::is_app(author, self.app_login().as_deref());
        Opening {
            greet: asked.or((!own).then_some(author)),
            thanks: !own,
        }
    }
}
