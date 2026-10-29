//! A shepherd whose flock lives in memory, for `shep kelpie add`, `start`,
//! `pause` and `status`
//!
//! It keeps each sheep's config and status, answers the requests those
//! commands send as shep 0.12 does, and records every request. It is dialed
//! in memory, so a test that uses it binds no socket.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shep_client::shep_core::config::AppConfig;
use shep_client::shep_core::protocol::request::{
    ActionOutcome, ActionReply, DogSource, ProcessInfo, Response, SheepConfigView,
};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use super::Memory;
use crate::shepherd::{self, Connection, Dial, SHEP_VERSION};

/// One sheep: its config, whether it runs, and whether a dog holds its
/// name, which is `Some` saying whether that dog has the channel
#[derive(Debug, Clone)]
struct Sheep {
    config: AppConfig,
    online: bool,
    // Whether it shows as `Starting` rather than `Online` while it runs
    starting: bool,
    dog: Option<bool>,
    // Whether a runner just started is still taking its actions
    opening: bool,
    // What shep says of a dog's handshake, and whether it gave the dog up
    heard: Heard,
    // What its next `status` answers, `None` being a delivery that times out,
    // the last one repeating
    says: VecDeque<Option<String>>,
    // The same for its `handover` triggers, which a stand-in refuses until told
    hands: VecDeque<Option<String>>,
}

/// Whether a dog has named itself to the shepherd
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    Named,
    Silent,
    GivenUp,
}

/// The shepherd, its home, and what it was sent
#[derive(Debug)]
pub struct FakeShepherd {
    home: tempfile::TempDir,
    flock: Arc<Mutex<Vec<Sheep>>>,
    section: Arc<Mutex<String>>,
    memory: Memory,
    sent: UnboundedReceiver<Request>,
}

impl Default for FakeShepherd {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeShepherd {
    /// A shepherd on the pinned shep with an empty flock
    pub fn new() -> Self {
        Self::on(SHEP_VERSION)
    }

    /// A shepherd that says it runs `version`
    pub fn on(version: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let flock = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::clone(&flock);
        let section = Arc::new(Mutex::new(String::new()));
        let held = Arc::clone(&section);
        let (memory, sent) = Memory::answering(home.path(), version, move |r| {
            answer(&mut answers.lock().unwrap(), &held.lock().unwrap(), r)
        });
        Self {
            home,
            flock,
            section,
            memory,
            sent,
        }
    }

    /// Says from now on that this shepherd accepts no protocol below `oldest`
    pub fn accepts_from(&self, oldest: u32) {
        self.memory.accepts_from(oldest);
    }

    /// How to reach this shepherd, from the code under test
    pub fn dial(&self) -> Arc<dyn Dial> {
        Arc::new(self.memory.clone())
    }

    /// A connection to this shepherd, as kelpie makes one
    ///
    /// # Panics
    ///
    /// When kelpie refuses the shepherd's version.
    pub async fn connect(&self) -> Box<dyn Connection> {
        shepherd::connect(&self.memory, self.home()).await.unwrap()
    }

    /// The shep home the stand-in runs in
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// A folder inside the shepherd's scratch home, for a checkout or kelpie's home
    pub fn scratch(&self, name: &str) -> PathBuf {
        let folder = self.home.path().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// Puts `config` in the flock, running or not
    pub fn holds(&self, config: AppConfig, online: bool) {
        let sheep = Sheep {
            config,
            online,
            starting: false,
            dog: None,
            opening: false,
            heard: Heard::Named,
            says: VecDeque::new(),
            hands: VecDeque::new(),
        };
        self.flock.lock().unwrap().push(sheep);
    }

    /// Sets `key` in the `env` of the sheep named `name`
    pub fn sets_env(&self, name: &str, key: &str, value: &str) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.config.env.insert(key.to_owned(), value.to_owned());
    }

