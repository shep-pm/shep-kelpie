//! The real user's config the profile read grant reaches, set on a Windows worker

use super::*;

fn inner() -> Command {
    let mut inner = crate::spawn::command("claude");
    inner.args(["-p", "it's"]).current_dir("/k/wt/7");
    inner
}

fn policy() -> Policy {
    Policy {
        write: vec![PathBuf::from("/k/wt/7")],
        ..Policy::default()
    }
}

// Windows has no home for the sandbox user's git to read, so the install's
// profile read grant names the real user's config, and the embedder sets it.
fn reading_git_config() -> SandboxRuntime {
    SandboxRuntime::new().on(Platform::Windows).with_install(
        WindowsInstall::provisioned(false)
            .reading([("GIT_CONFIG_GLOBAL", "C:\\Users\\kelpie\\.gitconfig")]),
    )
}

fn env_of(command: &Command, name: &str) -> Option<Option<std::ffi::OsString>> {
    command
        .get_envs()
        .find(|(set, _)| *set == name)
        .map(|(_, value)| value.map(ToOwned::to_owned))
}

#[test]
fn a_windows_worker_gets_the_real_users_config_the_profile_read_grant_reaches() {
    let dir = tempfile::tempdir().unwrap();
    let wrapped = reading_git_config()
        .wrap(&policy(), &dir.path().join("s.json"), &inner())
        .unwrap();
    assert_eq!(
        env_of(&wrapped, "GIT_CONFIG_GLOBAL"),
        Some(Some("C:\\Users\\kelpie\\.gitconfig".into()))
    );
}

#[test]
fn a_value_the_command_already_sets_wins_over_the_profile_read_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = inner();
    command.env("git_config_global", "D:\\project\\gitconfig");
    let wrapped = reading_git_config()
        .wrap(&policy(), &dir.path().join("s.json"), &command)
        .unwrap();
    assert_eq!(
        env_of(&wrapped, "git_config_global"),
        Some(Some("D:\\project\\gitconfig".into()))
    );
    assert_eq!(env_of(&wrapped, "GIT_CONFIG_GLOBAL"), None);
}

#[test]
fn a_worker_off_windows_gets_none_of_the_profile_read_grants_variables() {
    let dir = tempfile::tempdir().unwrap();
    for platform in [Platform::Macos, Platform::Linux] {
        let wrapped = reading_git_config()
            .on(platform)
            .wrap(&policy(), &dir.path().join("s.json"), &inner())
            .unwrap();
        assert_eq!(env_of(&wrapped, "GIT_CONFIG_GLOBAL"), None, "{platform:?}");
    }
}
