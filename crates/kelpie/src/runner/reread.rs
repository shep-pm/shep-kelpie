//! A settings change reaching a running runner
//!
//! Every setting takes effect live, at the next step or call that reads
//! it, except `repo` and `forge`: a work item's worktree and pull request
//! belong to them, so those wait for the runner's next start. A change is
//! checked as a start checks it, and one that fails keeps the settings the
//! runner has.

use super::{OpenError, Runner, check_coderabbit, instructions};
use crate::settings::Settings;
use crate::webhook::Webhook;

impl Runner {
    /// Takes `settings` and `webhook` as the runner's own, from its next step
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
        webhook: Webhook,
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
        let changed = changed(&self.settings, &settings, &self.webhook, &webhook);
        if changed.is_empty() && waiting.is_empty() {
            return Ok(None);
        }
        let extra_instructions = instructions::read_extra(&settings)?;
        if settings.coderabbit.enabled && !self.settings.coderabbit.enabled {
            check_coderabbit(&settings, &self.ports)?;
        }
        self.settings = settings;
        self.extra_instructions = extra_instructions;
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
        Ok(Some(line))
    }
}

// The top-level settings that differ, `webhook` for kelpie's own.
fn changed(old: &Settings, new: &Settings, was: &Webhook, now: &Webhook) -> Vec<&'static str> {
    [
        (
            "merge_authority",
            old.merge_authority != new.merge_authority,
        ),
        ("ci", old.ci != new.ci),
        ("generated", old.generated != new.generated),
        ("models", old.models != new.models),
        ("review", old.review != new.review),
        ("coderabbit", old.coderabbit != new.coderabbit),
        ("pacing", old.pacing != new.pacing),
        ("worker", old.worker != new.worker),
        ("preview", old.preview != new.preview),
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
    use crate::webhook::{Webhook, WebhookKind};

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
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.webhook()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: merge_authority, review now in effect")
        );
        assert_eq!(runner.settings().merge_authority, MergeAuthority::Auto);
        assert_eq!(runner.settings().review.loop_guard.get(), 3);
        assert_eq!(runner.status().merge_authority, MergeAuthority::Auto);
    }

    #[test]
    fn the_same_settings_change_nothing() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let mut runner = runner.lock().unwrap();
        assert_eq!(runner.reread(rig.settings(), rig.webhook()), Ok(None));
    }

    #[test]
    fn repo_and_forge_wait_for_the_next_start() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let next = settings_with(&rig, |s| {
            s.replace("forge = \"shep-pm/shep\"", "forge = \"shep-pm/elsewhere\"")
        });
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(next, rig.webhook()).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed; forge from the runner's next start")
        );
        assert_eq!(runner.settings().forge.as_str(), "shep-pm/shep");
    }

    #[test]
    fn a_new_webhook_takes_effect_at_once() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let text = "[webhook]\nkind = \"discord\"\nurl = \"https://discord.example/h\"\n";
        let webhook: Webhook = crate::webhook::KelpieSettings::from_section(text)
            .unwrap()
            .webhook;
        let mut runner = runner.lock().unwrap();
        let line = runner.reread(rig.settings(), webhook).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("settings changed: webhook now in effect")
        );
        assert_eq!(runner.webhook.kind, WebhookKind::Discord);
    }

    #[test]
    fn a_change_that_does_not_hold_keeps_the_settings_the_runner_has() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        rig.forge.set_visibility(Visibility::Private);
        rig.coderabbit_on();
        let next = rig.settings();
        let mut runner = runner.lock().unwrap();
        let err = runner.reread(next, rig.webhook()).unwrap_err().to_string();
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
            .reread(missing, rig.webhook())
            .unwrap_err()
            .to_string();
        assert!(err.contains("worker.instructions_file"), "{err}");
        assert_eq!(runner.settings().worker.instructions_file, None);
        assert!(Path::new(&rig.paths().settings).exists());
    }
}