    /// Holds `toml` as kelpie's own `[kelpie]` section of `dogs.toml`
    pub fn holds_section(&self, toml: &str) {
        *self.section.lock().unwrap() = toml.to_owned();
    }

    /// The `[kelpie]` section `holds_section` last held
    pub fn section(&self) -> String {
        self.section.lock().unwrap().clone()
    }

    /// Has `name`, just started, answer its next trigger as a runner still opening
    pub fn just_started(&self, name: &str) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.opening = true;
    }

    /// Has the running sheep `name` show as starting, as one does before it is up
    pub fn starts(&self, name: &str) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.starting = true;
    }

    /// A way to have a sheep `name` run `program` from now on, as if it were
    /// pointed elsewhere, that another thread can hold while an upgrade
    /// blocks this one waiting on a build
    pub fn mover(&self) -> impl Fn(&str, &str) + Send + 'static {
        let flock = Arc::clone(&self.flock);
        move |name, program| {
            let mut flock = flock.lock().unwrap();
            let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
            program.clone_into(&mut sheep.config.script);
        }
    }

    /// Has `name` answer its `handover` triggers in turn, the last one from
    /// then on, as `replies` does for the others. A stand-in told nothing
    /// refuses, so a test that is not about handovers sees its runner drained.
    pub fn hands(&self, name: &str, answers: &[Option<&str>]) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.hands = answers.iter().map(|b| b.map(str::to_owned)).collect();
    }

    /// Has `name` answer its `status` triggers with `bodies` in turn, the last one from then on
    ///
    /// `{build}` in a body is the stamp of the file the sheep's program is
    /// when it is answered, as a runner shows the one it started from.
    pub fn says(&self, name: &str, bodies: &[&str]) {
        self.replies(name, &bodies.iter().map(|&b| Some(b)).collect::<Vec<_>>());
    }

    /// Has `name` answer its `status` triggers in turn, the last one from then
    /// on: `None` is a trigger the shepherd delivers and no reply comes back for
    pub fn replies(&self, name: &str, answers: &[Option<&str>]) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.says = answers.iter().map(|b| b.map(str::to_owned)).collect();
    }

    /// How many scripted `status` answers `name` has left, the last one counting
    /// as it repeats: a test waits on this for the looks it has had so far
    pub fn answers_left(&self, name: &str) -> usize {
        let flock = self.flock.lock().unwrap();
        let sheep = flock.iter().find(|s| s.config.name == name).unwrap();
        sheep.says.len()
    }

    /// Puts an adopted dog named `name` in the flock, running, with the
    /// shepherd channel or without
    pub fn holds_dog(&self, name: &str, channel: bool) {
        self.holds_dog_at(name, "/opt/kelpie", channel);
    }

    /// Puts an adopted dog named `name` that runs `program` in the flock, running
    pub fn holds_dog_at(&self, name: &str, program: &str, channel: bool) {
        let sheep = Sheep {
            config: AppConfig::minimal(name, program),
            online: true,
            starting: false,
            dog: Some(channel),
            opening: false,
            heard: Heard::Named,
            says: VecDeque::new(),
            hands: VecDeque::new(),
        };
        self.flock.lock().unwrap().push(sheep);
    }

    /// Has the dog named `name` run without ever naming itself in its handshake
    pub fn never_names(&self, name: &str) {
        self.hears(name, Heard::Silent);
    }

    /// Has shep give up on the dog named `name` after restarting it once for silence
    pub fn gives_up_on(&self, name: &str) {
        self.hears(name, Heard::GivenUp);
    }

    fn hears(&self, name: &str, heard: Heard) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.heard = heard;
    }

    /// Stops the sheep or dog named `name`, as a crash loop that gave up would
    pub fn stops(&self, name: &str) {
        let mut flock = self.flock.lock().unwrap();
        let sheep = flock.iter_mut().find(|s| s.config.name == name).unwrap();
        sheep.online = false;
    }

    /// The config of the sheep named `name`, and whether it runs
    pub fn sheep(&self, name: &str) -> Option<(AppConfig, bool)> {
        let flock = self.flock.lock().unwrap();
        let sheep = flock.iter().find(|s| s.config.name == name)?;
        Some((sheep.config.clone(), sheep.online))
    }

    /// The requests sent since the last look, leaving out the reads
    pub fn writes(&mut self) -> Vec<Request> {
        std::iter::from_fn(|| self.sent.try_recv().ok())
            .filter(|r| {
                !matches!(
                    r,
                    Request::ListFlock
                        | Request::DogSheepSettings { .. }
                        | Request::DogConfigResolved { .. }
                        | Request::SheepConfig { .. }
                )
            })
            .collect()
    }
}

