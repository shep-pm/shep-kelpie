use crate::test::Rig;

use super::settings_with;

// The host the file holds, which is the one the dog goes to.
fn recorded(rig: &Rig) -> Option<String> {
    let text = std::fs::read_to_string(rig.paths().e2e()).ok()?;
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    Some(file["host"].as_str().unwrap().to_owned())
}

#[test]
fn a_new_e2e_host_reaches_the_file_the_dog_reads() {
    let rig = Rig::new("shep");
    rig.e2e_on();
    let runner = rig.open().unwrap();
    assert_eq!(recorded(&rig).as_deref(), Some("e2e-box"));
    let next = settings_with(&rig, |s| s.replace("e2e-box", "other-box"));
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(line.as_deref(), Some("settings changed: e2e now in effect"));
    assert_eq!(recorded(&rig).as_deref(), Some("other-box"));
}

#[test]
fn a_table_taken_away_removes_the_file() {
    let rig = Rig::new("shep");
    rig.e2e_on();
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| {
        s.replace("\n[app.dogs.kelpie.e2e]\nhost = \"e2e-box\"\n", "")
    });
    let mut runner = runner.lock().unwrap();
    let line = runner.reread(next, rig.kelpie_settings()).unwrap();
    assert_eq!(line.as_deref(), Some("settings changed: e2e now in effect"));
    assert_eq!(recorded(&rig), None);
}

#[test]
fn a_reread_refused_by_another_check_leaves_the_file_as_it_was() {
    let rig = Rig::new("shep");
    rig.e2e_on();
    let runner = rig.open().unwrap();
    let next = settings_with(&rig, |s| {
        let s = s.replace("e2e-box", "other-box");
        format!("{s}\n[app.dogs.kelpie.rulings]\npm_alone = [\"security\"]\n")
    });
    let mut runner = runner.lock().unwrap();
    runner.reread(next, rig.kelpie_settings()).unwrap_err();
    assert_eq!(recorded(&rig).as_deref(), Some("e2e-box"));
}
