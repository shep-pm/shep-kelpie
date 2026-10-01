use std::fs;
use std::path::Path;

use super::*;
use crate::test::git;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

// An old `~/.kelpie` as a working install has it: kelpie's own files, one
// project with a live worktree, and files kelpie does not own beside them.
fn populated(old: &Path, checkout: &Path) {
    write(&old.join("settings.toml"), "[webhook]\n");
    write(&old.join("totp/secret"), "s");
    write(&old.join("tools/package.json"), "{}");
    write(&old.join("codex/auth.json"), "{}");
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

fn everything(old: &Path, new: &Path, koji: &ProjectName) -> Vec<Move> {
    let mut moves = shared(old, new);
    moves.extend(project(old, new, koji));
    moves.extend(dog(old, &new.join("dog")));
    moves
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
    let koji = ProjectName::try_from("koji").unwrap();

    let mut lines = run(&new, &everything(&old, &new, &koji)).unwrap();
    lines.extend(repoint(&old, &new, &koji).unwrap());

    let said = lines.join("\n");
    for moved in [
        "totp",
        "tools",
        "codex",
        "settings.toml",
        "state.json",
        "worktrees",
    ] {
        assert!(said.contains(moved), "{moved} not reported in:\n{said}");
    }
    assert_eq!(fs::read_to_string(new.join("totp/secret")).unwrap(), "s");
    assert_eq!(fs::read_to_string(new.join("dog/book.json")).unwrap(), "{}");
    assert!(new.join("builds/shep-kelpie.previous").is_file());
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
    assert!(new.join("koji/worker/settings.json").is_file());
    assert!(new.join("koji/builds/debug/x").is_file());
    assert!(new.join("koji/shots/7/a.png").is_file());
    assert!(new.join("koji/worktrees/7/.git").is_file());
    // A runner still on the old build reads the shared files through a link.
    assert_eq!(fs::read_link(old.join("tools")).unwrap(), new.join("tools"));
    assert_eq!(fs::read_to_string(old.join("totp/secret")).unwrap(), "s");
    // What kelpie does not own stays.
    for kept in [
        "control-center.md",
        "handoffs/one.md",
        "targets/bench141/x",
        "builds/by-hand",
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
    assert_eq!(
        git(&new.join("koji/worktrees/7"), &["branch", "--show-current"]),
        "kelpie/7"
    );

    let again = run(&new, &everything(&old, &new, &koji)).unwrap();
    assert_eq!(again, Vec::<String>::new());
    assert_eq!(repoint(&old, &new, &koji), Ok(None));
}

#[test]
fn an_item_whose_new_place_is_taken_stays_and_is_named() {
    let root = tempfile::tempdir().unwrap();
    let (old, new) = (root.path().join(".kelpie"), root.path().join("kelpie"));
    write(&old.join("totp/secret"), "old");
    write(&new.join("totp/secret"), "new");

    let lines = run(&new, &shared(&old, &new)).unwrap();

    assert_eq!(fs::read_to_string(old.join("totp/secret")).unwrap(), "old");
    assert_eq!(fs::read_to_string(new.join("totp/secret")).unwrap(), "new");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("left") && l.contains("totp")),
        "{lines:?}"
    );
}

#[test]
fn a_kelpie_home_set_by_hand_takes_the_new_layout_in_place() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("k");
    write(&home.join("totp/secret"), "s");
    write(&home.join("projects/koji/state.json"), "{}");
    write(&home.join("wt/koji/7/a.rs"), "fn main() {}");
    let koji = ProjectName::try_from("koji").unwrap();
    let mut moves = shared(&home, &home);
    moves.extend(project(&home, &home, &koji));

    run(&home, &moves).unwrap();

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
