//! A settings change reaching a running runner
//!
//! Every setting takes effect live, at the next step or call that reads
//! it, except `repo` and `forge`: a work item's worktree and pull request
//! belong to them, so those wait for the runner's next start. A change is
//! checked as a start checks it, and one that fails keeps the settings the
//! runner has.

use super::{OpenError, Runner, check_coderabbit, check_local, check_reviewers, instructions};
use crate::review_bot::Bot;
use crate::settings::Settings;
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
        if settings.repo != self.settings.repo {
            waiting.push("repo");
            settings.repo.clone_from(&self.settings.repo);
        }
        if settings.forge != self.settings.forge {
            waiting.push("forge");
            settings.forge = self.settings.forge.clone();
        }
        let reviewers = kelpie.reviewers;
        let gpu_metrics_url = kelpie.gpu_metrics_url.clone();
        let agents = settings.role_agents(&kelpie.agents)?;
        let lineup = settings.lineup(&kelpie, &self.home)?;
        let webhook = kelpie.webhook;
        let mut changed = changed((&self.settings, &self.webhook), (&settings, &webhook));
        if reviewers != self.reviewers {
            changed.push("reviewers");
        }
        if lineup != self.lineup && !changed.contains(&"review") {
            changed.push("local_reviewers");
        }
        if agents != self.agents {
            changed.push("agents");
        }
        if gpu_metrics_url != self.gpu.url() {
            changed.push("gpu_metrics_url");
        }
        if changed.is_empty() && waiting.is_empty() {
            return Ok(None);
        }
        let extra_instructions = instructions::read_extra(&settings)?;
        check_reviewers(&settings, &reviewers, &self.ports)?;
        let listed =
            |s: &Settings| s.coderabbit.enabled && s.reviewers().contains(&Bot::Coderabbit);
        if listed(&settings) && !listed(&self.settings) {
            check_coderabbit(&settings, &self.ports)?;
        }
        if lineup != self.lineup {
            check_local(&settings, &lineup, &self.ports)?;
        }
        crate::skills::check(&settings.skills, &self.paths.skills)?;
        let skills = (settings.skills != self.settings.skills)
            .then(|| Skills::load(&settings.skills, &self.paths.skills));
        self.settings = settings;
        self.reviewers = reviewers;
        self.lineup = lineup;
        self.agents = agents;
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

// The top-level settings that differ. `webhook` also changes when kelpie's
// own settings do.
fn changed((old, was): Reach<'_>, (new, now): Reach<'_>) -> Vec<&'static str> {
    [
        (
            "merge_authority",
            old.merge_authority != new.merge_authority,
        ),
        ("ci", old.ci != new.ci),
        ("max_items", old.max_items != new.max_items),
        ("generated", old.generated != new.generated),
        ("models", old.models != new.models),
        ("review", old.review != new.review),
        ("coderabbit", old.coderabbit != new.coderabbit),
        (
            "pull_request_reviewers",
            old.pull_request_reviewers != new.pull_request_reviewers,
        ),
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
    use crate::settings::{LocalRound, MergeAuthority};
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
            s.replace("merge_authority = \"ask\"", "merge_authority = \"auto\"")
                .replace("divisor = 1000", "divisor = 500")
                .replace("max_items = 1", "max_items = 2")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: merge_authority, max_items, coderabbit now in effect")
        );
        assert_eq!(runner.settings().max_items.get(), 2);
        assert_eq!(runner.settings().merge_authority, MergeAuthority::Auto);
        assert_eq!(runner.settings().coderabbit.divisor.get(), 500);
        assert_eq!(runner.status().merge_authority, MergeAuthority::Auto);
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
    fn repo_and_forge_wait_for_the_next_start() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let repo = rig.repo().display().to_string();
        let next = settings_with(&rig, |s| {
            s.replace("forge = \"shep-pm/shep\"", "forge = \"shep-pm/elsewhere\"")
                .replace(&repo, "/srv/elsewhere")
                .replace("divisor = 1000", "divisor = 500")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some(
                "settings changed: coderabbit now in effect; repo and forge from the runner's next start"
            )
        );
        assert_eq!(runner.settings().forge.as_str(), "shep-pm/shep");
        assert_eq!(runner.settings().repo, rig.repo());
    }

    #[test]
    fn a_local_round_change_is_checked_as_a_start_checks_it() {
        let rig = Rig::new("shep");
        let local = |command: &str| {
            format!("[app.dogs.kelpie.review.local]\nkind = \"command\"\ncommand = \"{command}\"\n")
        };
        let qwen = local("~/.claude/scripts/qwen-review.sh");
        rig.edit_settings(|s| crate::test::with_tables(&s, &qwen));
        let runner = rig.open().unwrap();
        let before = rig.settings().review.local;
        let missing = settings_with(&rig, |s| {
            s.replace("~/.claude/scripts/qwen-review.sh", "/nonexistent/review.sh")
        });
        let mut runner = runner.lock().unwrap();
        let err = runner.reread(missing, rig.kelpie_settings()).unwrap_err();
        assert!(
            err.to_string().starts_with("setting `review.local`"),
            "{err}"
        );
        assert_eq!(runner.settings().review.local, before);

        let off = settings_with(&rig, |s| {
            s.replace(
                "kind = \"command\"\ncommand = \"/nonexistent/review.sh\"",
                "kind = \"off\"",
            )
        });
        let line = runner.reread(off, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: review now in effect")
        );
        assert_eq!(runner.settings().review.local, Some(LocalRound::Off {}));
    }

    #[test]
    fn a_changed_local_reviewer_definition_is_checked_and_named() {
        let rig = Rig::new("shep");
        rig.edit_settings(|s| {
            s.replace(crate::test::OLD_LOCAL, "").replace(
                "[app.dogs.kelpie.review]\n",
                "[app.dogs.kelpie.review]\nreviewers = [\"mine\", \"claude\"]\n",
            )
        });
        let script = rig.home.path().join("review.sh");
        crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
        let base = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
        let define = |command: &str| {
            format!("{base}[local_reviewers.mine]\nkind = \"command\"\ncommand = \"{command}\"\n")
        };
        rig.set_kelpie_settings(&define(&script.display().to_string()));
        let runner = rig.open().unwrap();
        let mut runner = runner.lock().unwrap();

        rig.set_kelpie_settings(&define("/nonexistent/review.sh"));
        let err = runner
            .reread(rig.settings(), rig.kelpie_settings())
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("setting `review.reviewers`: cannot run /nonexistent"),
            "{err}"
        );

        rig.set_kelpie_settings(&define("~/review.sh"));
        let line = runner
            .reread(rig.settings(), rig.kelpie_settings())
            .unwrap();
        assert_eq!(line, None, "the same command, spelt from the home folder");

        rig.set_kelpie_settings(&base);
        let err = runner
            .reread(rig.settings(), rig.kelpie_settings())
            .unwrap_err();
        assert!(err.to_string().contains("mine is not defined"), "{err}");
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
        assert!(err.starts_with("setting `coderabbit.enabled`"), "{err}");
        assert!(!runner.settings().coderabbit.enabled);
        let missing = settings_with(&rig, |s| {
            s.replace(crate::test::CODERABBIT_ON, crate::test::CODERABBIT_OFF)
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
        assert!(Path::new(&rig.paths().settings).exists());
    }
}
