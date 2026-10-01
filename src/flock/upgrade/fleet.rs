//! Kelpie's sheep on the shepherd, as an upgrade restarts them

use std::path::PathBuf;

use shep_client::Client;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::{DogSource, Response};
use shep_client::shep_core::status::ProcStatus;

use super::{Fleet, Kind, Member};
use crate::flock::{control, dog_problem, flock, resume, tables};
use crate::shepherd;

/// The shepherd at one home, asked afresh for each question
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shepherd {
    home: PathBuf,
}

impl Shepherd {
    /// The shepherd whose home is `home`
    pub fn at(home: PathBuf) -> Self {
        Self { home }
    }

    fn ask<T>(
        &self,
        question: impl AsyncFnOnce(&Client) -> Result<T, String>,
    ) -> Result<T, String> {
        shepherd::block_on(async {
            let client = shepherd::connect(&self.home)
                .await
                .map_err(|e| e.describe(&self.home))?;
            question(&client).await
        })
    }
}

impl Fleet for Shepherd {
    fn shepherd_version(&self) -> Result<String, String> {
        shepherd::block_on(shepherd::running_version(&self.home))
    }

    fn members(&self) -> Result<Vec<Member>, String> {
        self.ask(async |client| {
            let rows = flock(client).await?;
            let mut members = Vec::new();
            let dog = rows.iter().find(|r| r.name == crate::dog::NAME);
            if let Some((row, DogSource::Adopted { path, .. })) =
                dog.and_then(|row| Some((row, row.dog.as_ref()?)))
            {
                members.push(Member {
                    name: row.name.clone(),
                    kind: Kind::Dog,
                    program: PathBuf::from(path),
                    online: row.status == ProcStatus::Online,
                });
            }
            for name in tables(client).await?.into_keys() {
                let Some(row) = rows.iter().find(|r| r.name == name && r.dog.is_none()) else {
                    continue;
                };
                let request = Request::SheepConfig { name: name.clone() };
                let Ok(Response::SheepConfig(view)) = client.request(request).await else {
                    continue;
                };
                if view.config.args == ["runner", name.as_str()] {
                    members.push(Member {
                        name,
                        kind: Kind::Runner,
                        program: PathBuf::from(&view.config.script),
                        online: row.status == ProcStatus::Online,
                    });
                }
            }
            Ok(members)
        })
    }

    fn merging(&self, name: &str) -> Result<bool, String> {
        match self.ask(async |client| control::status_body(client, name).await)? {
            Some(body) => merge_in_flight(&body),
            None => Ok(true),
        }
    }

    fn restart(&self, member: &Member) -> Result<(), String> {
        self.ask(async |client| resume(client, &member.name).await)
    }

    fn up(&self, member: &Member) -> Result<bool, String> {
        self.ask(async |client| match member.kind {
            Kind::Dog => Ok(dog_problem(&flock(client).await?).is_none()),
            Kind::Runner => Ok(control::status_body(client, &member.name).await?.is_some()),
        })
    }
}

/// Whether a runner's `status` answer has a work item in its merge phase,
/// or one the merge queue holds
///
/// # Errors
///
/// A message when the answer has no list of work items.
pub fn merge_in_flight(status: &str) -> Result<bool, String> {
    let answer: serde_json::Value =
        serde_json::from_str(status).map_err(|e| format!("status was not JSON: {e}"))?;
    let items = answer["work_items"]
        .as_array()
        .ok_or("status named no work items")?;
    Ok(items
        .iter()
        .any(|item| item["phase"]["state"] == "merge" || !item["merge_queued"].is_null()))
}
