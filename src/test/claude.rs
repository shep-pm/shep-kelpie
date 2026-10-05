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
    /// Commits and pushes the first file as `Push` does, then changes the
    /// second, tracked file and leaves it uncommitted
    PushLeaving(&'static str, &'static str, &'static str),
    /// Commits and pushes each of these files with this text in one commit
    PushMany(&'static [(&'static str, &'static str)]),
    /// Commits and pushes the first file as `Push` does, after deleting the
    /// third, which was in the worktree and is not committed
    PushAndRemove(&'static str, &'static str, &'static str),
    /// Writes this file with this text in the worktree, then fails with this error
    WriteThenFail(&'static str, &'static str, AgentError),
    /// Writes each of these files with its text, commits nothing, and answers
    /// with the last text
    WriteMany(&'static [(&'static str, &'static str)], &'static str),
    /// Writes this file with this text and answers with the third, after
    /// removing the file that links the worktree's git folder to its repo, so
    /// kelpie's own git commands about the worktree fail
    WriteAndBreakGit(&'static str, &'static str, &'static str),
    /// Commits and pushes the first file as `Push` does, then leaves a new
    /// file with the third name uncommitted
    PushAndPlant(&'static str, &'static str, &'static str),
    /// Writes this file with this text in the worktree, commits nothing, and
    /// answers with this exact text: a session that confirms a finding with a
    /// failing test
    Write(&'static str, &'static str, &'static str),
    /// Stages this file with this text, commits nothing, and deletes the
    /// working copy, so only the index holds it, and answers
    Stage(&'static str, &'static str),
    /// Merges `origin/main` into the worktree's branch, keeping main's
    /// side of any conflict, and pushes it without force, as a worker
    /// resolving a conflict does
    MergeMain,
    /// Answers with this exact text and no cost: a review round, whose
    /// reply is read rather than acted on
    Text(&'static str),
    /// Answers like [`Self::Text`], with this cost for the session
    Billed(&'static str, Cost),
    /// Answers with this final message
    Say(&'static str),
    /// Blocks until the test releases it, then answers
    Hold(Hold),
    /// Blocks until the test releases it, then fails with this error
    HoldThenFail(Hold, AgentError),
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

    pub(crate) fn block(&self) {
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
    /// existed already asserted on, so a reviewer call never shows up and
    /// shifts their counts.
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

    /// Every call, worker and reviewer alike, in order
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
        let said = |text: &str| answer(call, text);
        match next {
            Some(Scripted::Reply(usage, session_cost)) => Ok(AgentReply {
                usage,
                session_cost: Some(session_cost),
                ..said("done")
            }),
            Some(Scripted::Tokens(usage)) => Ok(AgentReply {
                usage,
                session_cost: None,
                ..said("done")
            }),
            Some(Scripted::Spend(..)) => unreachable!("turned into a reply above"),
            Some(Scripted::Fail(error)) => Err(error),
            Some(Scripted::Kill) => {
                std::fs::write(call.cwd.join(LEFT_BEHIND), "work in progress\n").unwrap();
                panic!("the runner is killed mid-turn");
            }
            Some(Scripted::Text(text) | Scripted::Say(text)) => Ok(said(text)),
            Some(Scripted::Billed(text, cost)) => Ok(AgentReply {
                session_cost: Some(cost),
                ..said(text)
            }),
            Some(Scripted::Hold(hold)) => {
                hold.block();
                Ok(said("done"))
            }
            Some(Scripted::HoldThenFail(hold, error)) => {
                hold.block();
                Err(error)
            }
            Some(Scripted::Plant(file, text)) => {
                write_in(&call.cwd, file, text);
                Ok(said("done"))
            }
            Some(Scripted::Write(file, text, say)) => {
                write_in(&call.cwd, file, text);
                Ok(said(say))
            }
            Some(Scripted::Stage(file, text)) => {
                write_in(&call.cwd, file, text);
                git(&call.cwd, &["add", file]);
                std::fs::remove_file(call.cwd.join(file)).unwrap();
                Ok(said("done"))
            }
            Some(Scripted::PushAndRemove(file, text, removed)) => {
                std::fs::remove_file(call.cwd.join(removed)).unwrap();
                push(&call.cwd, &[(file, text)], file);
                Ok(said("pushed"))
            }
            Some(Scripted::WriteAndBreakGit(file, text, say)) => {
                write_in(&call.cwd, file, text);
                let own = git(&call.cwd, &["rev-parse", "--absolute-git-dir"]);
                std::fs::remove_file(std::path::Path::new(&own).join("commondir")).unwrap();
                Ok(said(say))
            }
            Some(Scripted::WriteThenFail(file, text, error)) => {
                write_in(&call.cwd, file, text);
                Err(error)
            }
            Some(Scripted::WriteMany(files, say)) => {
                for (file, text) in files {
                    write_in(&call.cwd, file, text);
                }
                Ok(said(say))
            }
            Some(Scripted::PushMany(files)) => {
                push(&call.cwd, files, "fix");
                Ok(said("pushed"))
            }
            Some(Scripted::PushAndPlant(file, text, junk)) => {
                push(&call.cwd, &[(file, text)], file);
                write_in(&call.cwd, junk, "left behind\n");
                Ok(said("pushed"))
            }
            Some(Scripted::PushLeaving(file, text, tracked)) => {
                push(&call.cwd, &[(file, text)], file);
                write_in(&call.cwd, tracked, "an edit the worker left uncommitted\n");
                Ok(said("pushed"))
            }
            Some(Scripted::Push(file, text)) => {
                push(&call.cwd, &[(file, text)], file);
                Ok(said("pushed"))
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
                Ok(said("merged"))
            }
            None => Err(AgentError::Failed(
                crate::settings::Harness::ClaudeCode,
                "the rig scripts no reply".into(),
            )),
        }
    }
}

// What a call that cost nothing answers, saying `text`.
fn answer(call: &AgentCall, text: &str) -> AgentReply {
    AgentReply {
        session_id: call.session.id().clone(),
        text: text.to_owned(),
        usage: Usage::default(),
        session_cost: Some(Cost(0)),
    }
}

// Writes each file, commits them in one commit with `message`, and pushes it
// the way a worker does.
fn push(cwd: &Path, files: &[(&str, &str)], message: &str) {
    for (file, text) in files {
        write_in(cwd, file, text);
        git(cwd, &["add", file]);
    }
    git(cwd, &["commit", "--quiet", "-m", message]);
    git(cwd, &["push", "--quiet", "origin", "HEAD"]);
}
