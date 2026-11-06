use super::*;

// Appends `lines` to golbat's `steps.jsonl`, where its runner logs its steps
fn logs(scene: &Scene, lines: &[Value]) {
    let folder = scene.kelpie_home.join("golbat");
    std::fs::create_dir_all(&folder).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let path = folder.join("steps.jsonl");
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(path, before + &text).unwrap();
}

fn installed(issue: &str, failed: Value) -> Value {
    json!({
        "at": "2026-11-05T10:00:00.000Z",
        "step": "packages-installed",
        "issue": issue,
        "folders": ["."],
        "failed": failed,
    })
}

fn unsure<'r>(report: &'r Report, subject: &str) -> (&'r str, &'r str) {
    match verdict(report, subject) {
        Verdict::Unsure { what, next } => (what, next),
        other => panic!("`{subject}` is {other:?}"),
    }
}

#[tokio::test]
async fn a_latest_failed_install_is_named() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    logs(
        &scene,
        &[installed(
            "7",
            json!("resolving\nerror: lockfile had changes, but lockfile is frozen"),
        )],
    );

    let report = scene.report().await;

    let (what, _) = unsure(&report, "golbat: package install #7");
    assert!(what.contains("failed"), "{what}");
    assert!(
        what.contains("error: lockfile had changes, but lockfile is frozen"),
        "{what}"
    );
    assert!(report.passed(), "{:#?}", report.render());
}

#[tokio::test]
async fn a_later_good_install_of_the_issue_names_nothing() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    logs(
        &scene,
        &[
            installed("7", json!("error: no network")),
            json!({"at": "2026-11-05T10:01:00.000Z", "step": "session-started", "issue": "7"}),
            installed("7", Value::Null),
        ],
    );

    let report = scene.report().await;

    assert!(
        !subjects(&report)
            .iter()
            .any(|subject| subject.starts_with("golbat: package install")),
        "{:#?}",
        report.render()
    );
}

// Steps of a long run, each wider than a few hundred bytes
fn filler(count: usize) -> Vec<Value> {
    (0..count)
        .map(|n| json!({"at": "2026-11-05T10:02:00.000Z", "step": "ended", "n": n, "pad": "x".repeat(900)}))
        .collect()
}

#[tokio::test]
async fn an_install_among_the_last_200_steps_of_a_long_log_is_named() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let mut steps = filler(300);
    steps.push(installed("7", json!("error: no network")));
    steps.extend(filler(150));
    logs(&scene, &steps);

    let report = scene.report().await;

    unsure(&report, "golbat: package install #7");
}

#[tokio::test]
async fn an_install_before_the_last_200_steps_is_not_read() {
    let scene = Scene::new().await;
    scene.runs("golbat", |_| {});
    let mut steps = vec![installed("7", json!("error: no network"))];
    steps.extend(filler(200));
    logs(&scene, &steps);

    let report = scene.report().await;

    assert!(
        !subjects(&report)
            .iter()
            .any(|subject| subject.starts_with("golbat: package install")),
        "{:#?}",
        report.render()
    );
}
