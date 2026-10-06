//! Agent calls routed to the adapter for the harness each one names

use std::fmt;
use std::sync::Arc;

use super::{AgentCall, AgentError, AgentReply, Agents, CallActivity, Ending};
use crate::settings::Harness;

/// One adapter per harness, each taking the calls that name it
pub struct Routed {
    claude_code: Arc<dyn Agents>,
    pi: Arc<dyn Agents>,
    codex: Arc<dyn Agents>,
}

impl Routed {
    /// Claude Code's calls to `claude_code`, pi's to `pi` and Codex's to `codex`
    pub fn new(claude_code: Arc<dyn Agents>, pi: Arc<dyn Agents>, codex: Arc<dyn Agents>) -> Self {
        Self {
            claude_code,
            pi,
            codex,
        }
    }

    fn adapter(&self, call: &AgentCall) -> &dyn Agents {
        match call.harness.harness() {
            Harness::ClaudeCode => self.claude_code.as_ref(),
            Harness::Pi => self.pi.as_ref(),
            Harness::Codex => self.codex.as_ref(),
            #[cfg(test)]
            Harness::StandIn => self.claude_code.as_ref(),
        }
    }
}

impl fmt::Debug for Routed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Routed").finish_non_exhaustive()
    }
}

impl Agents for Routed {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        self.adapter(call).prepare(call)
    }

    fn run(&self, call: &AgentCall, ending: &Ending) -> Result<AgentReply, AgentError> {
        self.adapter(call).run(call, ending)
    }

    fn last_active(&self, call: &AgentCall) -> CallActivity {
        self.adapter(call).last_active(call)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ports::{Reach, Role, Session, SessionId, Tools, Usage};
    use crate::settings::{AgentHarness, ContextSize, Effort, EndpointUrl, ModelServer};

    // An adapter that answers with its own name, so a reply says who ran it.
    struct Named(&'static str);

    impl Agents for Named {
        fn prepare(&self, _: &AgentCall) -> Result<(), AgentError> {
            Err(AgentError::Setup(self.0.into()))
        }

        fn run(&self, call: &AgentCall, _: &Ending) -> Result<AgentReply, AgentError> {
            Ok(AgentReply {
                session_id: call.session.id().clone(),
                text: self.0.into(),
                usage: Usage::default(),
                session_cost: None,
            })
        }
    }

    fn call(harness: AgentHarness) -> AgentCall {
        AgentCall {
            role: Role::Worker,
            harness,
            issue: 7,
            model: "m".into(),
            effort: Effort::Low,
            session: Session::New(SessionId("s".into())),
            cwd: PathBuf::from("/k/wt/7"),
            settings: PathBuf::from("/k/worker/settings.json"),
            instructions: None,
            prompt: "go".into(),
            plugin_dirs: Vec::new(),
            tools: Tools::Work,
            lease: None,
            reach: Reach::default(),
        }
    }

    #[test]
    fn each_call_goes_to_the_adapter_for_its_harness() {
        let routed = Routed::new(
            Arc::new(Named("claude")),
            Arc::new(Named("pi")),
            Arc::new(Named("codex")),
        );
        let pi = AgentHarness::Pi(ModelServer {
            url: EndpointUrl::try_from("http://box:11434/v1".to_owned()).unwrap(),
            context: ContextSize::try_from(65_536).unwrap(),
        });
        let harnesses = [
            (AgentHarness::ClaudeCode, "claude"),
            (pi, "pi"),
            (AgentHarness::Codex, "codex"),
        ];
        for (harness, name) in harnesses {
            let call = call(harness);
            assert_eq!(routed.run(&call, &Ending::default()).unwrap().text, name);
            assert_eq!(routed.prepare(&call), Err(AgentError::Setup(name.into())));
        }
    }
}
