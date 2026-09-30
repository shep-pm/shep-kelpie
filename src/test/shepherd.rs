//! A shepherd whose flock lives in memory, for `shep kelpie add`, `start`,
//! `pause` and `status`
//!
//! It keeps each sheep's config and status, answers the requests those
//! commands send as shep 0.12 does, and records every request.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shep_client::shep_core::config::AppConfig;
use shep_client::shep_core::protocol::request::{
    ActionOutcome, ActionReply, DogSource, ProcessInfo, Response, SheepConfigView,
};
use shep_client::shep_core::protocol::{Envelope, Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::shepherd::SHEP_VERSION;

/// One sheep: its config, whether it runs, and whether a dog holds its
/// name, which is `Some` saying whether that dog has the channel
#[derive(Debug, Clone)]
struct Sheep {
    config: AppConfig,
    online: bool,
    dog: Option<bool>,
    // Whether a runner just started is still taking its actions
    opening: bool,
}

/// The shepherd, its home, and what it was sent
pub(crate) struct FakeShepherd {
    home: tempfile::TempDir,
    flock: Arc<Mutex<Vec<Sheep>>>,
    section: Arc<Mutex<String>>,
    sent: UnboundedReceiver<Envelope>,
}

impl FakeShepherd {
    /// A shepherd on the pinned shep with an empty flock
    pub(crate) async fn new() -> Self {
        Self::on(SHEP_VERSION).await
    }

    /// A shepherd that says it runs `version`
    pub(crate) async fn on(version: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("run")).unwrap();
        let mut ack = sample_ack();
        ack.daemon_version = version.into();
        let flock = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::clone(&flock);
        let section = Arc::new(Mutex::new(String::new()));
        let held = Arc::clone(&section);
        let sent =
            fake_daemon_answering_with_ack(&home.path().join("run/shep.sock"), ack, move |r| {
                answer(&mut answers.lock().unwrap(), &held.lock().unwrap(), r)
            })
            .await;
        Self {
            home,
            flock,
            section,
            sent,
        }
    }

    pub(crate) fn home(&self) -> &Path {
        self.home.path()
    }

    /// A folder inside the shepherd's scratch home, for a checkout or kelpie's home
    pub(crate) fn scratch(&self, name: &str) -> PathBuf {
        let folder = self.home.path().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// Puts `config` in the flock, running or not
    pub(crate) fn holds(&self, config: AppConfig, online: bool) {
        let sheep = Sheep {
            config,
            online,
            dog: None,
            opening: false,
        };
        self.flock.lock().unwrap().push(sheep);
    }

    /// Holds `toml` as kelpie's own `[kelpie]` section of `dogs.toml`
    pub(crate) fn holds_section(&self, toml: &str) {
        *self.section.lock().unwrap() = toml.to_owned();
    }

    /// Has `name`, just started, answer its next trigger as a runner still opening
    pub(crate) fn just_started(&self, name: &str) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.opening = true;
    }

    /// Puts an adopted dog named `name` in the flock, running, with the
    /// shepherd channel or without
    pub(crate) fn holds_dog(&self, name: &str, channel: bool) {
        let sheep = Sheep {
            config: AppConfig::minimal(name, "/opt/kelpie"),
            online: true,
            dog: Some(channel),
            opening: false,
        };
        self.flock.lock().unwrap().push(sheep);
    }

    /// Stops the sheep or dog named `name`, as a crash loop that gave up would
    pub(crate) fn stops(&self, name: &str) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.online = false;
    }

    /// The config of the sheep named `name`, and whether it runs
    pub(crate) fn sheep(&self, name: &str) -> Option<(AppConfig, bool)> {
        let flock = self.flock.lock().unwrap();
        let sheep = flock.iter().find(|s| s.config.name == name)?;
        Some((sheep.config.clone(), sheep.online))
    }

    /// The requests sent since the last look, leaving out the reads
    pub(crate) fn writes(&mut self) -> Vec<Request> {
        std::iter::from_fn(|| self.sent.try_recv().ok())
            .map(|e| e.body)
            .filter(|r| {
                !matches!(
                    r,
                    Request::ListFlock
                        | Request::DogSheepSettings { .. }
                        | Request::DogConfig { .. }
                        | Request::SheepConfig { .. }
                )
            })
            .collect()
    }
}

