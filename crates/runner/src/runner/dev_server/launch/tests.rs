use super::*;

// Shep's own launch file, one configuration.
const SHEP: &str = include_str!("../../../../../../fixtures/launch.json");

fn two() -> String {
    r#"{"version":"0.0.1","configurations":[
        {"name":"api","runtimeExecutable":"cargo","runtimeArgs":["run"],"port":8080},
        {"name":"web","runtimeExecutable":"npm","port":3000,"cwd":"web"}]}"#
        .to_owned()
}

#[test]
fn a_configuration_is_picked_by_name_else_the_first() {
    let first = select(&two(), None, "main").unwrap().unwrap();
    assert_eq!(first.name.as_deref(), Some("api"));
    let web = select(&two(), Some("web"), "main").unwrap().unwrap();
    assert_eq!(web.runtime_executable, "npm");
    assert_eq!(web.port, Some(3000));
    assert_eq!(web.auto_port, None);
    let err = select(&two(), Some("worker"), "main").unwrap_err();
    assert!(err.contains("worker"), "{err}");
}

#[test]
fn shep_launch_file_reads_with_its_fields() {
    let shep = select(SHEP, None, "main").unwrap().unwrap();
    assert_eq!(shep.name.as_deref(), Some("shep-web"));
    assert_eq!(shep.runtime_executable, "npm");
    assert_eq!(shep.runtime_args, ["run", "dev", "--", "--port", "4421"]);
    assert_eq!(shep.port, Some(4421));
    assert_eq!(shep.cwd.as_deref(), Some("web"));
    assert_eq!(shep.auto_port, Some(true));
}

#[test]
fn an_absent_auto_port_is_none_and_an_explicit_one_is_its_own() {
    let auto_port = |tail: &str| {
        let text =
            format!(r#"{{"configurations":[{{"runtimeExecutable":"npm","port":3000{tail}}}]}}"#);
        select(&text, None, "main").unwrap().unwrap().auto_port
    };
    assert_eq!(auto_port(""), None);
    assert_eq!(auto_port(",\"autoPort\":false"), Some(false));
    assert_eq!(auto_port(",\"autoPort\":true"), Some(true));
}

#[test]
fn a_file_with_no_configuration_or_no_json_starts_nothing_or_is_refused() {
    assert_eq!(select(r#"{"configurations":[]}"#, None, "main"), Ok(None));
    assert_eq!(select("{}", None, "main"), Ok(None));
    assert!(select("not json", None, "main").is_err());
}

fn worktree_with(dirs: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for sub in dirs {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    dir
}

fn with_cwd(cwd: Option<&str>) -> Configuration {
    Configuration {
        name: None,
        runtime_executable: "npm".into(),
        runtime_args: Vec::new(),
        port: Some(3000),
        cwd: cwd.map(str::to_owned),
        auto_port: None,
    }
}

#[test]
fn a_cwd_inside_the_worktree_is_the_folder_and_none_is_the_worktree() {
    let tree = worktree_with(&["web/app"]);
    let root = tree.path();
    assert_eq!(folder(root, &with_cwd(None)).unwrap(), root);
    assert_eq!(folder(root, &with_cwd(Some(""))).unwrap(), root);
    assert_eq!(
        folder(root, &with_cwd(Some("web/app"))).unwrap(),
        root.join("web/app")
    );
}

#[test]
fn a_cwd_that_leaves_the_worktree_is_refused() {
    let tree = worktree_with(&["web"]);
    let root = tree.path();
    let outside = tempfile::tempdir().unwrap();
    for cwd in ["..", "web/../..", "/etc", outside.path().to_str().unwrap()] {
        let err = folder(root, &with_cwd(Some(cwd))).unwrap_err();
        assert!(err.contains("leaves the worktree"), "{cwd}: {err}");
    }
    assert!(folder(root, &with_cwd(Some("missing"))).is_err());
}

#[cfg(unix)]
#[test]
fn a_cwd_that_is_a_link_out_of_the_worktree_is_refused() {
    let tree = worktree_with(&[]);
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), tree.path().join("out")).unwrap();
    let err = folder(tree.path(), &with_cwd(Some("out"))).unwrap_err();
    assert!(err.contains("link"), "{err}");
}

#[test]
fn main_without_the_file_has_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(read(dir.path(), "main"), Ok(None));
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn the_file_is_main_s_and_not_the_one_in_the_working_tree() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    let file = dir.path().join(FILE);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, SHEP).unwrap();
    git(dir.path(), &["add", FILE]);
    git(dir.path(), &["commit", "--quiet", "-m", "launch"]);
    git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    std::fs::write(&file, r#"{"configurations":[{"runtimeExecutable":"sh"}]}"#).unwrap();

    assert_eq!(
        read(dir.path(), "main").unwrap().as_deref(),
        Some(SHEP.trim())
    );
}
