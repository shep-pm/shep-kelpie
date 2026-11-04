// goose on a model behind a gateway, against a stand-in paddock: the key goes
// only from the forwarder, and each call holds a lease while it runs.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::Arc;

use super::tests::{FRESH, FRESH_ID, World, id, script};
use super::*;
use crate::ipc::blocking::Stream;
use crate::settings::{ContextSize, EndpointUrl, Gateway, GatewayKind, GatewayName, KeyVar};
use crate::test::{Answer, OpenSandbox, StandInEndpoint};

pub(super) const KEY: &str = "pk-not-for-workers";

pub(super) fn gateways(host: &str, kind: GatewayKind) -> Gateways {
    let gateway = Gateway {
        kind,
        url: EndpointUrl::try_from(host.to_owned()).unwrap(),
        key_env: KeyVar::try_from("PADDOCK_KEY".to_owned()).unwrap(),
    };
    let name = GatewayName::try_from("paddock".to_owned()).unwrap();
    Gateways::reading(BTreeMap::from([(name, gateway)]), |_| Some(KEY.into()))
}

// goose as a script that prints `stdout` and exits with `code`.
pub(super) fn goose_on(
    world: &World,
    server: &StandInEndpoint,
    stdout: &str,
    code: u8,
) -> GooseCli {
    goose_of_kind(world, server, GatewayKind::Paddock, stdout, code)
}

pub(super) fn goose_of_kind(
    world: &World,
    server: &StandInEndpoint,
    kind: GatewayKind,
    stdout: &str,
    code: u8,
) -> GooseCli {
    GooseCli::on(&Base::default().sandboxed(Arc::new(OpenSandbox::default()), world.path("home")))
        .with_program(script(world, stdout, "", code))
        .with_gateways(gateways(server.host(), kind))
}

pub(super) fn on_gateway(world: &World, role: Role) -> AgentCall {
    let mut call = world.call("http://unused/v1", role, Session::New(id(FRESH_ID)));
    call.harness = AgentHarness::Goose(ModelServer {
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
fn a_reviewers_turn_holds_a_lease_from_start_to_end_and_a_failed_one_too() {
    let server = StandInEndpoint::start([]).like_paddock(KEY, &["qwen3.8:27b"]);
    let w = World::new();
    let reviewer = on_gateway(&w, Role::Reviewer);
    let began = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let told = Arc::clone(&began);
    let waits = Arc::new(std::sync::Mutex::new(Vec::new()));
    let waited = Arc::clone(&waits);
    let ending = Ending::telling(
        move || told.store(true, std::sync::atomic::Ordering::SeqCst),
        move |wait| waited.lock().unwrap().push(wait),
    );
    let goose = goose_on(&w, &server, FRESH, 0);
    goose.prepare(&reviewer).unwrap();
    let reply = goose.run(&reviewer, &ending).unwrap();
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
    assert_eq!(*waits.lock().unwrap(), [crate::ports::Wait::Lease]);

    let failing = goose_on(&w, &server, "", 1);
    let err = failing.run(&reviewer, &Ending::default()).unwrap_err();
    assert!(matches!(err, AgentError::Failed(..)), "{err:?}");
    assert_eq!(lease_calls(&server), [held, held].concat());
}

#[test]
fn a_reviewer_turn_on_an_openai_gateway_starts_at_once_and_asks_no_paddock_route() {
    let server = StandInEndpoint::start([]).like_openai(KEY, &["qwen3.8:27b"]);
    let w = World::new();
    let reviewer = on_gateway(&w, Role::Reviewer);
    let told = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (began, waited) = (Arc::clone(&told), Arc::clone(&told));
    let ending = Ending::telling(
        move || began.lock().unwrap().push("began".to_owned()),
        move |wait| waited.lock().unwrap().push(format!("{wait:?}")),
    );
    let goose = goose_of_kind(&w, &server, GatewayKind::Openai, FRESH, 0);
    goose.prepare(&reviewer).unwrap();
    let reply = goose.run(&reviewer, &ending).unwrap();
    assert!(reply.usage.input > 0, "{reply:?}");
    assert_eq!(
        *told.lock().unwrap(),
        Vec::<String>::new(),
        "no lease to wait for or to begin from"
    );
    assert_eq!(
        server.seen(),
        Vec::<String>::new(),
        "only the forwarder talks to it"
    );
}

// Sends one chat call through `forwarder` and returns the reply.
fn chat_through(forwarder: &Forwarder, authorization: Option<&str>) -> String {
    let body = r#"{"model":"qwen3.8:27b"}"#;
    let authorization = authorization
        .map(|value| format!("authorization: {value}\r\n"))
        .unwrap_or_default();
    let chat = format!(
        "POST http://model.kelpie.test/v1/chat/completions HTTP/1.1\r\n\
         {authorization}content-length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = Stream::connect(&forwarder.socket).unwrap();
    stream.write_all(chat.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn the_key_reaches_the_gateway_only_from_the_forwarder_and_no_file_holds_it() {
    let server = StandInEndpoint::start([Answer::Says("hello")]).like_paddock(KEY, &[]);
    let w = World::new();
    let goose = goose_on(&w, &server, FRESH, 0);
    let call = on_gateway(&w, Role::Reviewer);
    goose.prepare(&call).unwrap();
    let (command, forwarder) = goose.sandboxed_command(&call, None).unwrap();

    let reply = chat_through(&forwarder, Some("Bearer kelpie"));
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
}

#[test]
fn a_reviewers_chat_call_on_a_gateway_asks_for_batch() {
    let server = StandInEndpoint::start([Answer::Says("hello"), Answer::Says("hello")])
        .like_paddock(KEY, &[]);
    let w = World::new();
    let goose = goose_on(&w, &server, FRESH, 0);
    for role in [Role::Reviewer, Role::Worker] {
        let call = on_gateway(&w, role);
        let (_command, forwarder) = goose.sandboxed_command(&call, None).unwrap();
        let reply = chat_through(&forwarder, None);
        assert!(reply.contains("hello"), "{role:?}: {reply}");
    }
    let batch = vec![("x-paddock-priority".to_owned(), "batch".to_owned())];
    assert_eq!(server.paddock_headers(), [batch.clone(), batch]);
}

#[test]
fn a_workers_chat_call_names_the_lease_its_turn_holds() {
    let server = StandInEndpoint::start([Answer::Says("hello"), Answer::Says("hello")])
        .like_paddock(KEY, &[]);
    let w = World::new();
    let goose = goose_on(&w, &server, FRESH, 0);
    for role in [Role::Worker, Role::Reviewer] {
        let call = on_gateway(&w, role);
        // The lease is let go of when `leased` returns, and the forwarder keeps its name.
        let (_command, forwarder) = goose
            .leased(&call, &Ending::default(), |lease| {
                goose.sandboxed_command(&call, lease)
            })
            .unwrap()
            .0
            .unwrap();
        assert!(chat_through(&forwarder, None).contains("hello"), "{role:?}");
    }
    let batch = ("x-paddock-priority".to_owned(), "batch".to_owned());
    let lease = ("x-paddock-lease".to_owned(), "L1".to_owned());
    // The lease's own take and release are requests too, with no paddock header.
    let chats: Vec<_> = (server.paddock_headers().into_iter())
        .filter(|headers| !headers.is_empty())
        .collect();
    assert_eq!(chats, [vec![batch.clone(), lease], vec![batch]]);
}
