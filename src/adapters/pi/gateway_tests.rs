// pi on a model behind a gateway, against a stand-in paddock: the key goes
// only from the forwarder, and a worker's turn holds a lease while it runs.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;

use super::tests::{FRESH, FRESH_ID, World, id};
use super::*;
use crate::ports::Role;
use crate::settings::{ContextSize, EndpointUrl, Gateway, GatewayName, KeyVar};
use crate::test::{Answer, OpenSandbox, StandInEndpoint};

const KEY: &str = "pk-not-for-workers";

fn gateways(host: &str) -> Gateways {
    let gateway = Gateway {
        url: EndpointUrl::try_from(host.to_owned()).unwrap(),
        key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
    };
    let name = GatewayName::try_from("paddock".to_owned()).unwrap();
    Gateways::reading(BTreeMap::from([(name, gateway)]), |_| Some(KEY.into()))
}

// pi as a script that prints `stdout` and exits with `code`.
fn pi_on(world: &World, server: &StandInEndpoint, stdout: &str, code: u8) -> PiCli {
    let (out, program) = (
        world.path(&format!("out-{code}")),
        world.path(&format!("pi-{code}")),
    );
    std::fs::write(&out, stdout).unwrap();
    let script = format!("#!/bin/sh\ncat {}\nexit {code}\n", out.display());
    crate::test::write_script(&program, &script);
    ClaudeCli::default()
        .sandboxed(Arc::new(OpenSandbox::default()), world.path("home"))
        .pi()
        .with_program(program)
        .with_gateways(gateways(server.host()))
}

fn on_gateway(world: &World, role: Role) -> AgentCall {
    let mut call = world.call("http://unused/v1", role, Session::New(id(FRESH_ID)));
    call.harness = AgentHarness::Pi(ModelServer {
        host: ModelHost::Gateway(GatewayName::try_from("paddock".to_owned()).unwrap()),
        context: ContextSize::try_from(65_536).unwrap(),
    });
    call
}

fn lease_calls(server: &StandInEndpoint) -> Vec<String> {
    let seen = server.seen().into_iter();
    seen.filter(|line| line.contains("/paddock/leases"))
        .collect()
}

#[test]
fn a_workers_turn_holds_a_lease_from_start_to_end_and_a_failed_one_too() {
    let server = StandInEndpoint::start([]).like_paddock(KEY, &["qwen3.8:27b"]);
    let w = World::new();
    let worker = on_gateway(&w, Role::Worker);
    let began = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let told = Arc::clone(&began);
    let ending = Ending::telling(move || told.store(true, std::sync::atomic::Ordering::SeqCst));
    let pi = pi_on(&w, &server, FRESH, 0);
    pi.prepare(&worker).unwrap();
    let reply = pi.run(&worker, &ending).unwrap();
    // The ledger takes a call with tokens and no cost as unpriced.
    assert!(
        reply.usage.input > 0 && reply.session_cost.is_none(),
        "{reply:?}"
    );
    let held = [
        "POST /paddock/leases HTTP/1.0",
        "DELETE /paddock/leases/L1 HTTP/1.0",
    ];
    assert_eq!(lease_calls(&server), held);
    assert!(
        began.load(std::sync::atomic::Ordering::SeqCst),
        "the ceiling counts from the grant"
    );

    let failing = pi_on(&w, &server, "", 1);
    let err = failing.run(&worker, &Ending::default()).unwrap_err();
    assert!(matches!(err, AgentError::Failed(..)), "{err:?}");
    assert_eq!(lease_calls(&server), [held, held].concat());

    let reviewer = on_gateway(&w, Role::Reviewer);
    pi.run(&reviewer, &Ending::default()).unwrap();
    assert_eq!(
        lease_calls(&server).len(),
        4,
        "only a worker's turn holds one"
    );
}

#[test]
fn the_key_reaches_the_gateway_only_from_the_forwarder_and_no_file_holds_it() {
    let server = StandInEndpoint::start([Answer::Says("hello")]).like_paddock(KEY, &[]);
    let w = World::new();
    let pi = pi_on(&w, &server, FRESH, 0);
    let call = on_gateway(&w, Role::Worker);
    pi.prepare(&call).unwrap();
    let (command, forwarder) = pi.sandboxed_command(&call).unwrap();

    let body = r#"{"model":"qwen3.8:27b"}"#;
    let chat = format!(
        "POST http://model.kelpie.test/v1/chat/completions HTTP/1.1\r\n\
         authorization: Bearer kelpie\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = UnixStream::connect(&forwarder.socket).unwrap();
    stream.write_all(chat.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.contains("hello"), "{reply}");
    assert_eq!(server.authorizations(), [Some(format!("Bearer {KEY}"))]);

    let shown = format!("{:?}{:?}", command.get_args().collect::<Vec<_>>(), command);
    assert!(!shown.contains(KEY), "{shown}");
    let mut files = vec![w.path("")];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if path.is_file() {
            let text = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&text);
            assert!(!text.contains(KEY), "{} holds the key", path.display());
        }
    }
    assert!(Path::new(&w.path("worker/settings.pi/models.json")).is_file());
}
