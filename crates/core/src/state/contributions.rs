//! The pull requests from other people that kelpie follows

use serde::{Deserialize, Serialize};

use super::{ProjectState, RulingKind};
use std::collections::BTreeMap;

use crate::ports::{Cost, Finding, Timestamp};
use crate::settings::{AgentName, PerItemUsd};
use crate::work_item::ReviewStage;

/// A pull request from someone else, as the board last saw it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    /// The pull request
    pub pull_request: u64,
    /// Its author's login
    pub author: String,
    /// Its head commit's hash
    pub head: String,
    /// When the board first saw that head
    pub seen: Timestamp,
    /// The head the security review last read, and what it found
    ///
    /// Left out until a head is read. A new head clears it, so it names the
    /// current `head` or nothing, and a head with none is not yet read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Security>,
    /// The last head that passed the security review, or that the maintainer
    /// trusted with a yes to its ruling
    ///
    /// Left out until one has. The next head is read as the change since it,
    /// unless it is not an ancestor of the next head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<String>,
    /// CI on the head, once kelpie approved its held runs
    ///
    /// Left out until it did, and after a push: the new head is not approved
    /// until it passes the security review, so absent reads as waiting to be
    /// approved. Always names the current `head` or nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<Ci>,
    /// The local reviewers' pass over the head, and what it found
    ///
    /// Left out until a pass ends. A new head clears it, so it names the
    /// current `head` or nothing, and a head with none is not yet reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<Pass>,
    /// The pass under way over the current head, while a review bot's turn
    /// holds it
    ///
    /// Left out once the pass ends or a push clears it, so it names the
    /// current `head` or nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walking: Option<Walking>,
    /// The reviewer that last answered a pass, whatever head it read
    ///
    /// The next pass starts at the reviewer after it. Left out until one
    /// answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answered: Option<AgentName>,
    /// How many passes in a row each local reviewer reviewed nothing in, by
    /// name
    ///
    /// Left out while none has. A reviewer that reviews something is removed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub local_failures: BTreeMap<AgentName, u32>,
    /// What the contributor said and did on the pull request, oldest first,
    /// the last [`KEPT`]
    ///
    /// Ids, kinds and times only: the text is read again when a prompt needs
    /// it. Left out until there is something.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heard: Vec<Said>,
    /// What the maintainer wrote on the pull request, kept apart as context,
    /// the last [`KEPT`]
    ///
    /// Only comments and replies: the maintainer's pushes are no one's to
    /// hear. Left out until there is something.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<Said>,
    /// Where the next read of the pull request's review threads starts
    ///
    /// The forge's comments are read from one place for the whole repo, but a
    /// review thread's only with its pull request, and a read of it can fail
    /// alone. Left out until one succeeds, when it is [`Self::seen`] at the
    /// first head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews_from: Option<Timestamp>,
    /// What the contribution's priced model calls have cost, over every head
    ///
    /// A call whose harness reports no dollars adds nothing. Left out while
    /// nothing is spent.
    #[serde(default, skip_serializing_if = "Cost::is_free")]
    pub spend: Cost,
    /// How many times the cap was doubled: once for each answered over-budget
    /// ruling, and again while the spend was still above it. Left out while
    /// none was.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub budget_raises: u32,
    /// Since when kelpie has waited on the contributor for the current head
    ///
    /// Set when findings were recorded, a bot's threads are open, or CI went
    /// red, and moved to now by the contributor's word. Left out while kelpie
    /// waits on no one, and after a push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<Timestamp>,
    /// What the contributor said that the representative has dealt with,
    /// oldest first, the last [`KEPT`]
    ///
    /// A comment or reply is answered in a comment, or handed to the
    /// maintainer as a ruling, and either way it is not looked at again. Ids,
    /// kinds and times only. Left out until there is something.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answered: Vec<Said>,
    /// The decision the contributor asked for, while it waits on the
    /// maintainer's answer and then on the representative to word it
    ///
    /// Left out when no decision is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deciding: Option<Deciding>,
    /// Where kelpie's asking whether it may take the pull request over stands
    ///
    /// Left out until the first ask is posted, and after a push, a clock that
    /// stops, or the maintainer's answer to keep waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<Asked>,
    /// That the contributor let kelpie take the pull request over, or the
    /// maintainer did
    ///
    /// Left out until it was. Nothing is asked again once it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<Released>,
    /// The maintainer's answer to the merge ruling on the current head
    ///
    /// Left out until one is given, and after a push: it names the current
    /// `head` or nothing. A head with an answer is not asked about again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruled: Option<Ruled>,
    /// Where kelpie's taking the pull request over stands: the issue it filed
    /// for one that closes none, and whether the pull request's body names it
    ///
    /// Left out until a release or a `rework` answer to the merge ruling has
    /// kelpie take the pull request, and gone with the record once its work
    /// item opens. A pull request that closes an issue of its own needs no
    /// filing, and stays without this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adopting: Option<Adopting>,
    /// What the maintainer answered a `rework` merge ruling with, for the
    /// worker who takes the pull request over
    ///
    /// Left out until kelpie takes the pull request over. It is about the pull
    /// request and not one head, so a push keeps it: the work item's start
    /// reads it even where its contributor pushed after the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework: Option<String>,
}

