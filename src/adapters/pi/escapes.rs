// A pi worker's ways to the model server, each tried under the real sandbox
// runtime with the policy the adapter builds. A stand-in server on 127.0.0.1
// is the model, and `curl` stands in for pi: inside the sandbox, pi's calls
// and its commands reach the network alike.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use super::tests::{WORKER_ID, World, id};
use super::*;
use crate::adapters::SandboxRuntime;
use crate::ports::Role;
use crate::preview::Tools as KelpieTools;
use crate::test::{Answer, StandInEndpoint};

const NEEDS: &str = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`";

// `curl`'s arguments, run whole inside the sandbox with the proxy settings
// the sandbox gives it. `env` first, since the sandbox sets its own `no_proxy`.
fn curl(world: &World, call: &AgentCall, forwarder: &Forwarder, args: &[&str]) -> Output {
    let tools = KelpieTools::at(std::env::var("KELPIE_TOOLS").expect(NEEDS).into());
    let policy = policy(call, &Files::of(call), &forwarder.socket);
    let mut command = Command::new("env");
    command
        .args([
            "no_proxy=localhost",
            "NO_PROXY=localhost",
            "curl",
            "-s",
            "-m",
            "10",
        ])
        .args(["-w", " [%{http_code}]"])
        .args(args)
        .current_dir(world.path("wt"));
    let wrapped = SandboxRuntime::new(tools)
        .wrap(
            &policy,
            &world.path("worker/settings.sandbox.json"),
            &command,
        )
        .unwrap();
    let mut wrapped = wrapped;
    wrapped.output().unwrap()
}

fn said(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[ignore = "needs kelpie's tools, and runs the real sandbox"]
fn a_worker_reaches_the_model_only_through_a_forwarder_that_passes_chat() {
    let server = StandInEndpoint::start([Answer::Says("hello from the model")]);
    let world = World::new();
    let call = world.fenced(
        server.url(),
        Path::new("/k/kelpie"),
        Session::New(id(WORKER_ID)),
    );
    let AgentHarness::Pi(model) = &call.harness else {
        unreachable!()
    };
    let folder = call.settings.parent().unwrap();
    let forwarder = Forwarder::open(folder, upstream(model).unwrap()).unwrap();
    let on_worker_host = |path: &str| format!("http://{WORKER_HOST}{path}");

    let chat = curl(
        &world,
        &call,
        &forwarder,
        &[
            "-X",
            "POST",
            "-d",
            r#"{"model":"m"}"#,
            &on_worker_host("/v1/chat/completions"),
        ],
    );
    let reply = said(&chat);
    assert!(
        reply.contains("hello from the model") && reply.ends_with("[200]"),
        "{reply}"
    );
    for (method, path) in [
        ("DELETE", "/api/delete"),
        ("POST", "/api/pull"),
        ("GET", "/api/tags"),
    ] {
        let admin = curl(
            &world,
            &call,
            &forwarder,
            &["-X", method, &on_worker_host(path)],
        );
        let reply = said(&admin);
        assert!(
            reply.contains(&format!("{method} {path}")) && reply.contains("[403]"),
            "{reply}"
        );
    }

    // The model's own address, tried straight and through the proxy, and another port on its host.
    let direct = server.url().replace("/v1", "/api/delete");
    let straight = curl(&world, &call, &forwarder, &["-X", "DELETE", &direct]);
    assert!(!said(&straight).contains("[200]"), "{}", said(&straight));
    let by_proxy = curl(
        &world,
        &call,
        &forwarder,
        &["--noproxy", "", "-X", "DELETE", &direct],
    );
    assert!(!said(&by_proxy).contains("[200]"), "{}", said(&by_proxy));
    let ssh = curl(
        &world,
        &call,
        &forwarder,
        &["--noproxy", "", "http://192.0.2.9:22/"],
    );
    assert!(!said(&ssh).contains("[200]"), "{}", said(&ssh));

    assert_eq!(server.seen(), ["POST /v1/chat/completions HTTP/1.1"]);
}

// The real pi, through the forwarder, answering from the stand-in: its models
// file names a host nothing resolves, and its calls still arrive.
#[test]
#[ignore = "needs kelpie's tools and pi, and runs the real sandbox"]
fn pi_answers_through_the_forwarder_with_no_model_host_in_its_sandbox() {
    let server = StandInEndpoint::start([Answer::Says("ok")]);
    let world = World::new();
    let tools = KelpieTools::at(std::env::var("KELPIE_TOOLS").expect(NEEDS).into());
    let pi = ClaudeCli::default()
        .sandboxed(
            Arc::new(SandboxRuntime::new(tools)),
            std::env::var_os("HOME").unwrap().into(),
        )
        .pi();
    let session = id(WORKER_ID);
    let mut call = world.call(server.url(), Role::Judge, Session::New(session.clone()));
    call.prompt = "Reply with the single word ok.".into();
    pi.prepare(&call).unwrap();
    let reply = pi.run(&call).unwrap();
    assert_eq!(reply.text, "ok");
    assert_eq!(reply.session_id, session);
    assert_eq!(server.seen(), ["POST /v1/chat/completions HTTP/1.1"]);
    let settings = std::fs::read_to_string(world.path("worker/settings.sandbox.json")).unwrap();
    assert!(!settings.contains("127.0.0.1"), "{settings}");
    assert!(settings.contains("model.kelpie.test"), "{settings}");
}
