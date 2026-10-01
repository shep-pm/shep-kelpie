//! The stand-in Claude: what it answers, and the calls it saw

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::{FakeMeter, git, write_in};
use crate::ports::{AgentCall, AgentError, AgentReply, Agents, Cost, Role, Usage, Utilization};

/// The file a killed worker leaves in its worktree, to find after a restart
pub(crate) const LEFT_BEHIND: &str = "left-behind.txt";

/// What the stand-in Claude does with its next call
#[derive(Debug, Clone)]
pub(crate) enum Scripted {
    /// Answers with this usage, and this cost for the session so far
    Reply(Usage, Cost),
    /// Answers with this usage and no cost, as a harness that reports
    /// none does
    Tokens(Usage),
    /// Answers like [`Self::Reply`], with the account's usage as given by
    /// the time it does: the call itself spent it
    Spend(Utilization, Usage, Cost),
    /// Fails with this error
    Fail(AgentError),
    /// Leaves [`LEFT_BEHIND`] in the worktree, then dies with the runner
    Kill,
    /// Commits this file with this text on the worktree's branch, pushes
    /// it the way a worker does, and answers
    Push(&'static str, &'static str),
    /// Writes this file with this text in the worktree, commits nothing,
    /// and answers: a write that got past the fence
    Plant(&'static str, &'static str),
    /// Merges `origin/main` into the worktree's branch, keeping main's
    /// side of any conflict, and pushes it without force, as a worker
    /// resolving a conflict does
    MergeMain,
    /// Answers with this exact text and no cost: a review round or judge
    /// one-shot, whose reply is read rather than acted on
    Text(&'static str),
    /// Answers like [`Self::Text`], with this cost for the session
    Billed(&'static str, Cost),
    /// Answers with this final message
    Say(&'static str),
    /// Blocks until the test releases it, then answers
    Hold(Hold),
}

/// A call in flight that a test lets go of when it chooses
#[derive(Debug, Clone, Default)]
pub(crate) struct Hold(Arc<(Mutex<Held>, Condvar)>);

#[derive(Debug, Default)]
struct Held {
    entered: bool,
    released: bool,
    returned: bool,
}

impl Hold {
    /// Waits up to `within` for the call to begin, and says whether it did
    pub(crate) fn entered(&self, within: Duration) -> bool {
        let (held, changed) = &*self.0;
        let held = held.lock().unwrap();
        let (held, _) = changed
            .wait_timeout_while(held, within, |h| !h.entered)
            .unwrap();
        held.entered
    }

    /// Lets the call answer
    pub(crate) fn release(&self) {
        let (held, changed) = &*self.0;
        held.lock().unwrap().released = true;
        changed.notify_all();
    }

    /// Whether the call has answered
    pub(crate) fn returned(&self) -> bool {
        self.0.0.lock().unwrap().returned
    }

    /// Waits up to `within` for the call to answer, and says whether it did
    pub(crate) fn answered(&self, within: Duration) -> bool {
        let (held, changed) = &*self.0;
        let held = held.lock().unwrap();
        let (held, _) = changed
            .wait_timeout_while(held, within, |h| !h.returned)
            .unwrap();
        held.returned
    }

    fn block(&self) {
        let (held, changed) = &*self.0;
        let mut held = held.lock().unwrap();
        held.entered = true;
        changed.notify_all();
        let mut held = changed.wait_while(held, |h| !h.released).unwrap();
        held.returned = true;
        changed.notify_all();
    }
}

/// A call as the stand-in Claude saw it
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    /// The call
    pub(crate) call: AgentCall,
    /// The settings file it named, as it stood during the call
    pub(crate) settings: serde_json::Value,
    /// The sandbox runtime's settings for its fence, as the real adapter writes them
    pub(crate) sandbox: serde_json::Value,
    /// Whether its build folder existed when the call started
    pub(crate) build_existed: bool,
}

/// Records every call and answers from a script, failing once it runs out
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeClaude {
    seen: Arc<Mutex<Vec<Seen>>>,
    script: Arc<Mutex<VecDeque<Scripted>>>,
    meter: Option<FakeMeter>,
}

impl FakeClaude {
    /// One whose calls spend what `meter` is told they do
    pub(crate) fn metered(meter: FakeMeter) -> Self {
        Self {
            meter: Some(meter),
            ..Self::default()
        }
    }

    /// The worker's own calls, in order: what every test before the review
    /// loop existed already asserted on, so a reviewer or judge call never
    /// shows up and shifts their counts.
    pub(crate) fn calls(&self) -> Vec<AgentCall> {
        self.seen().into_iter().map(|s| s.call).collect()
    }

    /// The worker's own calls, with the settings file each one saw
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.all_seen()
            .into_iter()
            .filter(|s| s.call.role == Role::Worker)
            .collect()
    }

    /// Every call, worker, reviewer and judge alike, in order
    pub(crate) fn all_calls(&self) -> Vec<AgentCall> {
        self.all_seen().into_iter().map(|s| s.call).collect()
    }

    /// Every call, with the settings file each one saw
    pub(crate) fn all_seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// Queues answers for its next calls, oldest first
    pub(crate) fn script(&self, steps: impl IntoIterator<Item = Scripted>) {
        self.script.lock().unwrap().extend(steps);
    }
}

impl Agents for FakeClaude {
    // The file Claude Code would be started with, so a test reads what a
    // call really left on disk.
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        crate::adapters::write_claude_settings(call)
    }