/// How far kelpie has come to hang a released contribution's work item on an
/// issue, for a pull request that closes none
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adopting {
    /// The issue kelpie filed from the pull request's title and body. A poll
    /// that filed it and failed to edit the body files no second one.
    pub issue: u64,
    /// That `Resolves #<issue>` is in the pull request's body, so the merge
    /// closes the issue and the next poll edits the body no more
    #[serde(default, skip_serializing_if = "is_false")]
    pub resolves: bool,
}

/// The maintainer's answer to the merge ruling on one head of a contribution,
/// kept until it is carried out
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ruled {
    /// The head the ruling was about
    pub head: String,
    /// What the maintainer answered
    pub verdict: Verdict,
}

/// What the maintainer answered to a contribution's merge ruling
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// Yes: the head is merged, then the contributor thanked
    Yes {
        /// That the forge took the merge, so it is not asked again
        #[serde(default, skip_serializing_if = "is_false")]
        merged: bool,
        /// That the thanks was posted, so a restart does not post it again
        #[serde(default, skip_serializing_if = "is_false")]
        thanked: bool,
    },
    /// No: the head stays open. The note, once the representative has posted
    /// it for the contributor, is taken out.
    No {
        /// The maintainer's note, until it is posted; none for a no with no
        /// note
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Rework: kelpie takes the pull request over and finishes it itself.
    /// The note is what the maintainer wants changed, and the worker gets it
    /// before its first turn.
    Rework {
        /// The maintainer's note for the worker; none for a rework with no
        /// note, which a tick or the card's choice gives
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

/// Kelpie's asking the contributor whether it may take their pull request
/// over
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asked {
    /// The id of the latest ask's comment: a reaction counts on this one only
    pub comment: u64,
    /// When the latest ask was posted
    pub at: Timestamp,
    /// How many times it was asked
    pub asks: u32,
    /// How many asks were answered no, by a word or a thumbs down
    #[serde(default, skip_serializing_if = "is_zero")]
    pub nos: u32,
    /// The contributor's words after the latest ask that were read and were
    /// neither yes nor no, so they are not read again. The last [`KEPT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<Said>,
    /// That the maintainer chose to close the pull request with thanks, which
    /// the next poll does
    #[serde(default, skip_serializing_if = "is_false")]
    pub closing: bool,
    /// That the thanks for a close was posted, so a restart does not post it
    /// again while the close is retried
    #[serde(default, skip_serializing_if = "is_false")]
    pub thanked: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Asked {
    /// Whether the latest ask has no answer yet
    pub fn is_open(&self) -> bool {
        self.nos < self.asks
    }
}

/// That kelpie may take a contribution over, and how it came to
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Released {
    /// When it was recorded
    pub at: Timestamp,
    /// What released it
    pub how: How,
}

/// What released a contribution
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum How {
    /// The contributor said yes
    Yes,
    /// The contributor left a thumbs up on the ask
    ThumbsUp,
    /// The contributor did not answer before `quiet_days` ran out
    Silence,
    /// The maintainer had kelpie take it over: `adopt anyway` on the ruling for
    /// a contributor who said no twice
    Maintainer,
    /// The maintainer answered the merge ruling `rework`, which takes the pull
    /// request over without a release from its contributor
    Rework,
}

/// A contributor's request for a decision, handed to the maintainer as a
/// ruling, and the answer it is waiting for
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deciding {
    /// The comment or reply that asked, read again for the wording
    pub said: Said,
    /// The question put to the maintainer
    pub asked: String,
    /// The maintainer's answer, once given. The representative words it for
    /// the contributor and posts it, which ends this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A pass of the reviewers over one head of a contribution
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pass {
    /// The head it read
    pub head: String,
    /// The reviewers that were called, in the order they ran, a failing one
    /// included
    pub ran: Vec<AgentName>,
    /// The reviewer that answered, with findings or `CLEAN`. None when the
    /// pass ended unread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<AgentName>,
    /// Why no reviewer read the head, when none did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<String>,
    /// What the reviewer found, none for `CLEAN`. A review bot's are its open
    /// threads above a nit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    /// A review bot's open threads that are nits, kept apart from `findings`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nits: Vec<Finding>,
    /// The id of the review kelpie posted on the head for `findings`
    ///
    /// Left out until it is posted, and it is posted once: a head with an id
    /// here is never reviewed on the forge again, and a push clears the pass
    /// with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<u64>,
    /// The id of the comment kelpie posted on the head to say the reviewer
    /// found nothing, as `review` is for findings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u64>,
}

/// A pass part way through its reviewers, kept while a review bot's turn
/// waits on the bot
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Walking {
    /// The head it reads
    pub head: String,
    /// The reviewers the walk is past, whether they ran or were passed over
    pub past: Vec<AgentName>,
    /// The reviewers that were called so far, in order, as [`Pass::ran`]
    pub ran: Vec<AgentName>,
    /// Why each reviewer past that did not read did not
    pub missed: Vec<String>,
    /// The review bot the walk waits on, if it is at one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<BotTurn>,
}

/// A review bot's turn in a pass
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotTurn {
    /// The reviewer's agent file, which names the bot
    pub reviewer: AgentName,
    /// Where the turn is: waiting to summon, summoned, or reading the
    /// threads until they settle
    pub stage: ReviewStage,
}

/// CI on a contribution's head after kelpie approved its held runs
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ci {
    /// The head it approved and follows
    pub head: String,
    /// When kelpie approved the runs: the checks register after it
    pub since: Timestamp,
    /// Where CI stands
    pub state: CiState,
}

/// Where CI stands on a contribution's head
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CiState {
    /// Not every check has reported, or the rollup is not yet trusted
    Waiting,
    /// Every check finished and none failed, or all were skipped
    Green,
    /// Every check finished, and these failed
    Red(Vec<String>),
}

/// What the calls on a pull request that is not followed any more cost, kept
/// for when it is followed again
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spent {
    /// The pull request
    pub pull_request: u64,
    /// What its calls cost, as [`Contribution::spend`]
    #[serde(default, skip_serializing_if = "Cost::is_free")]
    pub spend: Cost,
    /// How many times its cap was doubled, as [`Contribution::budget_raises`]
    #[serde(default, skip_serializing_if = "is_zero")]
    pub budget_raises: u32,
}

/// How many of what was said a contribution keeps, in each list
pub const KEPT: usize = 50;

impl Contribution {
    /// A contribution at `head`, first seen at `seen`, that no one has read
    pub fn new(pull_request: u64, author: String, head: String, seen: Timestamp) -> Self {
        Self {
            pull_request,
            author,
            head,
            seen,
            security: None,
            passed: None,
            ci: None,
            pass: None,
            walking: None,
            last_answered: None,
            local_failures: BTreeMap::new(),
            spend: Cost::default(),
            budget_raises: 0,
            heard: Vec::new(),
            context: Vec::new(),
            reviews_from: None,
            waiting_since: None,
            answered: Vec::new(),
            deciding: None,
            asked: None,
            released: None,
            ruled: None,
            adopting: None,
            rework: None,
        }
    }

    /// The most its priced calls may cost before no further call starts:
    /// `per_item`, doubled once for each over-budget ruling answered
    pub fn budget_cap(&self, per_item: PerItemUsd) -> Cost {
        let doubled = 2u64.saturating_pow(self.budget_raises);
        Cost(per_item.cost().0.saturating_mul(doubled))
    }

    /// The cap its calls are over, or `None` while they cost no more than it
    pub fn over_budget(&self, per_item: PerItemUsd) -> Option<Cost> {
        let cap = self.budget_cap(per_item);
        (self.spend > cap).then_some(cap)
    }

    /// Lifts its cap for an answer to an over-budget ruling: one doubling, and
    /// more while its calls still cost more than the cap, so one answer
    /// always lets the next call start
    pub fn raise_budget(&mut self, per_item: PerItemUsd) {
        self.budget_raises = self.budget_raises.saturating_add(1);
        while self.over_budget(per_item).is_some() {
            self.budget_raises = self.budget_raises.saturating_add(1);
        }
    }

    /// This contribution with what the pull request spent while it was not
    /// followed, if it spent any
    #[must_use]
    pub fn with_spent(self, spent: Option<&Spent>) -> Self {
        Self {
            spend: spent.map_or(self.spend, |s| s.spend),
            budget_raises: spent.map_or(self.budget_raises, |s| s.budget_raises),
            ..self
        }
    }

    /// Where the next read of the review threads starts
    pub fn reviews_since(&self) -> Timestamp {
        self.reviews_from.unwrap_or(self.seen)
    }

    /// This contribution after a push to `head`, seen at `seen`: the head is
    /// unread, the last head that passed stays, and the push is heard
    #[must_use]
    pub fn pushed(&self, head: String, seen: Timestamp) -> Self {
        let mut next = Self {
            head: head.clone(),
            seen,
            security: None,
            ci: None,
            pass: None,
            walking: None,
            // The head's own results start it again, and the contributor
            // who pushed has been heard: the asking starts over with it, but
            // the maintainer's close is still to be carried out.
            waiting_since: None,
            asked: self.asked.clone().filter(|a| a.closing),
            // An answer is about the head it was given on
            ruled: None,
            // `seen` moves on, and the threads are read from where they were
            reviews_from: Some(self.reviews_since()),
            ..self.clone()
        };
        next.hear(Said::Push { head, at: seen });
        next
    }

    /// Whether `said` is on record already, among what was heard or the context
    pub fn knows(&self, said: &Said) -> bool {
        let same = |known: &Said| known.same_as(said);
        self.heard.iter().any(same) || self.context.iter().any(same)
    }

    /// Records what the contributor said, keeping the last [`KEPT`], unless it
    /// was kept once and has dropped off the list since. Returns whether it
    /// was recorded.
    pub fn hear(&mut self, said: Said) -> bool {
        keep(&mut self.heard, said)
    }

    /// Records what the maintainer said, keeping the last [`KEPT`], on the
    /// same terms as [`Self::hear`]
    pub fn overhear(&mut self, said: Said) -> bool {
        keep(&mut self.context, said)
    }

    /// What the contributor said that no answer or ruling has taken up yet,
    /// oldest first: comments and review replies, never a push
    pub fn unanswered(&self) -> Vec<Said> {
        let taken = |said: &&Said| !self.answered.iter().any(|a| a.same_as(said));
        let asks = |said: &&Said| !matches!(said, Said::Push { .. });
        (self.heard.iter().filter(asks).filter(taken))
            .cloned()
            .collect()
    }

    /// Records that `said` was answered, or handed to the maintainer,
    /// keeping the last [`KEPT`]
    pub fn answer(&mut self, said: Said) {
        if !self.answered.iter().any(|a| a.same_as(&said)) {
            self.answered.push(said);
        }
        let over = self.answered.len().saturating_sub(KEPT);
        self.answered.drain(..over);
    }

    /// When the latest thing on record happened: the place a read of the
    /// forge's comments starts from after a restart
    pub fn latest(&self) -> Timestamp {
        let times = self.heard.iter().chain(&self.context).map(Said::at);
        times.chain([self.seen]).max().unwrap_or(self.seen)
    }

    /// Whether the current head's results leave the next move to the
    /// contributor: CI is red, the reviewers found something, or a bot's
    /// threads are open, nits included
    pub fn waits_on_contributor(&self) -> bool {
        let red = |ci: &Ci| ci.head == self.head && matches!(ci.state, CiState::Red(_));
        let open = |pass: &Pass| {
            pass.head == self.head && !(pass.findings.is_empty() && pass.nits.is_empty())
        };
        self.ci.as_ref().is_some_and(red) || self.pass.as_ref().is_some_and(open)
    }

    /// Starts or clears the quiet clock to match the current head's results,
    /// leaving a running one where it is
    pub fn follow_clock(&mut self, now: Timestamp) {
        self.waiting_since = match self.waits_on_contributor() {
            true => self.waiting_since.or(Some(now)),
            false => None,
        };
        // Kelpie waits on no one, so there is nothing to have asked about. A
        // close the maintainer chose still goes ahead.
        if self.waiting_since.is_none() && !self.asked.as_ref().is_some_and(|a| a.closing) {
            self.asked = None;
        }
    }

    /// The contributor's word, written at `said`: a running clock that began
    /// no later starts over at `now`. A word from before kelpie began to wait
    /// is not an answer to what it waits on.
    pub fn restart_clock(&mut self, said: Timestamp, now: Timestamp) {
        if self.waiting_since.is_some_and(|since| said >= since) {
            self.waiting_since = Some(now);
        }
    }

    /// Whether the security review has read the current head
    pub fn is_read(&self) -> bool {
        self.security.as_ref().is_some_and(|s| s.head == self.head)
    }

    /// Whether kelpie approved the current head's held runs
    pub fn is_approved(&self) -> bool {
        self.ci.as_ref().is_some_and(|ci| ci.head == self.head)
    }

    /// Whether CI is green on the current head
    pub fn is_green(&self) -> bool {
        let green = |ci: &Ci| ci.head == self.head && ci.state == CiState::Green;
        self.ci.as_ref().is_some_and(green)
    }

    /// Whether a pass has ended on the current head
    pub fn is_reviewed(&self) -> bool {
        self.pass.as_ref().is_some_and(|p| p.head == self.head)
    }

    /// Whether the maintainer is to be asked whether to merge the current
    /// head: its pass has ended, CI is green on it, no answer to the ruling
    /// is kept for it and the maintainer is not closing the pull request
    pub fn awaits_merge_ruling(&self) -> bool {
        let closing = self.asked.as_ref().is_some_and(|a| a.closing);
        self.is_reviewed() && self.is_green() && self.ruled.is_none() && !closing
    }

    /// The walk of the current head's pass, if one is under way
    pub fn walking_head(&self) -> Option<&Walking> {
        self.walking.as_ref().filter(|w| w.head == self.head)
    }

    /// Whether the current head passed the security review, or the maintainer
    /// trusted it: it is the last one that did
    pub fn is_passed(&self) -> bool {
        self.passed.as_deref() == Some(self.head.as_str())
    }
}

// A full list has let its oldest go, so a comment written before all of what
// it still holds was kept once and is not recorded again: it would only push
// out something newer. Pushes are not comments and are always new.
fn keep(list: &mut Vec<Said>, said: Said) -> bool {
    let dropped = list.len() >= KEPT
        && !matches!(said, Said::Push { .. })
        && list
            .iter()
            .map(Said::at)
            .min()
            .is_some_and(|oldest| said.at() < oldest);
    if dropped {
        return false;
    }
    list.push(said);
    let over = list.len().saturating_sub(KEPT);
    list.drain(..over);
    true
}

/// One thing said or done on a pull request, with no text
///
/// A comment and a review reply are numbered apart by the forge, so an id
/// means a thing only with its kind.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Said {
    /// A comment on the pull request's conversation
    Comment {
        /// The forge's id for it
        id: u64,
        /// When it was written
        at: Timestamp,
    },
    /// A comment in a review thread on its diff
    Reply {
        /// The forge's id for it
        id: u64,
        /// When it was written
        at: Timestamp,
    },
    /// A push: the head moved to this commit
    Push {
        /// The new head commit's hash
        head: String,
        /// When the board saw the head move, which is after the push
        at: Timestamp,
    },
}

impl Said {
    /// When it happened
    pub fn at(&self) -> Timestamp {
        match self {
            Self::Comment { at, .. } | Self::Reply { at, .. } | Self::Push { at, .. } => *at,
        }
    }

    // Whether both are the same thing: the same kind and id, whenever each
    // was seen
    fn same_as(&self, other: &Said) -> bool {
        match (self, other) {
            (Self::Comment { id: a, .. }, Self::Comment { id: b, .. })
            | (Self::Reply { id: a, .. }, Self::Reply { id: b, .. }) => a == b,
            (Self::Push { head: a, .. }, Self::Push { head: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// What the security review made of one head
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Security {
    /// The head it read
    pub head: String,
    /// What it found
    pub verdict: SecurityVerdict,
}

/// The verdict on a head
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecurityVerdict {
    /// Nothing in it needs a human's eye, so it passed
    Passed,
    /// Something in it does, each a finding with its file, line and why
    Flagged(Vec<Finding>),
}

/// Why a contribution waits on the maintainer, saved in its ruling's kind as
/// `reason`
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Held {
    /// The security review flagged the head. Nothing of it runs until the
    /// maintainer answers: a yes trusts it, and a no leaves it unpassed.
    Security {
        /// The head the lines are in
        head: String,
        /// Each file, line and why the review flagged
        lines: Vec<Finding>,
    },
    /// The contribution's priced calls cost more than its cap, so no further
    /// call starts on it. A yes doubles the cap, again while it is below the
    /// spend, and so does a no with a note.
    OverBudget {
        /// The cap it was over: `budget.per_item_usd`, doubled once for each
        /// ruling like this one that was answered
        cap: Cost,
    },
    /// The contributor asked for a decision, which the representative does
    /// not make. The maintainer's answer is worded for the contributor and
    /// posted; nothing is posted until then.
    Question {
        /// What the representative asks the maintainer
        asked: String,
    },
    /// The contributor said no to kelpie's taking the pull request over
    /// twice and has been quiet since. The maintainer chooses: keep waiting,
    /// adopt it anyway, or close it with thanks.
    Waiting,
    /// The contributor has been quiet for `quiet_days` and the pull request's
    /// fork does not let the maintainers edit it or is gone, so kelpie cannot finish it
    /// on its own branch. The maintainer chooses: leave it open, which
    /// follows it no further, or close it with thanks.
    NoEdits,
    /// A contribution kelpie has taken over cannot be adopted: its pull request
    /// merges into another branch, or the issue its work item would hang on is
    /// labelled for an implementer the project does not list. The maintainer
    /// chooses: leave it open, which follows it no further, or close with
    /// thanks. Answering is not the only way out: the reason is read again each
    /// poll, so retargeting the pull request or fixing the label adopts it and
    /// withdraws the ruling.
    CannotAdopt {
        /// Why kelpie cannot adopt it, as the maintainer reads it
        why: String,
    },
    /// The pass has ended and CI is green on the head, and the maintainer
    /// decides every merge of someone else's work, whatever `git.merging`
    /// says. The ruling names the findings recorded and the bot threads
    /// open. A yes merges this head, and a no with a note is worded for the
    /// contributor and posted.
    Merge {
        /// The head a yes merges
        head: String,
        /// Whether the ruling offers no rework, because the pull request's
        /// fork cannot be pushed to. Saved only when set, and read as unset
        /// from a file saved before it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        rework_off: bool,
    },
}

impl RulingKind {
    /// Whether the ruling is about someone else's pull request. It is never
    /// posted on that pull request nor put to the project manager: a public
    /// comment would accuse the contributor and tip off an attacker, and text
    /// from a stranger must not reach an agent that answers it.
    pub fn is_contribution(&self) -> bool {
        matches!(self, Self::Contribution(_))
    }
}

impl ProjectState {
    /// Follows `followed` from now on, keeping what the calls on the pull
    /// requests it drops cost, and giving up what the ones it takes back had
    pub fn follow(&mut self, followed: Vec<Contribution>) {
        let is_followed = |n: u64| followed.iter().any(|c| c.pull_request == n);
        let dropped: Vec<Spent> = (self.contributions.iter())
            .filter(|c| {
                !is_followed(c.pull_request) && !(c.spend.is_free() && c.budget_raises == 0)
            })
            .map(|c| Spent {
                pull_request: c.pull_request,
                spend: c.spend,
                budget_raises: c.budget_raises,
            })
            .collect();
        self.spent.retain(|s| {
            !is_followed(s.pull_request)
                && !dropped.iter().any(|d| d.pull_request == s.pull_request)
        });
        self.spent.extend(dropped);
        let surplus = self.spent.len().saturating_sub(KEPT);
        self.spent.drain(..surplus);
        self.contributions = followed;
    }

    /// What the pull request `n` spent while it was not followed
    pub fn spent_by(&self, n: u64) -> Option<&Spent> {
        self.spent.iter().find(|s| s.pull_request == n)
    }
}
