//! Whether and how kelpie triages issues other people open,
//! `[app.dogs.kelpie.triage]`

use std::collections::BTreeMap;
use std::num::{NonZeroU8, NonZeroU32};

use schemars::JsonSchema;
use serde::Deserialize;

use super::agents::find;
use super::gateway::GatewayName;
use super::{AgentName, ExpertAgent, Settings, SettingsError};
use crate::agents::{Agents, Role, Runs, TRIAGER};
use crate::ports::{AuthorAssociation, Probability};
use crate::state::TRIAGED_KEPT;

/// The most questions one triage comment may ask
pub const MOST_QUESTIONS: u8 = 10;

/// The most a daily cap may ask for. Both count the calls saved in the triage
/// records, [`TRIAGED_KEPT`] of them are kept, and a record that holds a call is
/// kept ahead of one that holds none, so a cap past it is one kelpie cannot see
/// and cannot hold anyone to.
pub(super) const MOST_PER_DAY: u32 = 200;

/// The probability at or above which an issue is read as spam, when
/// `triage.spam.threshold` is absent
const SPAM_THRESHOLD: Probability = Probability::millionths(800_000);

/// Whose issues kelpie triages
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TriagePick {
    /// No one's: kelpie triages nothing
    #[default]
    Off,
    /// The maintainer's own issues
    Maintainer,
    /// Every person's issues
    All,
}

/// What the triager may ask the person who opened an issue, which is what
/// they can answer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorRole {
    /// The maintainer: decisions with recommended answers, in the repo's own
    /// terms
    Maintainer,
    /// Someone who works on the repo: what they saw and what they expected,
    /// and a decision only when their issue proposes one
    Developer,
    /// Someone who can only say what they saw: plain language, one question
    Reporter,
}

impl AuthorRole {
    /// The role the forge's association implies, for a login `triage.authors`
    /// does not list. An association the listing did not say is a reporter.
    pub fn of(association: Option<AuthorAssociation>) -> Self {
        match association {
            Some(AuthorAssociation::Owner) => Self::Maintainer,
            Some(
                AuthorAssociation::Member
                | AuthorAssociation::Collaborator
                | AuthorAssociation::Contributor,
            ) => Self::Developer,
            Some(
                AuthorAssociation::FirstTimeContributor
                | AuthorAssociation::FirstTimer
                | AuthorAssociation::None,
            )
            | None => Self::Reporter,
        }
    }

    /// The most questions one comment may ask this role, never past
    /// `max_questions`
    pub fn most_questions(&self, max_questions: NonZeroU8) -> usize {
        let cap = match self {
            Self::Maintainer => MOST_QUESTIONS,
            Self::Developer => MOST_DEVELOPER_QUESTIONS,
            Self::Reporter => 1,
        };
        usize::from(cap.min(max_questions.get()))
    }
}

/// The most questions a developer is asked at once
const MOST_DEVELOPER_QUESTIONS: u8 = 3;

/// Whether and how kelpie triages issues other people open
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Triage {
    /// Whose issues kelpie triages. `off` when absent. Anything but `off`
    /// needs the GitHub App to cover the repo, since the triager would
    /// otherwise post as the maintainer.
    pub pick: TriagePick,
    /// Whether kelpie takes back an issue the maintainer opened and labelled
    /// `ready-for-agent` by hand, swapping the label for `needs-triage` so the
    /// pass asks its questions before the board starts it. Needs `pick` to be
    /// `maintainer` or `all`. Off when absent.
    pub hand_ready: bool,
    /// Whole days without a comment from the author before kelpie hands the
    /// issue to the maintainer as it stands, above 0. 7 when absent.
    pub wait_days: NonZeroU32,
    /// The most questions one triage comment asks, 1 to 10. 10 when absent.
    pub max_questions: NonZeroU8,
    /// The agent file whose role is `triager` that researches an issue.
    /// `triager` when absent.
    pub agent: AgentName,
    /// The most triage calls in any 24 hours, up to 200. An issue past it waits
    /// for the next free place and is never dropped. 10 when absent, 0 for no
    /// cap.
    pub per_day: u32,
    /// The most triage calls on one author's issues in any 24 hours, up to 200,
    /// logins compared ignoring case. An issue past it is labelled
    /// `needs-triage` with no call and no comment. 2 when absent, 0 for no cap.
    pub per_author_per_day: u32,
    /// An author whose account is younger than this many days, and who has no
    /// history in the repo, gets `needs-triage` with no call and no comment.
    /// 30 when absent, 0 for no check.
    pub min_account_days: u32,
    /// The most kilobytes of an issue body that reach the triager, which the
    /// call is given cut at that size with a line saying so. The title is
    /// never cut. 20 when absent, 0 sends the whole body.
    pub max_body_kb: u32,
    /// The role of each login listed, in place of the one its association
    /// implies: `{ "login" = "reporter" }`, the roles being `maintainer`,
    /// `developer` and `reporter`. The association says how someone relates to
    /// the repo, not what they can answer. Logins are compared ignoring case.
    /// No login is overridden when absent.
    pub authors: BTreeMap<String, AuthorRole>,
    /// The classifier check each issue goes through after the limits and
    /// before its research call. No check when the table is absent.
    #[serde(default)]
    pub spam: TriageSpam,
}