    fn run(&self, call: &AgentCall) -> Result<AgentReply, AgentError> {
        let settings: serde_json::Value = std::fs::read_to_string(&call.settings)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let build = settings["env"]["CARGO_TARGET_DIR"].as_str().map(Path::new);
        let build_existed = build.is_some_and(Path::is_dir);
        let sandbox = call
            .reach
            .fence
            .as_deref()
            .map_or(serde_json::Value::Null, |fence| {
                crate::adapters::srt_settings(&crate::adapters::fence_policy(fence))
            });
        self.seen.lock().unwrap().push(Seen {
            call: call.clone(),
            settings,
            sandbox,
            build_existed,
        });
        let next = self.script.lock().unwrap().pop_front();
        let next = match next {
            Some(Scripted::Spend(account, usage, cost)) => {
                if let Some(meter) = &self.meter {
                    meter.set(account);
                }
                Some(Scripted::Reply(usage, cost))
            }
            other => other,
        };
        match next {
            Some(Scripted::Reply(usage, session_cost)) => Ok(AgentReply {
                session_id: call.session.id().clone(),
                text: "done".into(),
                usage,
                session_cost: Some(session_cost),
            }),
            Some(Scripted::Tokens(usage)) => Ok(AgentReply {
                session_id: call.session.id().clone(),
                text: "done".into(),
                usage,
                session_cost: None,
            }),
            Some(Scripted::Spend(..)) => unreachable!("turned into a reply above"),
            Some(Scripted::Fail(error)) => Err(error),
            Some(Scripted::Kill) => {
                std::fs::write(call.cwd.join(LEFT_BEHIND), "work in progress\n").unwrap();
                panic!("the runner is killed mid-turn");
            }
            Some(Scripted::Text(text)) => Ok(AgentReply {
                session_id: call.session.id().clone(),
                text: text.to_owned(),
                usage: Usage::default(),
                session_cost: Some(Cost(0)),
            }),
            Some(Scripted::Billed(text, cost)) => Ok(AgentReply {
                session_id: call.session.id().clone(),
                text: text.to_owned(),
                usage: Usage::default(),
                session_cost: Some(cost),
            }),
            Some(Scripted::Say(text)) => Ok(AgentReply {
                session_id: call.session.id().clone(),
                text: text.into(),
                usage: Usage::default(),
                session_cost: Some(Cost(0)),
            }),
            Some(Scripted::Hold(hold)) => {
                hold.block();
                Ok(AgentReply {
                    session_id: call.session.id().clone(),
                    text: "done".into(),
                    usage: Usage::default(),
                    session_cost: Some(Cost(0)),
                })
            }
            Some(Scripted::Plant(file, text)) => {
                write_in(&call.cwd, file, text);
                Ok(AgentReply {
                    session_id: call.session.id().clone(),
                    text: "done".into(),
                    usage: Usage::default(),
                    session_cost: Some(Cost(0)),
                })
            }
            Some(Scripted::Push(file, text)) => {
                write_in(&call.cwd, file, text);
                git(&call.cwd, &["add", file]);
                git(&call.cwd, &["commit", "--quiet", "-m", file]);
                git(&call.cwd, &["push", "--quiet", "origin", "HEAD"]);
                Ok(AgentReply {
                    session_id: call.session.id().clone(),
                    text: "pushed".into(),
                    usage: Usage::default(),
                    session_cost: Some(Cost(0)),
                })
            }
            Some(Scripted::MergeMain) => {
                git(&call.cwd, &["fetch", "--quiet", "origin", "main"]);
                git(
                    &call.cwd,
                    &[
                        "merge",
                        "--quiet",
                        "-X",
                        "theirs",
                        "--no-edit",
                        "origin/main",
                    ],
                );
                git(&call.cwd, &["push", "--quiet", "origin", "HEAD"]);
                Ok(AgentReply {
                    session_id: call.session.id().clone(),
                    text: "merged".into(),
                    usage: Usage::default(),
                    session_cost: Some(Cost(0)),
                })
            }
            None => Err(AgentError::Failed(
                crate::settings::Harness::ClaudeCode,
                "the rig scripts no reply".into(),
            )),
        }
    }
}
