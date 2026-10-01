use std::fs;
use std::path::Path;

use super::*;
use crate::test::git;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn koji() -> ProjectName {
    ProjectName::try_from("koji").unwrap()
}

// An old `~/.kelpie` as a working install has it: kelpie's own files, one
// project with a live worktree, and files kelpie does not own beside them.
fn populated(old: &Path, checkout: &Path) {
    write(&old.join("settings.toml"), "[webhook]\n");
    write(&old.join("totp/secret"), "s");
    write(&old.join("tools/package.json"), "{}");
    write(&old.join("rulings/3"), "koji");
    write(&old.join("dog/book.json"), "{}");
    write(&old.join("builds/shep-kelpie.previous"), "old build");
    let item = format!(
        r#"{{"items":[{{"issue":7,"worktree":"{0}/wt/koji/7","build":"{0}/targets/koji/7"}}],"note":"{0}/wt/kojix"}}"#,
        old.display()
    );
    write(&old.join("projects/koji/state.json"), &item);
    write(&old.join("projects/koji/worker/settings.json"), "{}");
    write(&old.join("targets/koji/debug/x"), "built");
    write(&old.join("shots/koji/7/a.png"), "png");
    // Not kelpie's.
    write(&old.join("control-center.md"), "notes");
    write(&old.join("handoffs/one.md"), "handoff");
    write(&old.join("targets/bench141/x"), "bench");
    write(&old.join("builds/by-hand"), "a build of the maintainer's");
    write(&old.join("codex/auth.json"), "{}");
    write(&old.join("projects/rotom/state.json"), "{}");
    fs::create_dir_all(checkout).unwrap();
    git(checkout, &["init", "--quiet", "-b", "main"]);
    git(
        checkout,
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
    );
    fs::create_dir_all(old.join("wt/koji")).unwrap();
    let tree = old.join("wt/koji/7");
    git(
        checkout,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "kelpie/7",
            tree.to_str().unwrap(),
        ],
    );
}

// What a runner of koji and the dog move, in the order they move it.
fn everything(old: &Path, new: &Path) -> Result<Vec<String>, String> {
    let mut lines = run(&shared(old, new))?;
    lines.extend(run(&project(old, new, &koji())?)?);
    lines.extend(run(&dog(old, &new.join("dog")))?);
    lines.extend(repoint(old, new, &koji())?);
    Ok(lines)
}

#[test]
fn a_populated_old_home_moves_and_the_second_run_moves_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (
        root.path().join("home/.kelpie"),
        root.path().join("shep/kelpie"),
    );
    let checkout = root.path().join("repos/koji");
    populated(&old, &checkout);

    let lines = everything(&old, &new).unwrap();

    let said = lines.join("\n");
    for moved in [
        "totp",
        "tools",
        "settings.toml",
        "state.json",
        "worktrees",
        "pointed",
    ] {
        assert!(said.contains(moved), "{moved} not reported in:\n{said}");
    }
    assert_eq!(fs::read_to_string(new.join("totp/secret")).unwrap(), "s");
    assert_eq!(fs::read_to_string(new.join("dog/book.json")).unwrap(), "{}");
    assert!(new.join("builds/shep-kelpie.previous").is_file());
    assert!(new.join("koji/worker/settings.json").is_file());
    assert!(new.join("koji/builds/debug/x").is_file());
    assert!(new.join("koji/shots/7/a.png").is_file());
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(new.join("koji/state.json")).unwrap()).unwrap();
    let item = &state["items"][0];
    assert_eq!(
        item["worktree"],
        new.join("koji/worktrees/7").to_str().unwrap()
    );
    assert_eq!(item["build"], new.join("koji/builds/7").to_str().unwrap());
    assert_eq!(
        state["note"],
        format!("{}/wt/kojix", old.display()),
        "another folder"
    );
    // A runner still on the old build reads the shared files through a link.
    assert_eq!(fs::read_link(old.join("tools")).unwrap(), new.join("tools"));
    assert_eq!(fs::read_to_string(old.join("totp/secret")).unwrap(), "s");
    // What kelpie does not own stays.
    for kept in [
        "control-center.md",
        "handoffs/one.md",
        "targets/bench141/x",
        "builds/by-hand",
        "codex/auth.json",
        "projects/rotom/state.json",
    ] {
        assert!(old.join(kept).is_file(), "{kept} moved");
    }
    assert!(!old.join("wt").exists(), "the empty old folder stays");
    assert!(!old.join("projects/koji").exists());
    // Git finds the worktree at its new place.
    let listed = git(&checkout, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains("kelpie/koji/worktrees/7"), "{listed}");
    assert!(!listed.contains("prunable"), "{listed}");

    assert_eq!(everything(&old, &new), Ok(Vec::new()));
    // A file put back at the old place after the move is never taken again.
    write(&old.join("projects/koji/state.json"), "{}");
    assert_eq!(everything(&old, &new), Ok(Vec::new()));
    assert!(old.join("projects/koji/state.json").is_file());
}

#[test]
fn a_second_shepherd_with_nothing_left_to_move_starts() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join(".kelpie");
    let (first, second) = (root.path().join("a/kelpie"), root.path().join("b/kelpie"));
    write(&old.join("totp/secret"), "s");
    run(&shared(&old, &first)).unwrap();

    assert_eq!(run(&shared(&old, &second)), Ok(Vec::new()));
    assert_eq!(
        run(&project(&old, &second, &koji()).unwrap()),
        Ok(Vec::new())
    );
}