impl Default for Triage {
    fn default() -> Self {
        Self {
            pick: TriagePick::default(),
            hand_ready: false,
            wait_days: NonZeroU32::new(7).unwrap_or(NonZeroU32::MIN),
            max_questions: NonZeroU8::new(MOST_QUESTIONS).unwrap_or(NonZeroU8::MIN),
            agent: AgentName::kelpies(TRIAGER),
            per_day: 10,
            per_author_per_day: 2,
            min_account_days: 30,
            max_body_kb: 20,
            authors: BTreeMap::new(),
            spam: TriageSpam::default(),
        }
    }
}

impl Triage {
    /// Whether kelpie triages no one's issue
    #[inline]
    pub fn is_off(&self) -> bool {
        self.pick == TriagePick::Off
    }

    /// The role `triage.authors` gives `login`, if it lists them
    pub fn role_of(&self, login: &str) -> Option<AuthorRole> {
        (self.authors.iter())
            .find(|(listed, _)| listed.eq_ignore_ascii_case(login))
            .map(|(_, role)| *role)
    }

    /// Checks the keys against their limits
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `triage.hand_ready` when it is on and
    /// `triage.pick` is `off`, `triage.max_questions` when it is above 10, or `triage.per_day` or `triage.per_author_per_day` when a cap is
    /// above 200, the most this file's `MOST_PER_DAY` allows and the number of
    /// finished triage records the state keeps, or `triage.spam.path` when
    /// `triage.spam.gateway` names a gateway and no route is given for it.
    pub fn check(&self) -> Result<(), SettingsError> {
        if self.hand_ready && self.is_off() {
            return Err(SettingsError::Invalid {
                setting: "triage.hand_ready",
                reason: "it takes back the maintainer's `ready-for-agent` issues for triage, and \
                         `triage.pick` is `off`: set `triage.pick` to `maintainer` or `all`, or \
                         turn `triage.hand_ready` off"
                    .to_owned(),
            });
        }
        if self.max_questions.get() > MOST_QUESTIONS {
            return Err(SettingsError::Invalid {
                setting: "triage.max_questions",
                reason: format!(
                    "it is {} and a comment asks at most {MOST_QUESTIONS}: set it from 1 to \
                     {MOST_QUESTIONS}",
                    self.max_questions
                ),
            });
        }
        // A cap the state cannot count is a cap that never engages, so it is
        // refused rather than quietly lowered.
        for (setting, cap) in [
            ("triage.per_day", self.per_day),
            ("triage.per_author_per_day", self.per_author_per_day),
        ] {
            if cap > MOST_PER_DAY {
                return Err(SettingsError::Invalid {
                    setting,
                    reason: format!(
                        "it is {cap} and kelpie keeps the last {TRIAGED_KEPT} finished triage \
                         records, a record that holds a call ahead of one that holds none, which \
                         is all a 24 hour count can see: set it from 0 to {MOST_PER_DAY}"
                    ),
                });
            }
        }
        if self.spam.gateway.is_some() && self.spam.path.is_none() {
            return Err(SettingsError::Invalid {
                setting: "triage.spam.path",
                reason: "`triage.spam.gateway` names a gateway and no route is given for its \
                         classifier: set `triage.spam.path` to the route, such as `/classify`"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// The spam check an issue goes through after the limits and before its
/// research call, `[app.dogs.kelpie.triage.spam]`: a text classifier the
/// project's gateway serves, asked whether the issue is spam
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TriageSpam {
    /// The gateway whose classifier the check asks, by name. No check when
    /// absent.
    pub gateway: Option<GatewayName>,
    /// The route its classifier is served on, such as `/classify`. Needed
    /// beside `gateway`, and refused without it.
    pub path: Option<ClassifierPath>,
    /// The probability at or above which an issue is read as spam, 0 to 1.
    /// 0.8 when absent.
    pub threshold: Probability,
}

impl Default for TriageSpam {
    fn default() -> Self {
        Self {
            gateway: None,
            path: None,
            threshold: SPAM_THRESHOLD,
        }
    }
}

impl TriageSpam {
    /// Whether issues go through no spam check
    #[inline]
    pub fn is_off(&self) -> bool {
        self.gateway.is_none()
    }

    /// Whether `p` is spam by this table's threshold
    #[inline]
    pub fn is_spam(&self, p: Probability) -> bool {
        p >= self.threshold
    }
}

/// The route a gateway serves its classifier on, as `triage.spam.path` gives
/// it. It goes in a request line, so it is one path: no space, no control
/// character, and no fragment, which would start a header of its own.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct ClassifierPath(String);

impl ClassifierPath {
    /// The route as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ClassifierPath {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let usable = value.starts_with('/')
            && !value
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '#');
        match usable {
            true => Ok(Self(value)),
            false => Err(
                "must be a route such as /classify, starting with `/` and holding no space, \
                 line break or `#`",
            ),
        }
    }
}

impl Settings {
    /// The triager the project names in `triage.agent`
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `triage.agent` when it has no agent
    /// file, its file's role is not `triager`, or `agents.allow` leaves it out.
    pub fn triager(&self, agents: &Agents) -> Result<ExpertAgent, SettingsError> {
        let at = "triage.agent";
        let name = &self.triage.agent;
        let agent = find(
            agents,
            name,
            at,
            at,
            Role::Triager,
            self.agents.allow.as_ref(),
        )?;
        let Runs::Session { model, limit } = &agent.runs else {
            unreachable!("a triager's file only parses to a session")
        };
        Ok(ExpertAgent {
            name: name.clone(),
            model: model.clone(),
            limit: limit.clone(),
            prompt: agent.prompt.clone(),
        })
    }
}
