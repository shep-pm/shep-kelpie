//! A settings change reaching a running runner
//!
//! Every setting takes effect live, at the next step or call that reads
//! it, except `git.checkout` and `git.remote`: a work item's worktree and
//! pull request belong to them, so those wait for the runner's next start. The agent
//! files are read again with them, and a change to those alone is a change
//! too. A change is checked as a start checks it, and one that fails keeps
//! the settings the runner has.

use super::{OpenError, Runner, check_bots, check_coderabbit, check_local, instructions};
use crate::agents::Agents;
use crate::review_bot::Bot;
use crate::settings::{ListedReviewer, Settings};
use crate::skills::Skills;
use crate::webhook::{KelpieSettings, Webhook};

impl Runner {
    /// Takes `settings` and kelpie's own as the runner's, from its next step
    ///
    /// Returns a line for the log naming what changed and what waits for
    /// the next start, or nothing when nothing changed.
    ///
    /// # Errors
    ///
    /// [`OpenError`] naming the setting that does not hold. Nothing changes then.
    pub fn reread(
        &mut self,
        mut settings: Settings,
        kelpie: KelpieSettings,
    ) -> Result<Option<String>, OpenError> {
        let mut waiting = Vec::new();
        if settings.git.checkout != self.settings.git.checkout {
            waiting.push("git.checkout");
            settings
                .git
                .checkout
                .clone_from(&self.settings.git.checkout);
        }
        if settings.git.remote != self.settings.git.remote {
            waiting.push("git.remote");
            settings.git.remote.clone_from(&self.settings.git.remote);
        }
        let gpu_metrics_url = kelpie.gpu_metrics_url.clone();
        let book = Agents::load(&self.paths.agents)?;
        let agents = settings.role_agents(&book)?;
        let lineup = settings.lineup(&book, &self.home)?;
        let listed = agents.implementers.iter().map(|i| &i.name);
        let reviewers = lineup.iter().map(|r| &r.name);
        (kelpie.gateways()).check_listed(&book, listed.chain(reviewers))?;
        let webhook = kelpie.webhook;
        let mut changed = changed((&self.settings, &self.webhook), (&settings, &webhook));
        if agents != self.agents || lineup != self.lineup || book != self.book {
            changed.push("agents");
        }
        if gpu_metrics_url != self.gpu.url() {
            changed.push("gpu_metrics_url");
        }
        if changed.is_empty() && waiting.is_empty() {
            return Ok(None);
        }
        let extra_instructions = instructions::read_extra(&settings)?;
        check_bots(&lineup, &self.ports)?;
        let listed = |lineup: &[ListedReviewer]| {
            let coderabbit = |r: &ListedReviewer| r.bot().is_some_and(|b| b.bot == Bot::Coderabbit);
            lineup.iter().any(coderabbit)
        };
        if listed(&lineup) && !listed(&self.lineup) {
            check_coderabbit(&self.remote, &lineup, &self.ports)?;
        }
        if lineup != self.lineup {
            check_local(&lineup, &self.ports)?;
        }
        crate::skills::check(&settings.skills, &self.paths.skills)?;
        let skills = (settings.skills != self.settings.skills)
            .then(|| Skills::load(&settings.skills, &self.paths.skills));
        self.settings = settings;
        self.lineup = lineup;
        self.agents = agents;
        if book != self.book {
            self.notes.extend(book.skipped());
        }
        self.book = book;
        self.gpu.point_at(gpu_metrics_url);
        self.extra_instructions = extra_instructions;
        self.webhook = webhook;
        // Read again under the new pacing settings at the next look.
        self.pacing.clear();
        let mut line = String::from("settings changed");
        if !changed.is_empty() {
            line.push_str(&format!(": {} now in effect", changed.join(", ")));
        }
        if !waiting.is_empty() {
            line.push_str(&format!(
                "; {} from the runner's next start",
                waiting.join(" and ")
            ));
        }
        if let Some(skills) = skills {
            for notice in skills.notices() {
                line.push_str(&format!("\n{notice}"));
            }
            self.skills = skills;
        }
        Ok(Some(line))
    }
}

// A project's settings and the webhook.
type Reach<'a> = (&'a Settings, &'a Option<Webhook>);

