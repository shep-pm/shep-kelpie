//! How rulings are decided, `[app.dogs.kelpie.rulings]`

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use super::SettingsError;
use crate::state::{Held, RulingKind, Stuck};

/// The setting that says which kinds of ruling the project manager decides
pub const PM_DECIDES: &str = "rulings.pm_decides";

/// The setting that lists kinds the project manager decides, each read as
/// `true` in `rulings.pm_decides`
pub const PM_ALONE: &str = "rulings.pm_alone";

/// How rulings are decided
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "Written")]
pub struct Rulings {
    /// Whether the project manager decides each kind of ruling, `true`, or
    /// the maintainer does. A kind it does not name is the maintainer's.
    pub pm_decides: BTreeMap<RulingName, bool>,
    // Whether `pm_decides` was read from the old list, for the lines that name the key
    #[schemars(skip)]
    from_list: bool,
}

/// How rulings are decided
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Written {
    /// Whether the project manager answers each kind of ruling, `true`, when
    /// it flags no `design` and does not abstain, or the maintainer does,
    /// `false`. A kind it does not name
    /// is the maintainer's. `merge`, `security`, `contribution-merge` and
    /// `local-model-spilled` are refused as `true`.
    #[serde(default, deserialize_with = "named_map")]
    #[schemars(with = "BTreeMap<RulingName, bool>")]
    pm_decides: Option<BTreeMap<RulingName, bool>>,
    /// Kinds the project manager decides alone, each read as `true` in
    /// `pm_decides`. Refused beside `pm_decides`.
    #[serde(default, deserialize_with = "named_list")]
    #[schemars(skip)]
    pm_alone: Option<Vec<RulingName>>,
}

/// The kind `unlabelled`, which the loader refuses by name and migrate deletes
const UNLABELLED: &str = "unlabelled";

impl TryFrom<Written> for Rulings {
    type Error = String;

    fn try_from(written: Written) -> Result<Self, String> {
        let (pm_decides, from_list) = match (written.pm_decides, written.pm_alone) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "set `{PM_DECIDES}` or the old `{PM_ALONE}`, not both: move the list's \
                     kinds into the map as `true`"
                ));
            }
            (Some(map), None) => (map, false),
            (None, Some(list)) => (list.into_iter().map(|kind| (kind, true)).collect(), true),
            (None, None) => (BTreeMap::new(), false),
        };
        Ok(Self {
            pm_decides,
            from_list,
        })
    }
}

// The map's kinds, each read by `RulingName::named`, which refuses `unlabelled` by name.
fn named_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BTreeMap<RulingName, bool>>, D::Error> {
    let written = BTreeMap::<String, bool>::deserialize(deserializer)?;
    let named = written
        .into_iter()
        .map(|(kind, decides)| RulingName::named(&kind).map(|kind| (kind, decides)));
    named
        .collect::<Result<_, _>>()
        .map(Some)
        .map_err(D::Error::custom)
}

// The list's kinds, read as the map's are.
fn named_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<RulingName>>, D::Error> {
    let written = Vec::<String>::deserialize(deserializer)?;
    let named = written.iter().map(|kind| RulingName::named(kind));
    named
        .collect::<Result<_, _>>()
        .map(Some)
        .map_err(D::Error::custom)
}