fn info(id: usize, sheep: &Sheep) -> ProcessInfo {
    let status = if sheep.online && sheep.starting {
        ProcStatus::Starting
    } else if sheep.online {
        ProcStatus::Online
    } else {
        ProcStatus::Stopped
    };
    let mut info = ProcessInfo::builder(u32::try_from(id).unwrap(), &sheep.config.name, status);
    if let Some(channel) = sheep.dog {
        info = info
            .dog(Some(DogSource::Adopted {
                path: sheep.config.script.clone(),
                channel,
                grants: None,
            }))
            .handshook(Some(sheep.heard == Heard::Named))
            .dog_stale(Some(sheep.heard == Heard::GivenUp));
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
        Request::DogConfigResolved { .. } => Response::DogSection {
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
                        starting: false,
                        dog: None,
                        opening: false,
                        heard: Heard::Named,
                        says: VecDeque::new(),
                        hands: VecDeque::new(),
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
        Request::SetSheepField { name, key, value } => {
            let sheep = flock.iter_mut().find(|s| &s.config.name == name).unwrap();
            assert_eq!(key, "stop_exit_codes", "the stand-in sets only this field");
            sheep.config.stop_exit_codes = serde_json::from_value(value.clone()).unwrap();
            Response::SheepFieldSet {
                name: name.clone(),
                key: key.clone(),
                pending: false,
                warning: None,
            }
        }
        Request::Delete { selector } => {
            let at = named(flock, selector).unwrap();
            flock.remove(at);
            Response::Deleted(vec![u32::try_from(at).unwrap()])
        }
        Request::Stop { selector } => {
            let at = named(flock, selector).unwrap();
            flock[at].online = false;
            Response::Stopped(vec![info(at, &flock[at])])
        }
        Request::Restart { selector } => {
            let at = named(flock, selector).unwrap();
            flock[at].online = true;
            flock[at].starting = false;
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
            } else if action == crate::shepherd::WITHDRAW {
                ActionOutcome::Replied {
                    body: serde_json::json!({ "handover": "withdrawn" }).to_string(),
                }
            } else if action == "handover" {
                match scripted(&mut sheep.hands) {
                    Some(Some(body)) => ActionOutcome::Replied { body },
                    Some(None) => ActionOutcome::TimedOut,
                    None => ActionOutcome::Replied {
                        body: serde_json::json!({
                            "error": "handover refused: the stand-in runner does not hand over"
                        })
                        .to_string(),
                    },
                }
            } else if let Some(answer) = scripted(&mut sheep.says) {
                match answer {
                    Some(body) => {
                        let build = crate::home::stamp_of(Path::new(&sheep.config.script))
                            .unwrap_or_default();
                        ActionOutcome::Replied {
                            body: body.replace("{build}", &build),
                        }
                    }
                    None => ActionOutcome::TimedOut,
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

// The next answer a sheep was told to give, the last one repeating.
fn scripted(says: &mut VecDeque<Option<String>>) -> Option<Option<String>> {
    match says.len() {
        0 => None,
        1 => says.front().cloned(),
        _ => says.pop_front(),
    }
}