// The settings that differ, by key or by table. `webhook` also changes
// when kelpie's own settings do.
fn changed((old, was): Reach<'_>, (new, now): Reach<'_>) -> Vec<&'static str> {
    [
        ("git.merging", old.git.merging != new.git.merging),
        ("git.issues", old.git.issues != new.git.issues),
        ("ci", old.ci != new.ci),
        ("concurrency", old.concurrency != new.concurrency),
        ("pacing", old.pacing != new.pacing),
        ("worker", old.worker != new.worker),
        ("skills", old.skills != new.skills),
        ("webhook", was != now),
    ]
    .into_iter()
    .filter_map(|(name, differs)| differs.then_some(name))
    .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::ports::Visibility;
    use crate::settings::{Filing, Merging};
    use crate::test::Rig;
    use crate::webhook::{KelpieSettings, WebhookKind};

    fn settings_with(rig: &Rig, edit: impl FnOnce(String) -> String) -> crate::settings::Settings {
        rig.edit_settings(edit);
        rig.settings()
    }

    #[test]
    fn a_changed_setting_takes_effect_at_once() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let next = settings_with(&rig, |s| {
            s.replace("merging = \"ask\"", "merging = \"auto\"")
                .replace("issues = \"ask\"", "issues = \"skip\"")
                .replace("fix_attempts = -1", "fix_attempts = 2")
                .replace("kickoff_hours = 8", "kickoff_hours = 6")
                .replace("active_items = 1", "active_items = 2")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some(
                "settings changed: git.merging, git.issues, ci, concurrency, pacing now in effect"
            )
        );
        assert_eq!(runner.settings().concurrency.active_items.get(), 2);
        assert_eq!(runner.settings().git.merging, Merging::Auto);
        assert_eq!(runner.settings().git.issues, Filing::Skip);
        assert_eq!(runner.settings().ci.fix_attempts.cap(), Some(2));
        assert_eq!(runner.settings().pacing.kickoff_hours.get(), 6);
        assert_eq!(runner.status().merging, Merging::Auto);
    }

    #[test]
    fn the_same_settings_change_nothing() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let mut runner = runner.lock().unwrap();
        assert_eq!(
            runner.reread(rig.settings(), rig.kelpie_settings()),
            Ok(None)
        );
    }

    #[test]
    fn the_checkout_and_the_remote_wait_for_the_next_start() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let repo = rig.repo().display().to_string();
        let next = settings_with(&rig, |s| {
            s.replace(
                "remote = \"shep-pm/shep\"",
                "remote = \"shep-pm/elsewhere\"",
            )
            .replace(&repo, "/srv/elsewhere")
            .replace("kickoff_hours = 8", "kickoff_hours = 6")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some(
                "settings changed: pacing now in effect; git.checkout and git.remote from the \
                 runner's next start"
            )
        );
        assert_eq!(runner.remote().as_str(), "shep-pm/shep");
        assert_eq!(runner.settings().git.checkout, rig.repo());
    }

    #[test]
    fn a_changed_reviewer_file_is_checked_as_a_start_checks_it_and_named() {
        let rig = Rig::new("shep");
        rig.reviewers(&["mine", "claude"]);
        let script = rig.home.path().join("review.sh");
        crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
        let define = |command: &str| {
            let text = format!("---\nrole: reviewer\nharness: command\ncommand: {command}\n---\n");
            rig.write_agent("mine", &text);
        };
        define(&script.display().to_string());
        let runner = rig.open().unwrap();
        let mut runner = runner.lock().unwrap();

        define("/nonexistent/review.sh");
        let err = runner
            .reread(rig.settings(), rig.kelpie_settings())
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("setting `agents.reviewers`: mine: cannot run /nonexistent"),
            "{err}"
        );

        define("~/review.sh");
        let line = runner
            .reread(rig.settings(), rig.kelpie_settings())
            .unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: agents now in effect"),
            "the same command, spelt from the home folder, in an edited file"
        );

        std::fs::remove_file(rig.paths().agents.join("mine.md")).unwrap();
        let err = runner
            .reread(rig.settings(), rig.kelpie_settings())
            .unwrap_err();
        assert!(
            err.to_string().contains("mine, which has no agent file"),
            "{err}"
        );
    }

    #[test]
    fn a_new_webhook_takes_effect_at_once() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let text = "[webhook]\nkind = \"discord\"\nurl = \"https://discord.example/h\"\n";
        let kelpie = KelpieSettings::from_section(text).unwrap();
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(rig.settings(), kelpie).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: webhook now in effect")
        );
        assert_eq!(runner.webhook.as_ref().unwrap().kind, WebhookKind::Discord);
    }

    #[test]
    fn a_webhook_taken_away_takes_effect_at_once() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let mut runner = runner.lock().unwrap();
        let line = runner
            .reread(rig.settings(), KelpieSettings::default())
            .unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: webhook now in effect")
        );
        assert!(runner.webhook.is_none());
    }

    #[test]
    fn a_change_that_does_not_hold_keeps_the_settings_the_runner_has() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.forge.set_visibility(Visibility::Private);
        rig.coderabbit_on();
        let next = rig.settings();
        let mut runner = runner.lock().unwrap();
        let err = runner
            .reread(next, rig.kelpie_settings())
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("setting `agents.reviewers`: coderabbit"),
            "{err}"
        );
        let listed = runner
            .settings()
            .agents
            .reviewers
            .clone()
            .unwrap_or_default();
        assert!(!listed.iter().any(|name| name.as_str() == "coderabbit"));
        let missing = settings_with(&rig, |s| {
            s.replace(
                crate::test::RIG_REVIEWERS_AND_CODERABBIT,
                crate::test::RIG_REVIEWERS,
            )
            .replace(
                "build_env = {}\n",
                "instructions_file = \"/nonexistent/w.md\"\n",
            )
        });
        let err = runner
            .reread(missing, rig.kelpie_settings())
            .unwrap_err()
            .to_string();
        assert!(err.contains("worker.instructions_file"), "{err}");
        assert_eq!(runner.settings().worker.instructions_file, None);
        assert!(Path::new(&rig.settings_file()).exists());
    }
}
