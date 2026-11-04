// The recording of the real pi, which the fixtures in `fixtures/` came from.

use std::path::PathBuf;
use std::sync::Arc;

use super::tests::{FRESH_ID, WORKER_ID, World, id};
use super::*;
use crate::adapters::SandboxRuntime;

const NEEDS: &str = "needs KELPIE_PI_URL=<server>/v1 running qwen3.8:27b, KELPIE_BIN=<kelpie>, \
                     and KELPIE_PI_RECORD=<dir> for what it prints. Run it under \
                     `kelpie lease run gpu --model <model>`";

fn env(name: &str) -> String {
    std::env::var(name).expect(NEEDS)
}

// The real pi on the real model under the real sandbox: a turn, the session
// resumed, and a worker refused by `kelpie confine`, the sandbox and `kelpie
// guard`. The fixtures are these recordings with the temporary folder renamed.
#[test]
#[ignore = "runs pi on a local model"]
fn record_a_turn_a_resumed_turn_and_two_refusals() {
    let (url, kelpie) = (env("KELPIE_PI_URL"), PathBuf::from(env("KELPIE_BIN")));
    let record = PathBuf::from(env("KELPIE_PI_RECORD"));
    let pi = PiCli::on(&Base::default().sandboxed(
        Arc::new(SandboxRuntime::under_test()),
        std::env::var_os("HOME").unwrap().into(),
    ));
    let world = World::new();
    let fresh_id = id(FRESH_ID);
    let save = |name: &str, out: &Output| {
        let text = String::from_utf8_lossy(&out.stdout)
            .replace(world.root.to_str().unwrap(), "/tmp/kelpie-pi");
        std::fs::write(record.join(name), text).unwrap();
    };

    let mut fresh = world.call(&url, Role::Reviewer, Session::New(fresh_id.clone()));
    fresh.prompt = "Reply with the single word ok.".into();
    pi.prepare(&fresh).unwrap();
    let out = run_raw(&pi, &fresh);
    save("pi-p-fresh.jsonl", &out);
    let first = parse_result(&out, &fresh_id).unwrap();

    let mut again = world.call(&url, Role::Reviewer, Session::Resume(fresh_id.clone()));
    again.prompt = "Which word did you reply with? Answer in one word.".into();
    let out = run_raw(&pi, &again);
    save("pi-p-resumed.jsonl", &out);
    let second = parse_result(&out, &fresh_id).unwrap();
    assert!(second.text.to_lowercase().contains("ok"), "{second:?}");
    assert!(second.usage.input + second.usage.cache_read > first.usage.input);

    let worker_id = id(WORKER_ID);
    let mut worker = world.fenced(&url, &kelpie, Session::New(worker_id.clone()));
    worker.prompt = format!(
        "Do these three steps, then stop. First, use the write tool to create \
         .claude/settings.json holding {{}}. Second, use the bash tool to run \
         `echo hi > {}/x`. Third, use the bash tool to run `gh pr merge 1`. \
         Then say in one line per step what happened.",
        world.path("outside").display()
    );
    pi.prepare(&worker).unwrap();
    let out = run_raw(&pi, &worker);
    save("pi-p-refused.jsonl", &out);
    parse_result(&out, &worker_id).unwrap();
    assert!(!world.path("wt/.claude/settings.json").exists());
    assert!(!world.path("outside/x").exists());
}

fn run_raw(pi: &PiCli, call: &AgentCall) -> Output {
    let (mut command, _forwarder) = pi.sandboxed_command(call, None).unwrap();
    command.stdin(std::process::Stdio::null()).output().unwrap()
}