fn info(id: usize, sheep: &Sheep) -> ProcessInfo {
    let status = if sheep.online {
        ProcStatus::Online
    } else {
        ProcStatus::Stopped
    };
    let mut info = ProcessInfo::builder(u32::try_from(id).unwrap(), &sheep.config.name, status);
    if let Some(channel) = sheep.dog {
        info = info.dog(Some(DogSource::Adopted {
            path: sheep.config.script.clone(),
            channel,
        }));
    }
    info.build()
}

fn named(flock: &[Sheep], selector: &SelectorSpec) -> Option<usize> {
    let SelectorSpec::Name(name) = selector else {
        panic!("kelpie named no sheep: {selector:?}");
    };
    flock.iter().position(|s| &s.config.name == name)
}

fn answer(flock: &mut Vec<Sheep>, section: &str, request: &Request) -> Response {
    let rows = |flock: &[Sheep]| flock.iter().enumerate().map(|(i, s)| info(i, s)).collect();
    match request {
        Request::ListFlock => Response::Flock(rows(flock)),
        Request::DogSheepSettings { dog } => Response::DogSheepSettings {
            tables: flock
                .iter()
                .filter_map(|s| Some((s.config.name.clone(), s.config.dogs.get(dog)?.clone())))
                .collect(),
        },
        Request::DogConfig { .. } => Response::DogSection {
            toml: section.to_owned().into(),
        },
        Request::SheepConfig { name } => {
            let sheep = flock.iter().find(|s| &s.config.name == name).unwrap();
            Response::SheepConfig(Box::new(SheepConfigView::new(
                sheep.config.clone(),
                Vec::new(),
                Vec::new(),
            )))
        }
        Request::Add { apps } => {
            for app in apps {
                if !flock.iter().any(|s| s.config.name == app.name) {
                    flock.push(Sheep {
                        config: app.clone(),
                        online: false,
                        dog: None,
                        opening: false,
                    });
                }
            }
            Response::Added(rows(flock))
        }
        Request::SetSheepDogSettings { name, dog, table } => {
            let sheep = flock.iter_mut().find(|s| &s.config.name == name).unwrap();
            match table {
                Some(table) => sheep.config.dogs.insert(dog.clone(), table.clone()),
                None => sheep.config.dogs.remove(dog),
            };
            Response::SheepDogSettingsSet {
                name: name.clone(),
                dog: dog.clone(),
            }
        }
        Request::Delete { selector } => {
            let at = named(flock, selector).unwrap();
            flock.remove(at);
            Response::Deleted(vec![u32::try_from(at).unwrap()])
        }
        Request::Restart { selector } => {
            let at = named(flock, selector).unwrap();
            flock[at].online = true;
            flock[at].opening = true;
            Response::Restarted {
                accepted: vec![info(at, &flock[at])],
                refused: Vec::new(),
            }
        }
        Request::Trigger {
            selector,
            action,
            params,
        } => {
            let Some(at) = named(flock, selector) else {
                return Response::Triggered(Vec::new());
            };
            let sheep = &mut flock[at];
            // A runner opens its channel before it takes its actions, and
            // shep-channel answers an action nobody took in plain text.
            let outcome = if sheep.dog == Some(false) {
                ActionOutcome::DogNoChannel
            } else if !sheep.online {
                ActionOutcome::NoChannel
            } else if sheep.opening {
                sheep.opening = false;
                ActionOutcome::Replied {
                    body: format!("unknown action: {action}"),
                }
            } else {
                let mut body = serde_json::json!({ "sheep": sheep.config.name, "action": action });
                if let Some(params) = params {
                    body["params"] = params.as_str().into();
                }
                ActionOutcome::Replied {
                    body: body.to_string(),
                }
            };
            Response::Triggered(vec![ActionReply {
                id: u32::try_from(at).unwrap(),
                name: sheep.config.name.clone(),
                outcome,
            }])
        }
        other => panic!("kelpie asked {other:?}"),
    }
}
