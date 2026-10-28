//! What a worker with no App token holds of the maintainer's gh login

use super::*;

fn token(text: &str) -> InstallationToken {
    InstallationToken::new(text.to_owned())
}

// A home whose gh folder is on disk, as the maintainer's is on Unix.
fn fence_at(
    home: &Path,
    token: Option<&InstallationToken>,
    login: Option<&InstallationToken>,
) -> Fence {
    let profile = WorkerProfile {
        home,
        token,
        login_token: login,
        ..profile(&[], &[])
    };
    *profile.reach().fence.expect("a worker is fenced")
}

fn home_with_gh() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".config/gh")).unwrap();
    home
}

#[test]
fn a_worker_without_the_login_variable_reads_the_maintainers_gh_folder() {
    let home = home_with_gh();

    let fence = fence_at(home.path(), None, None);

    assert!(fence.read.contains(&home.path().join(".config/gh")));
    assert!(!fence.env.contains_key("GH_TOKEN"));
}

#[test]
fn a_worker_handed_the_login_as_a_variable_reads_no_gh_folder() {
    let home = home_with_gh();
    let login = token("gho_maintainer");

    let fence = fence_at(home.path(), None, Some(&login));

    let granted: Vec<_> = fence.read.iter().map(|p| p.to_string_lossy()).collect();
    assert!(
        !granted.iter().any(|p| p.contains("gh")),
        "a gh folder is granted: {granted:?}"
    );
    assert_eq!(fence.env["GH_TOKEN"], "gho_maintainer");
}

#[test]
fn the_apps_token_wins_over_the_maintainers_login() {
    let home = home_with_gh();
    let (app, login) = (token("ghs_scoped"), token("gho_maintainer"));

    let fence = fence_at(home.path(), Some(&app), Some(&login));

    assert!(!fence.env.contains_key("GH_TOKEN"));
    assert!(fence.env.contains_key("GH_CONFIG_DIR"));
    assert!(!format!("{fence:?}").contains("gho_maintainer"));
}

// A lazy `derive(Debug)` on the fence would print the maintainer's own token
// wherever a call is logged.
#[test]
fn a_fence_prints_the_names_of_its_variables_and_none_of_their_values() {
    let home = home_with_gh();
    let login = token("gho_maintainer");

    let fence = fence_at(home.path(), None, Some(&login));

    let shown = format!("{fence:?}");
    assert!(shown.contains("GH_TOKEN"), "{shown}");
    assert!(!shown.contains("gho_maintainer"), "{shown}");
    assert!(!shown.contains("gc.auto"), "values are elided");
}
