use serde_json::json;

use crate::test::{Rig, StandInEndpoint};

const PAGE: &str = include_str!("../../fixtures/gpu-exporter.txt");

// The rig's settings, with kelpie's naming `url` as the GPU's page.
fn watching(rig: &Rig, url: &str) {
    rig.set_kelpie_settings(&format!(
        "gpu_metrics_url = \"{url}\"\n[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
        Rig::WEBHOOK_URL
    ));
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
    assert_eq!(
        rig.ask(&runner, "status", None)["gpu"],
        json!({ "gpus": [{
            "id": "GPU-3f2a6c1e-0b7d-4c55-9d1a-52f1d0c4a7e8",
            "utilization_percent": 97.0,
            "memory_used_bytes": 20_132_659_200_u64,
            "memory_total_bytes": 25_757_220_864_u64,
            "power_watts": 312.5,
            "temperature_celsius": 71.0,
        }] })
    );
}

#[test]
fn an_unreadable_page_leaves_status_working_and_says_why() {
    let rig = Rig::new("shep");
    let server = StandInEndpoint::start([]).with_metrics("<html>not metrics</html>");
    watching(&rig, &server.metrics_url());
    let runner = rig.open().unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["run"], "paused");
    assert_eq!(
        status["gpu"],
        json!({ "error": "cannot read the GPU metrics: the page holds no GPU metrics" })
    );
}

#[test]
fn a_box_that_does_not_answer_is_told_without_its_address() {
    let rig = Rig::new("shep");
    watching(&rig, "http://127.0.0.1:1/metrics");
    let runner = rig.open().unwrap();
    let status = rig.ask(&runner, "status", None);
    let error = status["gpu"]["error"].as_str().unwrap();
    assert!(
        error.starts_with("cannot read the GPU metrics: nothing answered"),
        "{error}"
    );
    assert!(!error.contains("127.0.0.1"), "{error}");
    assert!(status["gpu"].get("gpus").is_none());
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
    assert_eq!(
        rig.ask(&runner, "status", None)["gpu"]["gpus"][0]["power_watts"],
        312.5
    );
}
