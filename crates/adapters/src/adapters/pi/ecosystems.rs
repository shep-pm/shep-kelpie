//! The fixture projects of `claude::ecosystems`, run through pi's fence

use std::path::{Path, PathBuf};

use super::{Files, fenced};
use crate::adapters::claude::ecosystems::{self, Ecosystem, Through};
use crate::ports::{AgentCall, Policy, Reach, Role, Session, SessionId, Tools};
use crate::settings::{AgentHarness, ContextSize, Effort, EndpointUrl, ModelHost, ModelServer};

const PI: Through = Through { call, policy };

// The model is never reached: `/bin/sh` stands in for pi, and its fence lists no model host.
fn call(reach: Reach, settings: PathBuf, cwd: PathBuf) -> AgentCall {
    let server = ModelServer {
        host: ModelHost::Url(
            EndpointUrl::try_from("http://model.kelpie.test/v1".to_owned()).unwrap(),
        ),
        context: ContextSize::try_from(65_536).unwrap(),
    };
    AgentCall {
        issue: crate::ticket::TicketId::from_issue(7),
        settings,
        reach,
        ..AgentCall::new(
            Role::Worker,
            Tools::Work,
            AgentHarness::Pi(server),
            "qwen3.8:27b",
            Effort::Low,
            Session::New(SessionId("s".into())),
            cwd,
            String::new(),
        )
    }
}

fn policy(call: &AgentCall, _: &Path) -> Policy {
    fenced(call, &Files::of(call), Path::new("/nowhere/pi"), &[])
}

#[test]
#[ignore = "runs the real sandbox with Rust, after `cargo build`"]
fn a_rust_project_builds_tests_lints_and_pushes_inside_the_fence() {
    ecosystems::project(&PI, Ecosystem::Rust);
}

#[test]
#[ignore = "runs the real sandbox with Go and golangci-lint, after `cargo build`"]
fn a_go_project_builds_tests_lints_and_pushes_inside_the_fence() {
    ecosystems::project(&PI, Ecosystem::Go);
}

#[test]
#[ignore = "runs the real sandbox with bun, after `cargo build`"]
fn a_bun_project_installs_tests_and_pushes_inside_the_fence() {
    ecosystems::project(&PI, Ecosystem::Bun);
}

#[test]
#[ignore = "runs the real sandbox with Node, after `cargo build`"]
fn an_npm_project_installs_tests_and_pushes_inside_the_fence() {
    ecosystems::project(&PI, Ecosystem::Npm);
}

#[test]
#[ignore = "runs the real sandbox with Node, after `cargo build`"]
fn a_hook_script_loads_its_neighbours_inside_the_fence() {
    ecosystems::hook_script(&PI);
}
