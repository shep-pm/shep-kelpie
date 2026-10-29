//! The rig's settings edits, which a test makes before it next opens a runner
//!
//! Split out of `crates/runner/src/test/mod.rs`, which the size limit (IR-48) would not fit.

use std::sync::{Arc, Mutex};

use super::{
    LeaseReads, NO_RETRO, RETRO_ON, RIG_REVIEWERS_AND_CODERABBIT, Rig, project_table, write_in,
};
use crate::pacer::DayStart;
use crate::ports::{LocalLeases, Timestamp};
use crate::runner::Runner;
use crate::settings::{Settings, SettingsError};
use crate::state::StateStore;

impl Rig {
    /// Registers kelpie's App for the project's repo owner and installs it on
    /// the repo, so a runner opened next posts through it
    pub fn with_app(&self) {
        let kelpie_home = self.paths().kelpie_home;
        self.github.registered(&kelpie_home, Self::REMOTE);
    }

    /// Takes the App's registration away, as an owner removing it does, so
    /// the voice stops covering the repo
    pub fn without_app(&self) {
        let kelpie_home = self.paths().kelpie_home;
        std::fs::remove_dir_all(kelpie_home.join(crate::github::FOLDER)).unwrap();
    }

    /// Makes the rig's project kelpie's own repo, which is the one the sandbox-red
    /// list in `crates/runner/src/runner/ci_failure/mod.rs` is of
    pub fn remote_is_kelpie(&self) {
        self.edit_settings(|s| {
            let remote = format!("remote = \"{}\"", Self::REMOTE);
            assert!(s.contains(&remote), "the rig's remote moved");
            s.replace(&remote, "remote = \"shep-pm/shep-kelpie\"")
        });
    }

    /// Lists CodeRabbit after the rig's reviewers, so it reads each pass last
    pub fn coderabbit_on(&self) {
        self.reviewers(RIG_REVIEWERS_AND_CODERABBIT);
    }

