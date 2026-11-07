//! What `status` shows of the runner's own state

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use shep_kelpie_core::pacer::{Hold, Reading};
use shep_kelpie_core::ports::{Gpu, Timestamp};
use shep_kelpie_core::settings::Account;
use shep_kelpie_core::ticket::TicketId;

/// What `status` shows of the pacer
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PacerStatus {
    /// What an older runner said `pacing.enabled` was, which nothing reads and
    /// no runner sends. It is read so a runner not yet restarted answers, and
    /// left out of what this one sends: the accounts below are flattened, so
    /// an `enabled` key read as an account would fail the whole reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Each account the worker spends or a reading was taken of, by its name
    #[serde(flatten)]
    pub accounts: BTreeMap<Account, AccountStatus>,
}

/// What `status` shows of one account's usage
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStatus {
    /// The last time its usage was read, and what it makes of it
    pub reading: Option<Reading>,
    /// Why nothing new is starting on it, while a hold lasts
    pub holding: Option<Hold>,
}

/// The GPU, as the metrics page last said
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuStatus {
    /// Each GPU on the page
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<Gpu>,
    /// Why the page could not be read, or that it has not been yet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Seconds since that read ended, once there has been one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
}

/// What `status` shows while the runner drains
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draining {
    /// The calls still running: each work item's, then the project manager's
    pub calls: Vec<CallRunning>,
    /// The longest a call the runner starts may run before it is ended, in
    /// seconds
    pub ceiling: u64,
    /// The work items whose worker turn ends at its next tool call
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at_tool: Vec<TicketId>,
}

/// A call in flight
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRunning {
    /// The ticket of the work item it is for, and none for the project manager's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<TicketId>,
    /// Whose call it is
    pub role: CallRole,
}

/// Whose call is in flight, as `status` names it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallRole {
    /// A worker's turn
    Worker,
    /// A reviewer's session or local round
    Reviewer,
    /// The project manager's wake
    Pm,
    /// The security review, reading a contribution's head
    Security,
    /// The triager, researching an untouched issue
    Triager,
    /// The project manager's agent, running a batch retro
    Retro,
    /// The e2e run of a dependency bump's head
    E2e,
    /// A session in the maintainer's terminal, as its harness's facts name it
    Attached(String),
}

impl CallRole {
    /// The name `status` shows
    pub fn name(&self) -> String {
        match self {
            Self::Worker => "worker".to_owned(),
            Self::Reviewer => "reviewer".to_owned(),
            Self::Pm => "pm".to_owned(),
            Self::Security => "security".to_owned(),
            Self::Triager => "triager".to_owned(),
            Self::Retro => "retro".to_owned(),
            Self::E2e => "e2e".to_owned(),
            Self::Attached(name) => name.clone(),
        }
    }
}

impl Serialize for CallRole {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.name())
    }
}

impl<'de> Deserialize<'de> for CallRole {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "worker" => Self::Worker,
            "reviewer" => Self::Reviewer,
            "pm" => Self::Pm,
            "security" => Self::Security,
            "triager" => Self::Triager,
            "retro" => Self::Retro,
            "e2e" => Self::E2e,
            other => Self::Attached(other.to_owned()),
        })
    }
}

/// The reason the stored settings were refused, and since when
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// What did not hold
    pub reason: String,
    /// When it was first refused, in seconds since the Unix epoch
    pub at: Timestamp,
}

/// A workflow red on `main`, as `status` shows it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedStatus {
    /// The workflow's name
    pub workflow: String,
    /// The forge's id for the first red run kelpie read since it last read the
    /// workflow green
    pub first_run: u64,
    /// The forge's id for its latest red run
    pub run: u64,
    /// The commit the latest red run ran on
    pub head: String,
    /// When `first_run` was created
    pub since: Timestamp,
    /// How long it has been red, in seconds
    pub red_for: u64,
    /// The latest red run's page on the forge
    pub url: String,
    /// What is being done about the latest red head
    pub doing: String,
}

/// Where a repo's pull request template comes from
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateSource {
    /// The repo's own
    Repo,
    /// The owner's `.github` repository, named here
    Owner(String),
    /// None: the worker writes the body with the `pr` skill
    None,
}

impl fmt::Display for TemplateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repo => f.write_str("the repo's own"),
            Self::Owner(repo) => write!(f, "the owner's, from {repo}"),
            Self::None => f.write_str("none"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Status;
    use crate::status::tests::{recorded, recorded_status};

    // What a runner not yet restarted sends for `claude`: what `pacing.enabled`
    // was, and the three figures of the day each reading used to carry
    const OLDER: &str = r#"{"enabled":true,"claude":{"reading":{"at":300,"session":{"used_pct":3,"resets_at":400},"week":{"used_pct":15,"resets_at":500},"allowance_pct":14.3,"spent_today_pct":15,"per_hour_pct":1.8},"holding":null}}"#;

    #[test]
    fn a_status_from_a_runner_not_yet_restarted_still_reads() {
        let mut shown = recorded();
        shown["pacer"] = serde_json::from_str(OLDER).expect("the older pacer is JSON");
        let status: Status =
            serde_json::from_value(shown).expect("a runner not yet restarted still answers");
        assert_eq!(status.pacer.enabled, Some(true));
        let claude = &status.pacer.accounts[&Account::Claude];
        assert_eq!(claude.reading.unwrap().week.used_pct, 15);
        assert_eq!(claude.holding, None);
    }

    #[test]
    fn a_new_status_sends_no_enabled_key() {
        let status = recorded_status();
        assert_eq!(status.pacer.enabled, None);
        let shown = serde_json::to_value(&status).unwrap();
        assert_eq!(shown["pacer"].get("enabled"), None, "{shown}");
    }
}
