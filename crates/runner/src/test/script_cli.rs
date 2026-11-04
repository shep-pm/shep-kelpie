//! A CLI that is one shell line, run by the generic adapter in the place of Claude Code in a rig

use std::process::{Command, Output};
use std::sync::Arc;

use super::OpenSandbox;
use crate::adapters::{Base, Cli, CliAdapter, Launch};
use crate::ports::{AgentCall, AgentError, AgentReply, Policy, Usage};
use crate::settings::Harness;

/// Answers with what its `sh -c` line prints, given the prompt as `$1`
#[derive(Debug, Clone)]
pub struct ScriptCli(pub &'static str);

impl ScriptCli {
    /// The adapter for this script, in the open sandbox, with `home` as its home
    pub fn adapter(self, home: &std::path::Path) -> CliAdapter<Self> {
        let sandbox = Arc::new(OpenSandbox::default());
        CliAdapter::new(self, &Base::default().sandboxed(sandbox, home.to_owned()))
    }
}

impl Cli for ScriptCli {
    type Begun = ();
    const HARNESS: Harness = Harness::ClaudeCode;

    fn write_config(
        &self,
        call: &AgentCall,
        _: Option<&shep_kelpie_core::forwarder::Upstream>,
    ) -> Result<(), AgentError> {
        std::fs::write(&call.settings, "{}").map_err(|e| AgentError::Setup(e.to_string()))
    }

    fn command(&self, call: &AgentCall, _: &(), _: &Launch<'_>) -> Result<Command, AgentError> {
        let mut command = Command::new(crate::spawn::sh());
        command
            .args(["-c", self.0, "sh", &call.prompt])
            .current_dir(&call.cwd);
        Ok(command)
    }

    fn policy(&self, _: &AgentCall, _: &Launch<'_>) -> Result<Policy, AgentError> {
        Ok(Policy::default())
    }

    fn parse(&self, call: &AgentCall, _: (), output: &Output) -> Result<AgentReply, AgentError> {
        Ok(AgentReply {
            session_id: call.session.id().clone(),
            text: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            usage: Usage::default(),
            session_cost: None,
            context: None,
            compactions: 0,
            metered: None,
        })
    }

    fn parse_adopted(
        &self,
        _: &crate::ports::Adopted,
        _: (),
        _: &Output,
    ) -> Result<AgentReply, AgentError> {
        Err(AgentError::Setup("a script is not handed over".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Agents, Ending, Role, Session, SessionId, Tools};
    use crate::settings::{AgentHarness, Effort};

    fn call(home: &std::path::Path, session: Session) -> AgentCall {
        let cwd = home.join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        AgentCall {
            settings: home.join("worker").join("settings.json"),
            ..AgentCall::new(
                Role::Worker,
                Tools::Work,
                AgentHarness::ClaudeCode,
                "m",
                Effort::Low,
                session,
                cwd,
                "hello",
            )
        }
    }

    #[test]
    fn a_script_answers_through_the_generic_adapter() {
        let home = tempfile::tempdir().unwrap();
        let adapter = ScriptCli("printf 'you said %s' \"$1\"").adapter(home.path());
        let call = call(home.path(), Session::New(SessionId("s".into())));
        std::fs::create_dir_all(call.settings.parent().unwrap()).unwrap();

        adapter.prepare(&call).unwrap();
        let reply = adapter.run(&call, &Ending::default()).unwrap();

        assert_eq!(
            (reply.text.as_str(), reply.session_id.0.as_str()),
            ("you said hello", "s")
        );
    }

    #[test]
    fn the_scratch_folder_is_empty_at_each_command() {
        let home = tempfile::tempdir().unwrap();
        let adapter = ScriptCli("test -e \"$1/left\" && echo stale; touch \"$1/left\"; echo ran")
            .adapter(home.path());
        let mut call = call(home.path(), Session::New(SessionId("s".into())));
        std::fs::create_dir_all(call.settings.parent().unwrap()).unwrap();
        call.prompt = call.settings.with_extension("tmp").display().to_string();

        let first = adapter.run(&call, &Ending::default()).unwrap();
        let second = adapter.run(&call, &Ending::default()).unwrap();

        assert_eq!((first.text.as_str(), second.text.as_str()), ("ran", "ran"));
    }
}