#[test]
fn a_second_home_is_refused_what_the_old_home_still_holds_for_the_first() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join(".kelpie");
    let (first, second) = (root.path().join("a/kelpie"), root.path().join("b/kelpie"));
    write(&old.join("totp/secret"), "s");
    write(&old.join("projects/koji/state.json"), "{}");
    run(&shared(&old, &first)).unwrap();

    let error = run(&project(&old, &second, &koji()).unwrap()).unwrap_err();

    assert!(error.contains(&first.display().to_string()), "{error}");
    assert!(error.contains("KELPIE_HOME"), "{error}");
    assert!(old.join("projects/koji/state.json").is_file());
}

#[test]
fn a_shared_file_made_in_the_new_home_first_is_kept_and_the_rest_moves() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (root.path().join(".kelpie"), root.path().join("kelpie"));
    write(&old.join("tools/package.json"), "old");
    write(&old.join("totp/secret"), "s");
    write(&new.join("tools/package.json"), "installed first");

    let lines = run(&shared(&old, &new)).unwrap();

    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("left") && l.contains("tools")),
        "{lines:?}"
    );
    assert_eq!(
        fs::read_to_string(new.join("tools/package.json")).unwrap(),
        "installed first"
    );
    assert!(
        !fs::symlink_metadata(old.join("tools"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read_link(old.join("totp")).unwrap(), new.join("totp"));
}

#[test]
fn a_project_folder_that_is_a_shepherds_home_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join(".kelpie");
    write(&home.join("projects/shep/state.json"), "{}");
    write(&home.join("shep/flock.json"), "{}");
    let shep = ProjectName::try_from("shep").unwrap();

    let error = run(&project(&home, &home, &shep).unwrap()).unwrap_err();

    assert!(error.contains("shepherd's home"), "{error}");
    assert!(home.join("projects/shep/state.json").is_file());
}

#[test]
fn a_move_that_cannot_happen_stops_before_the_state_file_moves() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (root.path().join(".kelpie"), root.path().join("kelpie"));
    write(&old.join("projects/koji/state.json"), "{}");
    write(&old.join("wt/koji/7/a.rs"), "old");
    write(&new.join("koji/worktrees/7/a.rs"), "new");

    let error = run(&project(&old, &new, &koji()).unwrap()).unwrap_err();

    assert!(error.contains("already there"), "{error}");
    assert!(
        old.join("projects/koji/state.json").is_file(),
        "the state stays with its worktrees"
    );
    assert!(!new.join("koji/state.json").exists());
    assert!(!new.join("koji").join(MARKER).exists());
}

#[test]
fn a_shepherds_home_is_never_moved_from() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (root.path().join(".kelpie"), root.path().join("shep/kelpie"));
    write(&old.join("settings.toml"), "theirs");
    write(&old.join("flock.json"), "{}");

    let lines = run(&shared(&old, &new)).unwrap();

    assert!(lines[0].contains("shepherd's home"), "{lines:?}");
    assert!(old.join("settings.toml").is_file());
    assert!(!new.exists());
}

#[test]
fn a_new_home_inside_the_old_one_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join(".kelpie");
    let new = old.join("shep/kelpie");
    write(&old.join("totp/secret"), "s");

    let error = run(&shared(&old, &new)).unwrap_err();

    assert!(error.contains("move the shepherd out"), "{error}");
    assert!(old.join("totp/secret").is_file());
}

#[test]
fn a_start_that_died_before_its_link_makes_it_next_time() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (root.path().join(".kelpie"), root.path().join("kelpie"));
    write(&old.join("totp/secret"), "s");
    fs::create_dir_all(&new).unwrap();
    fs::rename(old.join("totp"), new.join("totp")).unwrap();

    run(&shared(&old, &new)).unwrap();

    assert_eq!(fs::read_link(old.join("totp")).unwrap(), new.join("totp"));
}

#[test]
fn a_sweep_takes_the_links_and_a_dead_door_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (root.path().join(".kelpie"), root.path().join("kelpie"));
    write(&old.join("totp/secret"), "s");
    write(&old.join("handoffs/one.md"), "handoff");
    run(&shared(&old, &new)).unwrap();
    fs::create_dir_all(old.join("dog")).unwrap();
    drop(std::os::unix::net::UnixListener::bind(old.join("dog/lease.sock")).unwrap());
    assert_eq!(links(&old, &new), [old.join("totp")]);

    let lines = sweep(&old, &new);

    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(fs::symlink_metadata(old.join("totp")).is_err());
    assert!(!old.join("dog").exists());
    assert!(new.join("totp/secret").is_file());
    assert!(old.join("handoffs/one.md").is_file());
    assert_eq!(links(&old, &new), Vec::<PathBuf>::new());
}

#[test]
fn a_kelpie_home_set_by_hand_takes_the_new_layout_in_place() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("k");
    write(&home.join("totp/secret"), "s");
    write(&home.join("projects/koji/state.json"), "{}");
    write(&home.join("wt/koji/7/a.rs"), "fn main() {}");

    run(&shared(&home, &home)).unwrap();
    run(&project(&home, &home, &koji()).unwrap()).unwrap();

    assert!(home.join("totp/secret").is_file());
    assert!(
        !fs::symlink_metadata(home.join("totp"))
            .unwrap()
            .is_symlink()
    );
    assert!(home.join("koji/state.json").is_file());
    assert!(home.join("koji/worktrees/7/a.rs").is_file());
    assert!(!home.join("projects").exists());
}
