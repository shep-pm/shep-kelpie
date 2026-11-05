use super::*;

const SUBJECT: &str = "dog settings";

fn unsure<'r>(report: &'r Report, subject: &str) -> (&'r str, &'r str) {
    match verdict(report, subject) {
        Verdict::Unsure { what, next } => (what, next),
        other => panic!("`{subject}` is {other:?}"),
    }
}

fn after_dog(report: &Report) -> &str {
    let at = subjects(report).iter().position(|s| *s == "dog").unwrap();
    &report.lines[at + 1].subject
}

#[tokio::test]
async fn a_dog_waiting_for_a_restart_gets_one_line_naming_each_key() {
    let scene = Scene::new().await;
    scene.shepherd.says(
        "kelpie",
        &[r#"{"waiting_for_restart": ["webhook.ca_cert", "webhook.token"]}"#],
    );

    let report = scene.report().await;

    let (what, next) = unsure(&report, SUBJECT);
    assert_eq!(what, "waiting for restart: webhook.ca_cert, webhook.token");
    assert!(next.contains("`shep restart kelpie`"), "{next}");
    assert_eq!(after_dog(&report), SUBJECT);
}

#[tokio::test]
async fn a_dog_waiting_for_nothing_gets_no_line() {
    let scene = Scene::new().await;
    scene
        .shepherd
        .says("kelpie", &[r#"{"waiting_for_restart": []}"#]);

    let report = scene.report().await;

    assert!(
        !subjects(&report).contains(&SUBJECT),
        "{:#?}",
        report.render()
    );
}

#[tokio::test]
async fn a_dog_that_does_not_answer_or_lacks_the_field_gets_no_line() {
    for answer in [
        None,
        Some(r#"{"leases": {}}"#),
        Some("not json"),
        Some(r#"{"waiting_for_restart": "webhook.token"}"#),
    ] {
        let scene = Scene::new().await;
        scene.shepherd.replies("kelpie", &[answer]);

        let report = scene.report().await;

        let lines = subjects(&report);
        assert!(!lines.contains(&SUBJECT), "{answer:?}: {lines:?}");
    }

    let scene = Scene::new().await;
    scene.shepherd.stops("kelpie");
    let report = scene.report().await;
    assert!(!subjects(&report).contains(&SUBJECT));
}

#[tokio::test]
async fn the_line_never_fails_the_run() {
    let scene = Scene::new().await;
    scene
        .shepherd
        .says("kelpie", &[r#"{"waiting_for_restart": ["gateways"]}"#]);

    let report = scene.report().await;

    assert!(report.passed(), "{:#?}", report.render());
    assert_eq!(
        report.render().last().unwrap(),
        "nothing a project needs is missing"
    );
}
