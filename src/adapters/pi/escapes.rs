// A pi worker's ways to the model server, each tried under the real sandbox
// runtime with the policy the adapter builds. A stand-in server on 127.0.0.1
// is the model, and `curl` stands in for pi: inside the sandbox, pi's calls
// and its commands reach the network alike.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    curl_allowing(world, call, forwarder, &[], args)
}

// The same, with `allowed` listed as hosts the project allows the worker.
fn curl_allowing(
    world: &World,
    call: &AgentCall,
    forwarder: &Forwarder,
    allowed: &[String],
    args: &[&str],
) -> Output {
    let tools = KelpieTools::at(std::env::var("KELPIE_TOOLS").expect(NEEDS).into());
    let mut policy = policy(call, &Files::of(call), &forwarder.socket);
    policy.hosts.extend(allowed.iter().cloned());
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

    // The model's own address and another port on its host, each tried straight
    // and through the proxy. The second port counts every connection it gets.
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let other_url = format!("http://{}/", other.local_addr().unwrap());
    let reached = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&reached);
    std::thread::spawn(move || {
        for _ in other.incoming().flatten() {
            counting.fetch_add(1, Ordering::SeqCst);
        }
    });
    let model = server.url().replace("/v1", "/api/delete");
    for url in [&model, &other_url] {
        let direct = ["--noproxy", "*", "-X", "DELETE", url];
        let straight = curl(&world, &call, &forwarder, &direct);
        assert!(said(&straight).ends_with("[000]"), "{}", said(&straight));
        let proxied = ["--noproxy", "", "-X", "DELETE", url];
        let reply = said(&curl(&world, &call, &forwarder, &proxied));
        assert!(
            reply.contains("blocked by network allowlist") && reply.ends_with("[403]"),
            "{reply}"
        );
    }
    // The project allowing the model's host, by name or with a port, does not open it.
    let listed = [
        "127.0.0.1".to_owned(),
        other_url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned(),
        "localhost".to_owned(),
    ];
    for url in [&model, &other_url] {
        let proxied = ["--noproxy", "", "-X", "DELETE", url];
        let reply = said(&curl_allowing(&world, &call, &forwarder, &listed, &proxied));
        assert!(!reply.contains("[200]"), "{reply}");
    }
    assert_eq!(reached.load(Ordering::SeqCst), 0);
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
    let mut call = world.call(server.url(), Role::Reviewer, Session::New(session.clone()));
    call.prompt = "Reply with the single word ok.".into();
    pi.prepare(&call).unwrap();
    let reply = pi.run(&call).unwrap();
    assert_eq!(reply.text, "ok");
    assert_eq!(reply.session_id, session);
    assert_eq!(server.seen(), ["POST /v1/chat/completions HTTP/1.1"]);
    let text = std::fs::read_to_string(world.path("worker/settings.sandbox.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    let network = &settings["network"];
    assert_eq!(
        network["allowedDomains"],
        serde_json::json!(["model.kelpie.test"])
    );
    assert!(
        network["deniedDomains"].to_string().contains("127.0.0.1"),
        "{text}"
    );
}