    /// Makes the project's `git.merging` `auto`, read when a runner next opens
    pub fn merge_auto(&self) {
        let (ask, auto) = ("merging = \"ask\"", "merging = \"auto\"");
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's `git.merging` moved");
            s.replace(ask, auto)
        });
    }

    /// Makes `branch` the project's base: origin gets it, one commit on from
    /// `main` so the two differ, and `git.base` names it for a runner that opens or rereads
    pub fn base_is(&self, branch: &str) {
        let repo = self.repo();
        let on = |args: &[&str]| crate::test::git(&repo, args);
        on(&["checkout", "--quiet", "-b", branch, "main"]);
        std::fs::write(repo.join("NEXT.md"), "the long-lived line\n").unwrap();
        on(&["add", "NEXT.md"]);
        on(&["commit", "--quiet", "-m", "start the line"]);
        on(&["push", "--quiet", "origin", branch]);
        on(&["checkout", "--quiet", "main"]);
        let ask = "issues = \"ask\"";
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's `git.issues` moved");
            s.replace(ask, &format!("{ask}\nbase = \"{branch}\""))
        });
    }

    /// Sets the project's `git.maintainer` to `login`, read when a runner next opens
    pub fn maintainer(&self, login: &str) {
        let ask = "issues = \"ask\"";
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's `git.issues` moved");
            s.replace(ask, &format!("{ask}\nmaintainer = \"{login}\""))
        });
    }

    /// Gives the project a hook at each of `points` that runs `script`, a
    /// shell script written to the rig's home as `name`, read when a
    /// runner next opens
    pub fn hook(&self, name: &str, points: &[&str], script: &str) {
        let file = self.home.path().join(name);
        std::fs::write(&file, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let hooks: String = points
            .iter()
            .map(|point| {
                format!(
                    "\n[[app.dogs.kelpie.hooks]]\npoint = \"{point}\"\ncommand = \"{}\"\n",
                    file.display()
                )
            })
            .collect();
        self.edit_settings(|s| s + &hooks);
    }

    /// Gives the project a hook at each of `points` that refuses any text
    /// naming `word`, exit 2 with a reason that does not repeat it, read
    /// when a runner next opens
    pub fn forbid_word(&self, points: &[&str], word: &str) {
        let script = format!(
            "if grep -q '{word}'; then echo 'it names a word the project keeps private' >&2; exit 2; fi"
        );
        self.hook("forbid-word.sh", points, &script);
    }

    /// Has the project take its tickets from the local store, read when a runner
    /// next opens
    pub fn tickets_local(&self) {
        let github = "[app.dogs.kelpie.tickets]\nsource = \"github\"";
        self.edit_settings(|s| {
            assert!(s.contains(github), "the example's `tickets.source` moved");
            s.replace(github, "[app.dogs.kelpie.tickets]\nsource = \"local\"")
        });
    }

    /// Runs `shep kelpie ticket <args>` on the project's local store, and returns
    /// its lines
    pub fn ticket_verb(&self, args: &[&str]) -> Vec<String> {
        crate::local_tickets::verb::run(&self.paths().tickets(), args).unwrap()
    }

    /// Has the project block on CI, read when a runner next opens
    pub fn ci_on(&self) {
        self.edit_settings(|s| s.replace("block = false", "block = true"));
    }

    /// [`Rig::ci_on`] for a runner already open, which reads its settings again
    pub fn ci_on_now(&self, runner: &Mutex<Runner>) {
        self.ci_on();
        let (settings, kelpie) = (self.settings(), self.kelpie_settings());
        runner.lock().unwrap().reread(settings, kelpie).unwrap();
    }

    /// Lets the project run its retro, read when a runner next opens
    pub fn retro_on(&self) {
        self.edit_settings(|s| {
            assert!(s.contains(NO_RETRO), "the rig's no-retro table moved");
            s.replace(NO_RETRO, RETRO_ON)
        });
    }

    /// Leaves the retro out of the project's settings, read when a runner next opens
    pub fn retro_left_out(&self) {
        self.edit_settings(|s| {
            assert!(s.contains(NO_RETRO), "the rig's no-retro table moved");
            s.replace(NO_RETRO, "")
        });
    }

    /// Sets the project's `git.issues` to `filing`, read when a runner next opens
    pub fn issues(&self, filing: &str) {
        let ask = "issues = \"ask\"";
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's `git.issues` moved");
            s.replace(ask, &format!("issues = \"{filing}\""))
        });
    }

    /// Sets the project's `ci.fix_attempts` to `cap`, read when a runner next opens
    pub fn fix_attempts(&self, cap: i64) {
        let none = "fix_attempts = -1";
        self.edit_settings(|s| {
            assert!(s.contains(none), "the example's `ci.fix_attempts` moved");
            s.replace(none, &format!("fix_attempts = {cap}"))
        });
    }

    /// Sets the project's `ci.test_command`, read when a runner next opens
    pub fn test_command(&self, command: &str) {
        let off = "# test_command = \"cargo test --test {name}\"";
        self.edit_settings(|s| {
            assert!(s.contains(off), "the example's `ci.test_command` moved");
            s.replace(off, &format!("test_command = \"{command}\""))
        });
    }

    /// Writes kelpie's agent file `name.md` holding `text`, read when a runner next opens
    pub fn write_agent(&self, name: &str, text: &str) {
        write_in(&self.paths().agents, &format!("{name}.md"), text);
    }

    /// An implementer file on the Codex account, which the agent board may pick
    /// once a test writes it
    pub const CODEX_IMPLEMENTER: &str = "---\nrole: implementer\nharness: stand-in\n\
                                         model: gpt-5-codex\neffort: medium\nusage: codex\n---\n";

    /// Writes the `codex` implementer file
    pub fn write_codex_implementer(&self) {
        self.write_agent("codex", Self::CODEX_IMPLEMENTER);
    }

    /// Saves a state in which Codex's day began with `used` percent of its week
    /// spent, read when a runner next opens
    pub fn codex_day_began(&self, used: u32) {
        let store = StateStore::new(self.paths().state);
        let mut state = store.load().unwrap().unwrap_or_default();
        state.codex_pacing = Some(DayStart {
            week_resets_at: Timestamp(Self::EPOCH + 7 * Self::DAY),
            day: 0,
            week_used_pct: used,
        });
        store.save(&state).unwrap();
    }

    /// Lists `names` as the project's reviewers, and has each read the pull
    /// request by setting `reviews.need` to their count, read when a runner next opens
    pub fn reviewers(&self, names: &[&str]) {
        self.agent_board
            .ranks_first(&[&["sonnet-high"][..], names].concat());
        // An empty list reads nobody, and `reviews.need` is at least one.
        self.reviews_need(Some(names.len().max(1)));
    }

    /// Sets `reviews.need` to `need`, or leaves it out for the default when
    /// `None`, read when a runner next opens
    pub fn reviews_need(&self, need: Option<usize>) {
        const TABLE: &str = "\n[app.dogs.kelpie.reviews]\nneed = ";
        self.edit_settings(|mut s| {
            if let Some(at) = s.find(TABLE) {
                let end = s[at + TABLE.len()..]
                    .find('\n')
                    .map_or(s.len(), |n| at + TABLE.len() + n + 1);
                s.replace_range(at..end, "");
            }
            match need {
                Some(need) => format!("{s}{TABLE}{need}\n"),
                None => s,
            }
        });
    }

    /// Gives the project an e2e host, `e2e-box`, read when a runner next opens
    pub fn e2e_on(&self) {
        self.edit_settings(|s| s + "\n[app.dogs.kelpie.e2e]\nhost = \"e2e-box\"\n");
    }

    /// Rewrites the project's settings file through `edit`
    pub fn edit_settings(&self, edit: impl FnOnce(String) -> String) {
        let file = self.settings_file();
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, edit(text)).unwrap();
    }

    /// The project's settings as its table now stands
    pub fn settings(&self) -> Settings {
        self.try_settings().unwrap()
    }

    /// [`Rig::settings`], with the error the project's settings give rather than a panic
    ///
    /// The rig keeps the runner's Flockfile entry where the old settings file was.
    ///
    /// # Errors
    ///
    /// The [`SettingsError`] the project's table gives.
    pub fn try_settings(&self) -> Result<Settings, SettingsError> {
        let paths = self.paths();
        let entry = std::fs::read_to_string(self.settings_file()).unwrap();
        let folder = &paths.folder;
        let (project, home) = (self.project.as_str(), self.home.path());
        Settings::from_table(&project_table(&entry), project, home, folder)
    }

    /// Answers the gateway's lease reads with nothing, and counts them, so a
    /// test can say who asked the gateway
    pub fn count_lease_reads(&self) -> Arc<LeaseReads> {
        let reads = Arc::new(LeaseReads::default());
        *self.counted_leases.lock().unwrap() = Some(Arc::clone(&reads));
        reads
    }

    /// Answers the gateway's lease reads with `client` holding its model for
    /// `what`, so a test can see who a wake would be told holds it
    pub fn lease_held_by(&self, client: &str, what: &str) -> Arc<LeaseReads> {
        let reads = self.count_lease_reads();
        reads.held_by(client, what);
        reads
    }

    // The gateway's leases as the rig answers them: the real stand-in, or the
    // counted one once a test asked for it.
    pub(super) fn lease_port(&self) -> Arc<dyn LocalLeases> {
        self.counted_leases
            .lock()
            .unwrap()
            .clone()
            .map(|reads| reads as Arc<dyn LocalLeases>)
            .unwrap_or_else(|| Arc::new(self.local_leases.clone()))
    }
}