/// A kind of ruling, as `rulings.pm_decides` and the usage ledger name it
// wire format: changing this is a breaking change to the rulings file
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RulingName {
    /// A merge: CI is green on a head and a yes merges it
    Merge,
    /// The worker's question
    Question,
    /// A pull request changes agents' own files
    AgentFiles,
    /// An agent does clearly worse at a kind of work than another agent that
    /// takes it. Neither answer changes a file.
    AgentFit,
    /// A pull request's labels or ready state changed outside kelpie
    ForeignChange,
    /// A merged pull request left findings unfixed
    FollowUp,
    /// The branch could not be rebased
    Rebase,
    /// CI is still red
    StillRed,
    /// A check stayed queued after its run was rerun
    CheckQueued,
    /// A head got no check
    NoChecks,
    /// The forge refused a merge twice
    MergeRefused,
    /// The pull request was closed
    Closed,
    /// A fix turn ended without a push
    FixNotPushed,
    /// A reviewer's test still fails after two fixes
    TestStillFails,
    /// Work in the worktree is not pushed
    Unpushed,
    /// A contributor's fork refused kelpie's push of the worker's commits
    PushRefused,
    /// A turn ran past its ceiling
    TurnTimeout,
    /// A turn failed
    TurnFailed,
    /// An adopted pull request could not start
    AdoptFailed,
    /// A work item or a contribution is over its budget
    OverBudget,
    /// The security review flagged a contribution's head. The project
    /// manager never answers it, so `pm_decides` refuses it.
    Security,
    /// A contribution's pass has ended and its CI is green on the head. The
    /// maintainer decides every merge of someone else's work, so
    /// `pm_decides` refuses it.
    ContributionMerge,
    /// The local model sat on the CPU. No ruling is raised for it now, and
    /// the name stays so ledger and history lines written before it went
    /// still read; `pm_decides` refuses it.
    LocalModelSpilled,
}

impl RulingName {
    /// Every kind, in the order the enum lists them
    pub const ALL: [Self; 23] = [
        Self::Merge,
        Self::Question,
        Self::AgentFiles,
        Self::AgentFit,
        Self::ForeignChange,
        Self::FollowUp,
        Self::Rebase,
        Self::StillRed,
        Self::CheckQueued,
        Self::NoChecks,
        Self::MergeRefused,
        Self::Closed,
        Self::FixNotPushed,
        Self::TestStillFails,
        Self::Unpushed,
        Self::PushRefused,
        Self::TurnTimeout,
        Self::TurnFailed,
        Self::AdoptFailed,
        Self::OverBudget,
        Self::Security,
        Self::ContributionMerge,
        Self::LocalModelSpilled,
    ];

    /// What `rulings.pm_decides` starts this kind at, or none for a kind
    /// the project manager is refused and that is never written
    pub fn pm_default(self) -> Option<bool> {
        match self {
            Self::Merge | Self::Security | Self::ContributionMerge | Self::LocalModelSpilled => {
                None
            }
            Self::Question | Self::ForeignChange | Self::AgentFiles | Self::AgentFit => Some(false),
            Self::FollowUp
            | Self::Rebase
            | Self::StillRed
            | Self::CheckQueued
            | Self::NoChecks
            | Self::MergeRefused
            | Self::Closed
            | Self::FixNotPushed
            | Self::TestStillFails
            | Self::Unpushed
            | Self::PushRefused
            | Self::TurnTimeout
            | Self::TurnFailed
            | Self::AdoptFailed
            | Self::OverBudget => Some(true),
        }
    }

