//! A settings change reaching a running runner
//!
//! Every setting takes effect live, at the next step or call that reads
//! it, except `repo` and `forge`: a work item's worktree and pull request
//! belong to them, so those wait for the runner's next start. A change is
//! checked as a start checks it, and one that fails keeps the settings the
//! runner has.

use super::{OpenError, Runner, check_coderabbit, check_local, instructions, ruling_channels};
use crate::channels::Channels;
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
        let (channels, webhook) = ruling_channels(&settings, kelpie)?;
        let changed = changed(
            (&self.settings, &self.channels, &self.webhook),
            (&settings, &channels, &webhook),
        );
        if changed.is_empty() && waiting.is_empty() {
            return Ok(None);
        }
        let extra_instructions = instructions::read_extra(&settings)?;
        if settings.coderabbit.enabled && !self.settings.coderabbit.enabled {
            check_coderabbit(&settings, &self.ports)?;
        }
        if settings.review.local != self.settings.review.local {
            check_local(&settings, &self.ports)?;
        }
        let skills = (settings.skills != self.settings.skills)
            .then(|| Skills::load(&settings.skills, &self.paths.skills));
        self.settings = settings;
        self.extra_instructions = extra_instructions;
        self.channels = channels;
        self.webhook = webhook;
        // Read again under the new pacing settings at the next look.
        self.pacing = None;
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

// A project's settings, the channels its rulings go to, and the webhook.
type Reach<'a> = (&'a Settings, &'a Channels, &'a Option<Webhook>);

// The top-level settings that differ. `ruling_channels` and `webhook` also
// change when kelpie's own settings do.
fn changed((old, went, was): Reach<'_>, (new, goes, now): Reach<'_>) -> Vec<&'static str> {
    [
        (
            "merge_authority",
            old.merge_authority != new.merge_authority,
        ),
        ("ci", old.ci != new.ci),
        ("max_items", old.max_items != new.max_items),
        ("ruling_channels", went != goes),
        ("generated", old.generated != new.generated),
        ("models", old.models != new.models),
        ("review", old.review != new.review),
        ("coderabbit", old.coderabbit != new.coderabbit),
        ("pacing", old.pacing != new.pacing),
        ("worker", old.worker != new.worker),
        ("preview", old.preview != new.preview),
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
    use crate::settings::MergeAuthority;
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
                .replace("loop_guard = 8", "loop_guard = 3")
                .replace("max_items = 1", "max_items = 2")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: merge_authority, max_items, review now in effect")
        );
        assert_eq!(runner.settings().max_items.get(), 2);
        assert_eq!(runner.settings().merge_authority, MergeAuthority::Auto);
        assert_eq!(runner.settings().review.loop_guard.get(), 3);
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
                .replace("loop_guard = 8", "loop_guard = 3")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.kelpie_settings()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some(
                "settings changed: review now in effect; repo and forge from the runner's next start"
            )
        );
        assert_eq!(runner.settings().forge.as_str(), "shep-pm/shep");
        assert_eq!(runner.settings().repo, rig.repo());
    }

    #[test]
    fn a_local_round_change_is_checked_as_a_start_checks_it() {
        let rig = Rig::new("shep");
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
        assert!(!runner.settings().review.local.is_on());
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
    fn a_webhook_taken_from_a_project_that_posts_to_it_is_refused() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let mut runner = runner.lock().unwrap();
        let err = runner
            .reread(rig.settings(), KelpieSettings::default())
            .unwrap_err();
        assert!(
            err.to_string().starts_with("setting `ruling_channels`"),
            "{err}"
        );
        assert!(runner.webhook.is_some());
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
