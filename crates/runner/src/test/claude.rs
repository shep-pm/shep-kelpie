//! The stand-in Claude: what it answers, and the calls it saw

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use super::{FakeMeter, Hold, git, write_in};
use crate::ports::{
    Adopted, AgentCall, AgentError, AgentReply, Agents, CallActivity, Cost, Ending, Flying,
    NotAdopted, Role, SessionId, Usage, Utilization, Wait,
};

/// The file a killed worker leaves in its worktree, to find after a restart
pub const LEFT_BEHIND: &str = "left-behind.txt";

/// What the stand-in Claude does with its next call
#[derive(Debug, Clone)]
pub enum Scripted {
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
    /// Merges `origin`'s branch of this name into the worktree's branch, then
    /// commits this file with this text, pushes it the way a worker does, and
    /// answers
    PushMerging(&'static str, &'static str, &'static str),
    /// Pushes like [`Self::Push`], and answers with its session's context
    /// this many tokens
    PushAt(&'static str, &'static str, u64),
    /// Commits this file with this text on the worktree's branch, pushes
    /// nothing, and answers
    Commit(&'static str, &'static str),
    /// Commits these files with these texts in one commit on the worktree's
    /// branch, pushes nothing, and answers
    CommitFiles(&'static [(&'static str, &'static str)]),
    /// Commits this file with this text on the worktree's branch, pushes
    /// it the way a worker does, and answers with this final message
    PushSay(&'static str, &'static str, &'static str),
    /// Pushes like [`Self::PushSay`] from a session Claude Code compacted
    /// partway, which answers with the final message only if the settings
    /// file's `SessionStart` hook for a compaction handed it the reply's
    /// `thread <n>: fixed` line format back, and with "done" otherwise
    PushSayCompacted(&'static str, &'static str, &'static str),
    /// Writes this file with this text in the worktree, commits nothing,
    /// and answers: a write that got past the fence
    Plant(&'static str, &'static str),
    /// Writes this file with this text in the worktree, sets its mtime to this
    /// Unix time, commits nothing, and runs past its ceiling
    TimeOutPlanting(&'static str, &'static str, u64),
    /// Commits this file with this text on the worktree's branch, dated this
    /// Unix time, pushes nothing, and runs past its ceiling
    TimeOutCommitting(&'static str, &'static str, u64),
    /// Stages this file with this text, commits nothing, and deletes the
    /// working copy, so only the index holds it, and answers
    Stage(&'static str, &'static str),
    /// Aborts the merge kelpie started, then commits this file with this
    /// text and pushes it, as a worker that gives the merge up does
    AbortAndPush(&'static str, &'static str),
    /// Finishes the merge of `origin/main` that kelpie started in the
    /// worktree: keeps main's side of each conflicted file, stages it,
    /// commits with `--no-edit`, and pushes without force, as a worker
    /// resolving a conflict does
    FinishMerge,
    /// Answers with this exact text and no cost: a review round, whose
    /// reply is read rather than acted on
    Text(&'static str),
    /// Writes this file with this text below the one folder the call's fence
    /// lets it write, as a reviewer leaves a test, and answers with this text
    Attach(&'static str, &'static str, &'static str),
    /// Links this file below that folder to a file outside it, and answers
    /// with this text
    #[cfg(unix)]
    AttachLinked(&'static str, &'static str),
    /// Answers like [`Self::Text`], with this cost for the session
    Billed(&'static str, Cost),
    /// Answers with this final message
    Say(&'static str),
    /// Answers with this final message, its session's context this many tokens
    SayAt(&'static str, u64),
    /// Blocks until the test releases it, then answers. Ended first, by
    /// the call's ending or by [`FakeClaude::stop`], it fails as the real
    /// adapters do.
    Hold(Hold),
    /// Blocks until the test releases it, then fails with this error
    HoldThenFail(Hold, AgentError),
    /// Blocks like [`Self::Hold`], then answers with this final message
    HoldThenSay(Hold, &'static str),
    /// Runs a tool call until the test releases the hold, then, as the
    /// worker's `PostToolUse` hook does, ends the turn at its result while a
    /// pause is set, and goes on as the second does otherwise
    AtTool(Hold, Box<Scripted>),
    /// Waits for a lease on its model until the test releases the hold, as a
    /// call queued at a gateway does, then goes on as the second does
    Queued(Hold, Box<Scripted>),
    /// Says its gateway answered busy, as a take turned away for a reason other
    /// than another lease's turn does, until the test releases the hold, then
    /// goes on as the second does
    Busy(Hold, Box<Scripted>),
    /// Appends these lines to its session's transcript, in the folder its
    /// `transcript` names, as Claude Code writes them, then goes on as the
    /// second does
    Transcribed(String, Box<Scripted>),
}

/// A held call's issue, and what the runner noted of it
type Holding = (Option<crate::ticket::TicketId>, Option<Flying>);

/// What is queued for one role's calls
type RoleScript = (Role, VecDeque<Scripted>);

/// A call as the stand-in Claude saw it
#[derive(Debug, Clone)]
pub struct Seen {
    /// The call
    pub call: AgentCall,
    /// The settings file it named, as it stood during the call
    pub settings: serde_json::Value,
    /// The sandbox runtime's settings for its fence, as the real adapter writes them
    pub sandbox: serde_json::Value,
    /// Whether its build folder existed when the call started
    pub build_existed: bool,
    /// What the runner had noted of the call, for its record, when it started
    pub carried: Option<crate::ports::Flying>,
    /// The `change.diff` in its folder when the call started, if it had one
    pub diff: Option<String>,
}

/// Records every call and answers from a script, failing once it runs out
#[derive(Debug, Clone, Default)]
pub struct FakeClaude {
    seen: Arc<Mutex<Vec<Seen>>>,
    script: Arc<Mutex<VecDeque<Scripted>>>,
    // Answers kept for one role's calls, which no other role's call takes, so
    // calls that run at once answer the same however their threads are timed
    by_role: Arc<Mutex<Vec<RoleScript>>>,
    meter: Option<FakeMeter>,
    stopped: Arc<AtomicBool>,
    // What every call in flight last showed, as a transcript would say;
    // untracked until a test sets it
    active: Arc<Mutex<Option<CallActivity>>>,
    // The held calls now in flight, by issue, with what the runner noted of
    // each: the calls a handover could carry
    holding: Arc<Mutex<Vec<Holding>>>,
    // Whether a held call's record reads as one that is there and is not valid
    spoiled: Arc<AtomicBool>,
    // The sessions started for a terminal, and those whose terminal session ended
    started: Arc<Mutex<Vec<SessionId>>>,
    ended: Arc<Mutex<Vec<SessionId>>>,
}

impl FakeClaude {
    /// One whose calls spend what `meter` is told they do
    pub fn metered(meter: FakeMeter) -> Self {
        Self {
            meter: Some(meter),
            ..Self::default()
        }
    }

    /// The worker's own calls, in order: what every test before the review
    /// existed already asserted on, so a reviewer call never shows up and
    /// shifts their counts.
    pub fn calls(&self) -> Vec<AgentCall> {
        self.seen().into_iter().map(|s| s.call).collect()
    }

    /// The worker's own calls, with the settings file each one saw
    pub fn seen(&self) -> Vec<Seen> {
        self.all_seen()
            .into_iter()
            .filter(|s| s.call.role == Role::Worker)
            .collect()
    }

    /// Every call, worker and reviewer alike, in order
    pub fn all_calls(&self) -> Vec<AgentCall> {
        self.all_seen().into_iter().map(|s| s.call).collect()
    }

    /// Every call, with the settings file each one saw
    pub fn all_seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// The sessions [`Agents::foreground`] started, in order
    pub fn terminals_started(&self) -> Vec<SessionId> {
        self.started.lock().unwrap().clone()
    }

    /// The sessions [`Agents::foreground_ended`] was told of, in order
    pub fn terminals_ended(&self) -> Vec<SessionId> {
        self.ended.lock().unwrap().clone()
    }

    /// Says what every call in flight last showed
    pub fn set_activity(&self, activity: CallActivity) {
        *self.active.lock().unwrap() = Some(activity);
    }

    /// Queues answers for its next calls, oldest first
    pub fn script(&self, steps: impl IntoIterator<Item = Scripted>) {
        self.script.lock().unwrap().extend(steps);
    }

    /// Queues answers for the next calls of `role`, which take them before
    /// any answer queued by [`FakeClaude::script`]
    pub fn script_for(&self, role: Role, steps: impl IntoIterator<Item = Scripted>) {
        let mut kept = self.by_role.lock().unwrap();
        match kept.iter_mut().find(|(r, _)| *r == role) {
            Some((_, queue)) => queue.extend(steps),
            None => kept.push((role, steps.into_iter().collect())),
        }
    }

    /// Makes every held call's record one that is there and cannot be read
    pub fn spoil_records(&self) {
        self.spoiled.store(true, Ordering::SeqCst);
    }

    /// Ends every held call with [`AgentError::Stopped`], so a test's held
    /// calls return once it stops the runner
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    // Blocks on `hold` until it is released, or the call is ended first.
    fn hold(&self, hold: &Hold, call: &AgentCall, ending: &Ending) -> Result<(), AgentError> {
        let stopped = || self.stopped.load(Ordering::SeqCst);
        self.holding
            .lock()
            .unwrap()
            .push((call.issue, ending.carried()));
        let released = hold.block_unless(|| stopped() || ending.asked());
        self.holding
            .lock()
            .unwrap()
            .retain(|(issue, _)| *issue != call.issue);
        if released {
            return Ok(());
        }
        Err(match stopped() {
            true => AgentError::Stopped,
            false => AgentError::TimedOut(call.harness.harness()),
        })
    }
}

impl Agents for FakeClaude {
    // The file Claude Code would be started with, so a test reads what a
    // call really left on disk.
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        crate::adapters::write_claude_settings(call, &[])
    }

    // The session as the terminal would resume it, with no sandbox around it.
    fn foreground(&self, call: &AgentCall) -> Result<std::process::Command, AgentError> {
        self.started.lock().unwrap().push(call.session.id().clone());
        let mut command = std::process::Command::new("claude");
        command
            .arg("--settings")
            .arg(&call.settings)
            .args(["--resume", &call.session.id().0])
            .current_dir(&call.cwd);
        Ok(command)
    }

    fn foreground_ended(&self, session: &SessionId) {
        self.ended.lock().unwrap().push(session.clone());
    }

    // Beside the settings file, where a test can read what it would watch.
    fn transcript(&self, call: &AgentCall) -> Option<crate::ports::Transcript> {
        Some(crate::ports::Transcript {
            folder: call.settings.with_extension("transcripts"),
            threads: None,
        })
    }

    fn last_active(&self, _call: &AgentCall) -> CallActivity {
        self.active
            .lock()
            .unwrap()
            .unwrap_or(CallActivity::Untracked)
    }

    // A held call can be carried to a new image, as a real call's process can.
    // One that returned has no record left, as a call that just ended has none.
    fn adoption(&self, call: &AgentCall) -> Result<Adopted, NotAdopted> {
        let record = call.settings.with_extension("call.json");
        if self.spoiled.load(Ordering::SeqCst) {
            return Err(format!(
                "the call's record {} cannot be read: expected value at line 1 column 1",
                record.display()
            )
            .into());
        }
        let holding = self.holding.lock().unwrap();
        let held = holding.iter().find(|(issue, _)| *issue == call.issue);
        let (_, flying) = held.ok_or_else(|| {
            NotAdopted::RecordGone(format!(
                "the call's record {} cannot be read: No such file or directory (os error 2)",
                record.display()
            ))
        })?;
        Ok(Adopted {
            pid: 1,
            record,
            resumes: Some(call.session.id().clone()),
            flying: *flying,
            harness: crate::settings::Harness::ClaudeCode,
            kept: None,
            session: None,
        })
    }

    // The new image's end of a carried call: it answers at once.
    fn adopt(&self, adopted: &Adopted, _ending: &Ending) -> Result<AgentReply, AgentError> {
        Ok(AgentReply {
            session_id: adopted
                .resumes
                .clone()
                .unwrap_or(SessionId("adopted".into())),
            text: "done".into(),
            usage: Usage::default(),
            session_cost: Some(Cost(0)),
            context: None,
            compactions: 0,
        })
    }

    fn run(&self, call: &AgentCall, ending: &Ending) -> Result<AgentReply, AgentError> {
        let own = {
            let mut kept = self.by_role.lock().unwrap();
            let queue = kept.iter_mut().find(|(r, _)| *r == call.role);
            queue.and_then(|(_, queue)| queue.pop_front())
        };
        // The representative words posts at moments no test scripted its
        // neighbours for, so it takes only what is queued for its own role
        // and never an answer meant for a worker or a reviewer.
        let shared = || self.script.lock().unwrap().pop_front();
        let next = own.or_else(|| (call.role != Role::Representative).then(shared).flatten());
        let next = match next {
            Some(Scripted::Transcribed(lines, then)) => {
                let folder = call.settings.with_extension("transcripts");
                std::fs::create_dir_all(&folder).unwrap();
                let file = folder.join(format!("{}.jsonl", call.session.id().0));
                let mut transcript = (std::fs::OpenOptions::new().create(true).append(true))
                    .open(file)
                    .unwrap();
                std::io::Write::write_all(&mut transcript, lines.as_bytes()).unwrap();
                Some(*then)
            }
            other => other,
        };
        let next = match next {
            Some(Scripted::Queued(queue, then)) => {
                ending.wait(Wait::Lease);
                self.hold(&queue, call, ending)?;
                ending.begin();
                Some(*then)
            }
            Some(Scripted::Busy(busy, then)) => {
                ending.wait(Wait::Busy);
                self.hold(&busy, call, ending)?;
                ending.begin();
                Some(*then)
            }
            other => other,
        };
        let (at_tool, next) = match next {
            Some(Scripted::AtTool(tool, then)) => (Some(tool), Some(*then)),
            other => (None, other),
        };
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
            carried: ending.carried(),
            diff: std::fs::read_to_string(call.cwd.join("change.diff")).ok(),
        });
        if let Some(tool) = at_tool {
            self.hold(&tool, call, ending)?;
            // The hook is kelpie's own: the same check the real one runs.
            let build = call.reach.fence.as_deref().map(|fence| &*fence.guard.build);
            if build.and_then(crate::guard::judge_pause).is_some() {
                return Ok(answer(call, ""));
            }
        }
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
                // Written long before the rig's clock, as its commits are, so a
                // restart reads it as an old change, not as progress.
                let left = call.cwd.join(LEFT_BEHIND);
                std::fs::write(&left, "work in progress\n").unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(left)
                    .unwrap()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                    .unwrap();
                panic!("the runner is killed mid-turn");
            }
            Some(Scripted::Text(text) | Scripted::Say(text)) => Ok(said(text)),
            Some(Scripted::SayAt(text, context)) => Ok(AgentReply {
                context: Some(context),
                ..said(text)
            }),
            Some(Scripted::Billed(text, cost)) => Ok(AgentReply {
                session_cost: Some(cost),
                ..said(text)
            }),
            Some(Scripted::Hold(hold)) => {
                self.hold(&hold, call, ending)?;
                Ok(said("done"))
            }
            Some(Scripted::HoldThenSay(hold, text)) => {
                self.hold(&hold, call, ending)?;
                Ok(said(text))
            }
            Some(Scripted::Queued(..) | Scripted::Busy(..)) => {
                unreachable!("turned into its second above")
            }
            Some(Scripted::AtTool(..) | Scripted::Transcribed(..)) => {
                unreachable!("turned into its second above")
            }
            Some(Scripted::HoldThenFail(hold, error)) => {
                hold.block();
                Err(error)
            }
            Some(Scripted::Attach(file, text, reply)) => {
                write_in(&attach_folder(call), file, text);
                Ok(said(reply))
            }
            #[cfg(unix)]
            Some(Scripted::AttachLinked(file, reply)) => {
                let folder = attach_folder(call);
                let outside = folder.parent().unwrap().join("outside.rs");
                std::fs::write(&outside, "fn outside() {}\n").unwrap();
                let link = folder.join(file);
                std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&outside, link).unwrap();
                Ok(said(reply))
            }
            Some(Scripted::Plant(file, text)) => {
                write_in(&call.cwd, file, text);
                Ok(said("done"))
            }
            Some(Scripted::TimeOutPlanting(file, text, mtime)) => {
                write_in(&call.cwd, file, text);
                std::fs::File::options()
                    .write(true)
                    .open(call.cwd.join(file))
                    .unwrap()
                    .set_modified(UNIX_EPOCH + Duration::from_secs(mtime))
                    .unwrap();
                Err(AgentError::TimedOut(crate::settings::Harness::ClaudeCode))
            }
            Some(Scripted::TimeOutCommitting(file, text, committed)) => {
                write_in(&call.cwd, file, text);
                git(&call.cwd, &["add", file]);
                let status = std::process::Command::new("git")
                    .args([
                        "-c",
                        "user.name=rig",
                        "-c",
                        "user.email=rig@example.invalid",
                    ])
                    .args([
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "maintenance.auto=false",
                        "commit",
                        "--quiet",
                        "-m",
                        file,
                    ])
                    .current_dir(&call.cwd)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_COMMITTER_DATE", format!("@{committed} +0000"))
                    .status()
                    .unwrap();
                assert!(status.success());
                Err(AgentError::TimedOut(crate::settings::Harness::ClaudeCode))
            }
            Some(Scripted::Stage(file, text)) => {
                write_in(&call.cwd, file, text);
                git(&call.cwd, &["add", file]);
                std::fs::remove_file(call.cwd.join(file)).unwrap();
                Ok(said("done"))
            }
            Some(Scripted::Commit(file, text)) => {
                write_in(&call.cwd, file, text);
                git(&call.cwd, &["add", file]);
                git(&call.cwd, &["commit", "--quiet", "-m", file]);
                Ok(said("committed"))
            }
            Some(Scripted::CommitFiles(files)) => {
                for (file, text) in files {
                    write_in(&call.cwd, file, text);
                    git(&call.cwd, &["add", file]);
                }
                git(&call.cwd, &["commit", "--quiet", "-m", "files"]);
                Ok(said("committed"))
            }
            Some(Scripted::Push(file, text)) => {
                push(&call.cwd, &[(file, text)], file);
                Ok(said("pushed"))
            }
            Some(Scripted::PushMerging(branch, file, text)) => {
                // A clone has no fetch refspec, so the tip is named.
                let tip = format!("origin/{branch}");
                let spec = format!("{branch}:refs/remotes/{tip}");
                git(&call.cwd, &["fetch", "--quiet", "origin", &spec]);
                git(&call.cwd, &["merge", "--quiet", "--no-edit", &tip]);
                push(&call.cwd, &[(file, text)], file);
                Ok(said("pushed"))
            }
            Some(Scripted::PushAt(file, text, context)) => {
                push(&call.cwd, &[(file, text)], file);
                Ok(AgentReply {
                    context: Some(context),
                    ..said("pushed")
                })
            }
            Some(Scripted::PushSay(file, text, say)) => {
                push(&call.cwd, &[(file, text)], file);
                Ok(said(say))
            }
            Some(Scripted::PushSayCompacted(file, text, say)) => {
                push(&call.cwd, &[(file, text)], file);
                // Its history now a summary, it knows the reply's line format
                // only if the hook handed the turn's prompt back.
                let remembered = after_compaction(&call.cwd, &settings_of(call));
                let text = match remembered.contains("`thread <n>: fixed`") {
                    true => say,
                    false => "done",
                };
                // The hook's mark is what the adapter counts the compaction by.
                Ok(AgentReply {
                    compactions: 1,
                    ..said(text)
                })
            }
            Some(Scripted::AbortAndPush(file, text)) => {
                git(&call.cwd, &["merge", "--abort"]);
                push(&call.cwd, &[(file, text)], file);
                Ok(said("pushed"))
            }
            Some(Scripted::FinishMerge) => {
                let conflicted = git(&call.cwd, &["diff", "--name-only", "--diff-filter=U"]);
                for file in conflicted.lines() {
                    git(&call.cwd, &["checkout", "--theirs", "--", file]);
                    git(&call.cwd, &["add", file]);
                }
                git(&call.cwd, &["commit", "--quiet", "--no-edit"]);
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

// The one folder a call's fence lets it write.
fn attach_folder(call: &AgentCall) -> std::path::PathBuf {
    let fence = call.reach.fence.as_ref().expect("a fenced call");
    match fence.write.as_slice() {
        [folder] => folder.clone(),
        other => panic!("one folder to write: {other:?}"),
    }
}

// The settings file the call named, as it stands.
fn settings_of(call: &AgentCall) -> serde_json::Value {
    std::fs::read_to_string(&call.settings)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

// What Claude Code adds to a session it compacted: the output of each
// `SessionStart` hook whose matcher is `compact`, run in `cwd`.
fn after_compaction(cwd: &Path, settings: &serde_json::Value) -> String {
    let entries = settings["hooks"]["SessionStart"].as_array();
    let mut added = String::new();
    for entry in entries.into_iter().flatten() {
        if entry["matcher"] != "compact" {
            continue;
        }
        for hook in entry["hooks"].as_array().into_iter().flatten() {
            let command = hook["command"].as_str().expect("a hook has a command");
            let out = std::process::Command::new(crate::spawn::sh())
                .args(["-c", command])
                .current_dir(cwd)
                .output()
                .expect("sh runs");
            added.push_str(&String::from_utf8_lossy(&out.stdout));
        }
    }
    added
}

// What a call that cost nothing answers, saying `text`.
fn answer(call: &AgentCall, text: &str) -> AgentReply {
    AgentReply {
        session_id: call.session.id().clone(),
        text: text.to_owned(),
        usage: Usage::default(),
        session_cost: Some(Cost(0)),
        context: None,
        compactions: 0,
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