    // Why the project manager never decides this kind, for the kinds it never does.
    fn refused_because(self) -> Option<&'static str> {
        match self {
            Self::Merge => Some("a merge ruling is never decided by the project manager alone"),
            Self::Security => Some(
                "a ruling on a contribution never goes to the project manager, since text from \
                 a stranger must not reach it",
            ),
            Self::ContributionMerge => Some(
                "the maintainer decides every merge of someone else's work, and a contribution's \
                 ruling never goes to the project manager",
            ),
            Self::LocalModelSpilled => Some(
                "`local-model-spilled` is no longer a kind of ruling, since kelpie no longer \
                 checks where a local model sits",
            ),
            _ => None,
        }
    }

    /// The kind `name` spells, or why it names none. `unlabelled` is refused,
    /// naming `settings migrate`.
    ///
    /// # Errors
    ///
    /// A message naming `name` when no kind has it.
    pub fn named(name: &str) -> Result<Self, String> {
        if name == UNLABELLED {
            return Err(format!(
                "`{UNLABELLED}` is no longer a kind of ruling, since the issue writer that raised \
                 it is gone: delete it, or run `shep kelpie settings migrate`, which does"
            ));
        }
        (Self::ALL.into_iter())
            .find(|kind| kind.as_str() == name)
            .ok_or_else(|| {
                let names = Self::ALL.map(Self::as_str);
                format!(
                    "unknown variant `{name}`, expected one of `{}`",
                    names.join("`, `")
                )
            })
    }

    /// The kind of `kind`
    pub fn of(kind: &RulingKind) -> Self {
        match kind {
            RulingKind::Merge { .. } => Self::Merge,
            RulingKind::Question { .. } => Self::Question,
            RulingKind::AgentFiles { .. } => Self::AgentFiles,
            RulingKind::AgentFit(..) => Self::AgentFit,
            RulingKind::ForeignChange { .. } => Self::ForeignChange,
            RulingKind::FollowUp { .. } => Self::FollowUp,
            RulingKind::Contribution(held) => match held {
                Held::Security { .. } => Self::Security,
                Held::OverBudget { .. } => Self::OverBudget,
                Held::Question { .. }
                | Held::Waiting
                | Held::NoEdits
                | Held::CannotAdopt { .. } => Self::Question,
                Held::Merge { .. } => Self::ContributionMerge,
            },
            RulingKind::Stuck(stuck) => match stuck {
                Stuck::Rebase { .. } => Self::Rebase,
                Stuck::StillRed { .. } => Self::StillRed,
                Stuck::CheckQueued { .. } => Self::CheckQueued,
                Stuck::NoChecks { .. } => Self::NoChecks,
                Stuck::MergeRefused { .. } => Self::MergeRefused,
                Stuck::Closed => Self::Closed,
                Stuck::FixNotPushed { .. } => Self::FixNotPushed,
                Stuck::TestStillFails { .. } => Self::TestStillFails,
                Stuck::Unpushed { .. } => Self::Unpushed,
                Stuck::PushRefused { .. } => Self::PushRefused,
                Stuck::TurnTimeout { .. } => Self::TurnTimeout,
                Stuck::TurnFailed { .. } => Self::TurnFailed,
                Stuck::AdoptFailed { .. } => Self::AdoptFailed,
                Stuck::OverBudget { .. } => Self::OverBudget,
            },
        }
    }

    /// The name as the settings and the ledger spell it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Question => "question",
            Self::AgentFiles => "agent-files",
            Self::AgentFit => "agent-fit",
            Self::ForeignChange => "foreign-change",
            Self::FollowUp => "follow-up",
            Self::Rebase => "rebase",
            Self::StillRed => "still-red",
            Self::CheckQueued => "check-queued",
            Self::NoChecks => "no-checks",
            Self::MergeRefused => "merge-refused",
            Self::Closed => "closed",
            Self::FixNotPushed => "fix-not-pushed",
            Self::TestStillFails => "test-still-fails",
            Self::Unpushed => "unpushed",
            Self::PushRefused => "push-refused",
            Self::TurnTimeout => "turn-timeout",
            Self::TurnFailed => "turn-failed",
            Self::AdoptFailed => "adopt-failed",
            Self::OverBudget => "over-budget",
            Self::Security => "security",
            Self::ContributionMerge => "contribution-merge",
            Self::LocalModelSpilled => "local-model-spilled",
        }
    }
}

impl Rulings {
    /// Whether the project manager alone decides a ruling of `kind`
    pub fn pm_decides(&self, kind: RulingName) -> bool {
        self.pm_decides.get(&kind) == Some(&true)
    }

    // The key the kinds were read from, as the lines that name it spell it.
    fn key(&self) -> &'static str {
        match self.from_list {
            true => PM_ALONE,
            false => PM_DECIDES,
        }
    }

    /// Why the kinds are refused, if they are, naming the key they were read
    /// from: a kind the project manager never decides is set to `true`
    pub fn refusal(&self) -> Option<SettingsError> {
        let (kind, because) = (RulingName::ALL.into_iter())
            .filter(|kind| self.pm_decides(*kind))
            .find_map(|kind| Some((kind, kind.refused_because()?)))?;
        let fix = match self.from_list {
            true => "out of the list",
            false => "out of the map, or set it to false",
        };
        Some(SettingsError::Invalid {
            setting: self.key(),
            reason: format!("{because}: take `{}` {fix}", kind.as_str()),
        })
    }
}
