use std::net::TcpListener;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::runner::Runner;
use crate::test::{Rig, StandInEndpoint};

const PAGE: &str = include_str!("../../fixtures/gpu-exporter.txt");

// Shep gives an action this long to answer.
const ACTION_BUDGET: Duration = Duration::from_secs(3);

// Kelpie's settings, with `gpu_metrics_url` set to `url`.
fn watching(rig: &Rig, url: &str) {
    rig.set_kelpie_settings(&format!(
        "gpu_metrics_url = \"{url}\"\n[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    ));
}

// The status once the background read has landed, which it does on its own.
fn read(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let status = rig.ask(runner, "status", None);
        if status["gpu"].get("age_seconds").is_some() {
            return status;
        }
        assert!(Instant::now() < until, "the GPU was never read: {status}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn status_has_no_gpu_without_a_metrics_url() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    assert!(rig.ask(&runner, "status", None).get("gpu").is_none());
}

#[test]
fn status_shows_the_gpus_figures_from_the_metrics_page() {
    let rig = Rig::new("shep");
    let server = StandInEndpoint::start([]).with_metrics(PAGE);
    watching(&rig, &server.metrics_url());
    let runner = rig.open().unwrap();
    let gpu = read(&rig, &runner)["gpu"].clone();
    assert_eq!(gpu["age_seconds"], 0);
    assert_eq!(
        gpu["gpus"],
        json!([{
            "id": "3f2a6c1e-0b7d-4c55-9d1a-52f1d0c4a7e8",
            "utilization_percent": 97.0,
            "memory_used_bytes": 20_132_659_200_u64,
            "memory_total_bytes": 25_757_220_864_u64,
            "power_watts": 312.5,
            "temperature_celsius": 71.0,
        }])
    );
}

#[test]
fn an_unreadable_page_leaves_status_working_and_says_why() {
    let rig = Rig::new("shep");
    let server = StandInEndpoint::start([]).with_metrics("<html>not metrics</html>");
    watching(&rig, &server.metrics_url());
    let runner = rig.open().unwrap();
    let status = read(&rig, &runner);
    assert_eq!(status["run"], "paused");
    assert_eq!(
        status["gpu"]["error"],
        "cannot read the GPU metrics: the page holds no GPU metrics"
    );
    assert!(status["gpu"].get("gpus").is_none());
}

#[test]
fn a_box_that_does_not_answer_is_told_without_its_address() {
    let rig = Rig::new("shep");
    watching(&rig, "http://127.0.0.1:1/metrics");
    let runner = rig.open().unwrap();
    let error = read(&rig, &runner)["gpu"]["error"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        error.starts_with("cannot read the GPU metrics: nothing answered"),
        "{error}"
    );
    assert!(!error.contains("127.0.0.1"), "{error}");
}

#[test]
fn a_box_that_never_replies_does_not_slow_status_or_hold_the_runner() {
    let rig = Rig::new("shep");
    // Accepts the connection and says nothing, as a hung box does.
    let hung = TcpListener::bind("127.0.0.1:0").unwrap();
    watching(
        &rig,
        &format!("http://{}/metrics", hung.local_addr().unwrap()),
    );
    let runner = rig.open().unwrap();
    for _ in 0..5 {
        let asked = Instant::now();
        let status = rig.ask(&runner, "status", None);
        assert!(asked.elapsed() < ACTION_BUDGET / 3, "status waited");
        assert_eq!(
            status["gpu"]["error"],
            "the GPU metrics have not been read yet"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let asked = Instant::now();
    assert_eq!(rig.ask(&runner, "pause", None)["run"], "paused");
    assert!(asked.elapsed() < ACTION_BUDGET / 3, "pause waited");
    drop(hung);
}

#[test]
fn a_changed_metrics_url_takes_effect_at_the_next_reread() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let server = StandInEndpoint::start([]).with_metrics(PAGE);
    watching(&rig, &server.metrics_url());
    let line = runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: gpu_metrics_url now in effect")
    );
    assert_eq!(read(&rig, &runner)["gpu"]["gpus"][0]["power_watts"], 312.5);
}
