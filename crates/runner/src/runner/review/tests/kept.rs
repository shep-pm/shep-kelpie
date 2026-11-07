//! The local review rounds kelpie keeps, and the old ones it removes

use super::*;

// The folder the rig's kept local rounds go to, when the project names none
fn kept_folder(rig: &Rig) -> std::path::PathBuf {
    rig.paths().folder.join("local-rounds")
}

// The names of the rounds kept so far, oldest first
fn kept_rounds(rig: &Rig) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(kept_folder(rig)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn round_json(rig: &Rig, name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(kept_folder(rig).join(name).join("round.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

const ENDPOINT_REVIEWER: &str = "---\nrole: reviewer\nharness: endpoint\n\
    url: http://stand-in.invalid:8080\nmodel: iq3-xxs\ncontext: 262144\n---\n";

#[test]
fn a_local_round_is_kept_after_its_work_item_ends() {
    let (rig, runner) = at_round_1("shep");
    rig.reviewer
        .script([ScriptedRound::Findings(vec![Finding::new(
            Severity::Medium,
            "work.txt",
            1,
            "leftover text",
            "ships",
        )])]);
    let stray = rig.build_7().join("qwen-review/request-9.json");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(&stray, "{}").unwrap();
    step(&runner).unwrap(); // round 1, qwen: one finding
    std::fs::remove_dir_all(rig.build_7()).unwrap(); // the work item ends

    assert_eq!(kept_rounds(&rig), ["7-r1-20260921T141320Z"]);
    let kept = kept_folder(&rig).join("7-r1-20260921T141320Z");
    let report = std::fs::read_to_string(kept.join("round-1.txt")).unwrap();
    assert_eq!(
        report, "scripted\n",
        "the files are as the round wrote them"
    );
    let reply = std::fs::read_to_string(kept.join("round-1/reply-0.json")).unwrap();
    assert_eq!(reply, "{\"scripted\":true}");
    assert!(
        !kept.join("request-9.json").exists(),
        "a file that is no part of the round is left"
    );
}

#[test]
fn a_kept_round_records_its_pull_request_head_model_and_how_it_ended() {
    let rig = Rig::new("shep");
    rig.reviewers(&["mine", "claude"]);
    rig.write_agent("mine", ENDPOINT_REVIEWER);
    rig.reviewer.skip_check();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    let head = rig.forge.head_of("kelpie/7").unwrap();
    let base = std::process::Command::new("git")
        .args(["-C"])
        .arg(rig.worktree_7())
        .args(["rev-parse", "origin/main"])
        .output()
        .unwrap();
    let base = String::from_utf8(base.stdout).unwrap().trim().to_owned();
    let findings = vec![Finding::new(Severity::Medium, "work.txt", 1, "a", "b")];
    rig.reviewer.script([ScriptedRound::Takes {
        clock: rig.clock.clone(),
        seconds: 270,
        then: Box::new(ScriptedRound::Findings(findings)),
    }]);

    step(&runner).unwrap(); // round 1, the endpoint

    assert_eq!(
        round_json(&rig, "7-r1-20260921T141750Z"),
        json!({
            "project": "shep",
            "issue": "7",
            "pull_request": 71,
            "round": 1,
            "reviewer": "mine",
            "harness": "endpoint",
            "model": "iq3-xxs",
            "max_tokens": 65536,
            "base": base,
            "head": head,
            "started": "2026-09-21T14:13:20Z",
            "ended": "2026-09-21T14:17:50Z",
            "seconds": 270,
            "outcome": "answered",
            "reason": null,
            "findings": 1,
        })
    );
}

#[cfg(unix)]
#[test]
fn a_round_that_cannot_be_copied_leaves_no_half_kept_round() {
    use std::os::unix::fs::PermissionsExt;
    let (rig, runner) = at_round_1("shep");
    let hunks = rig.build_7().join("qwen-review/hunks");
    std::fs::create_dir_all(&hunks).unwrap();
    std::fs::set_permissions(&hunks, std::fs::Permissions::from_mode(0o000)).unwrap();

    step(&runner).unwrap(); // round 1, qwen: clean, but its files cannot be read

    std::fs::set_permissions(&hunks, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(kept_rounds(&rig), Vec::<String>::new());
}

#[test]
fn a_failed_local_round_is_kept_with_its_reason() {
    let (rig, runner) = at_round_1("shep");
    rig.reviewer
        .script([ScriptedRound::Fail(crate::ports::ReviewerError::Failed(
            "the GPU is gone".into(),
        ))]);

    step(&runner).unwrap(); // round 1, qwen: fails

    let [name] = kept_rounds(&rig).try_into().unwrap();
    let record = round_json(&rig, &name);
    assert_eq!(record["outcome"], "failed");
    assert_eq!(
        record["reason"], "the local round failed: the GPU is gone",
        "{record}"
    );
    assert_eq!(record["findings"], 0);
    assert_eq!(record["harness"], "command");
    assert_eq!(record["model"], json!(null));
    assert!(kept_folder(&rig).join(&name).join("round-1.txt").is_file());
}

// Leaves a kept round named `name` in the rig's folder, holding one file
fn leave_kept(rig: &Rig, name: &str) {
    leave_kept_of(rig, name, "shep");
}

// Leaves a kept round of project `project`
fn leave_kept_of(rig: &Rig, name: &str, project: &str) {
    let folder = kept_folder(rig).join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let record = json!({ "project": project });
    std::fs::write(folder.join("round.json"), record.to_string()).unwrap();
}

#[test]
fn rounds_older_than_keep_days_are_removed_and_zero_keeps_them_all() {
    let rig = Rig::new("shep");
    // The rig's clock reads 2026-09-21, so 30 days back is 2026-08-22.
    for name in ["7-r1-20260101T000000Z", "9-r2-20260821T235959Z"] {
        leave_kept(&rig, name);
    }
    for name in ["8-r1-20260823T000000Z", "keep-me", "notes.txt"] {
        leave_kept(&rig, name);
    }
    leave_kept_of(&rig, "5-r1-20260101T000000Z", "another");
    let runner = rig.open().unwrap();
    assert_eq!(
        kept_rounds(&rig),
        [
            "5-r1-20260101T000000Z",
            "8-r1-20260823T000000Z",
            "keep-me",
            "notes.txt"
        ],
        "the runner's start removes the project's old rounds, and nothing else"
    );

    leave_kept(&rig, "7-r3-20260101T000000Z");
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: kept, and the old round goes
    assert_eq!(
        kept_rounds(&rig),
        [
            "5-r1-20260101T000000Z",
            "7-r1-20260921T141320Z",
            "8-r1-20260823T000000Z",
            "keep-me",
            "notes.txt"
        ]
    );
    drop(runner);

    rig.edit_settings(|s| s + "\n[app.dogs.kelpie.local_rounds]\nkeep_days = 0\n");
    leave_kept(&rig, "7-r4-20260101T000000Z");
    let _runner = rig.open().unwrap();
    assert!(kept_rounds(&rig).contains(&"7-r4-20260101T000000Z".to_owned()));
}

#[cfg(unix)]
#[test]
fn a_round_that_cannot_be_removed_does_not_hold_back_the_other_old_rounds() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("shep");
    leave_kept(&rig, "6-r1-20260101T000000Z");
    let locked = kept_folder(&rig).join("6-r1-20260101T000000Z/raw");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("reply"), "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    for name in [
        "5-r1-20260101T000000Z",
        "7-r1-20260101T000000Z",
        "8-r1-20260101T000000Z",
    ] {
        leave_kept(&rig, name);
    }

    let _runner = rig.open().unwrap();

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(kept_rounds(&rig), ["6-r1-20260101T000000Z"]);
}
