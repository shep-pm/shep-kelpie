//! The stand-ins that name an adapter, and the ones the adapters' own tests use
//!
//! The rest of the stand-ins are in `shep-kelpie-core`, which the root's rig
//! gathers with these.

mod github;
mod leases;
mod reviewer;

pub use github::{
    Asked, FakeGithub, FakeSigner, HeldToken, PEM, REVIEW_ID, SentReview, ShownCheck,
};
pub use leases::{DogStandIn, FakeLeases, Told};
pub use reviewer::{FakeReviewer, ScriptedRound, SeenRound};

use shep_kelpie_core::test::{AppComments, FakeCodeRabbit, Hold};

pub(crate) use shep_kelpie_core::test::FakeClock;

// A program no machine holds, so a test's policy names one and lets no real
// harness's files through.
#[cfg(test)]
pub(crate) static NO_PROGRAM: std::sync::LazyLock<std::path::PathBuf> =
    std::sync::LazyLock::new(|| std::path::Path::new("/nowhere/program").to_owned());

// What the adapters' own tests use of core's stand-ins and of `test_support`
#[cfg(test)]
pub(crate) use shep_kelpie_core::test::{
    Answer, OpenSandbox, StandInEndpoint, Take, ticket, unreachable_url,
};
#[cfg(test)]
pub(crate) use shep_kelpie_test_support::{
    CALL_PATIENCE, Elsewhere, Fake, cut_from_main, exited, git, linked_worktree, main_commits,
    make_fifo, short_tempdir, waits_for, write_fake, write_script,
};

// A `command` hook that runs `folder`'s `gate.js` beside its `other.js`: the
// worker's policy must read the one and not the other, nor the folder.
#[cfg(test)]
pub(crate) fn hook_beside_another(folder: &std::path::Path) -> shep_kelpie_core::hooks::Hooks {
    std::fs::create_dir_all(folder).unwrap();
    for name in ["gate.js", "other.js"] {
        std::fs::write(folder.join(name), "process.exit(0)\n").unwrap();
    }
    let command = format!("node {}", folder.join("gate.js").display());
    let hook = shep_kelpie_core::settings::ProjectHook {
        point: shep_kelpie_core::settings::HookPoint::Command,
        command: command.try_into().unwrap(),
    };
    shep_kelpie_core::hooks::Hooks::new("koji", "acme/koji", folder, &[hook])
}

// That `policy` reads what `hook_beside_another(folder)` runs and the folder it
// loads its neighbours from, and nothing above that folder
#[cfg(test)]
pub(crate) fn assert_reads_the_hook_and_its_folder(
    policy: &shep_kelpie_core::ports::Policy,
    folder: &std::path::Path,
) {
    for read in [folder.join("gate.js"), folder.to_owned()] {
        assert!(
            policy.read.contains(&read),
            "{read:?} not in {:?}",
            policy.read
        );
    }
    let above = folder.parent().unwrap().to_owned();
    assert!(
        !policy.read.contains(&above),
        "{above:?} in {:?}",
        policy.read
    );
}
