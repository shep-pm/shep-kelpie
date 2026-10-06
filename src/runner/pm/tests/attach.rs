//! The maintainer's terminal holding the project manager's session

use std::process::{Child, Command, Stdio};

use serde_json::json;

use super::{answered, pm_seen, step_until, with_pm};
use crate::ports::{AgentError, Session};
use crate::runner::{StepReport, step};
use crate::settings::Harness;
use crate::test::Scripted;

// A process that lives until dropped, standing in for `shep kelpie pm` or
// the session it starts
struct Live(Child);

impl Live {
    fn new() -> Self {
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn one_terminal_holds_its_session_while_it_or_the_session_runs() {
    let (rig, runner) = with_pm("rotom");
    let (cli, other) = (Live::new(), Live::new());
    let pm = |params: String| rig.ask(&runner, "pm", Some(&params));
    let ready = pm(format!("attach {}", cli.pid()));
    assert_eq!(ready["attach"], "ready");
    assert_eq!(ready["folder"], json!(rig.paths().pm));
    assert_eq!(ready["command"]["cwd"], json!(rig.paths().pm));
    let session = ready["session"].clone();
    assert_eq!(rig.ask(&runner, "status", None)["pm"]["session"], session);
    assert_eq!(rig.ask(&runner, "status", None)["pm"]["attached"], true);

    let held = format!(
        "the project manager's session is already held, by process {}",
        cli.pid()
    );
    assert_eq!(
        pm(format!("attach {}", other.pid())),
        json!({ "error": held })
    );
    let not_held = format!(
        "the project manager's session is held by process {}, not this one",
        cli.pid()
    );
    assert_eq!(
        pm(format!("detach {}", other.pid())),
        json!({ "error": not_held })
    );

    let claude = Live::new();
    let running = pm(format!("attach {} {}", cli.pid(), claude.pid()));
    assert_eq!(running, json!({ "attach": "running" }));
    rig.ask(&runner, "tell", Some("while you are attached"));
    drop(cli);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(
        pm_seen(&rig).is_empty(),
        "its session still runs, so no wake"
    );

    drop(claude);
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    answered(step(&runner).unwrap());
    let [wake] = pm_seen(&rig).try_into().unwrap();
    assert_eq!(json!(wake.call.session.id().0), session);
    assert!(matches!(wake.call.session, Session::Resume(_)));
    assert_eq!(rig.ask(&runner, "status", None)["pm"]["attached"], false);
}

#[test]
fn a_pid_that_runs_nothing_holds_nothing() {
    let (rig, runner) = with_pm("koji");
    let mut child = Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    let gone = child.id();
    assert_eq!(
        rig.ask(&runner, "pm", Some(&format!("attach {gone}"))),
        json!({ "error": format!("no process {gone} is running") })
    );
    assert_eq!(rig.ask(&runner, "status", None)["pm"]["attached"], false);
}

#[test]
fn what_it_missed_while_held_in_a_terminal_wakes_it_once_it_is_back() {
    let (rig, runner) = with_pm("chelone");
    let cli = Live::new();
    rig.ask(&runner, "pm", Some(&format!("attach {}", cli.pid())));
    rig.ask(&runner, "add", Some("7"));
    let failed = AgentError::Failed(Harness::ClaudeCode, "it crashed".into());
    rig.claude.script([
        Scripted::Fail(failed),
        Scripted::Say("{\"unstick\": {\"item\": 7, \"action\": \"none\", \"why\": \"\"}}"),
    ]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    assert!(
        pm_seen(&rig).is_empty(),
        "no wake while the maintainer holds it"
    );

    rig.ask(&runner, "pm", Some(&format!("detach {}", cli.pid())));
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, _) = answered(Some(report));
    assert_eq!(
        woke_for,
        [
            "#7 is stuck: its worker's turn failed or stopped short twice, and ruling 1 asks \
          the maintainer"
        ]
    );
    assert_eq!(acted, ["left #7 to resolve itself"]);
}
