//! A settings change reaching a running runner
//!
//! Every setting takes effect live, at the next step or call that reads it,
//! except `git.checkout`, `git.remote` and kelpie's gateways: a work item's
//! worktree and pull request belong to the first two, and the adapters hold
//! the gateways, so those wait for the runner's next start. A new `git.base`
//! reaches the work items opened after it; each one in flight keeps the base
//! it was cut for. The agent files are read again with them. A change is
//! checked as a start checks it, and one that fails keeps the settings the
//! runner has.

use super::{
    OpenError, Runner, check_bots, check_coderabbit, check_local, instructions, open_checks, triage,
};
use crate::agents::Agents;
use crate::review_bot::Bot;
use crate::settings::{Gateways, ListedReviewer, Settings, SettingsError, TicketSource};
use crate::skills::{RepoPlugins, Skills, default_head};
use crate::webhook::KelpieSettings;

mod changed;

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
        // The owner's template does not depend on the settings, so it is read whatever they say.
        let (owner_template, note) = super::owner_template::refreshed(
            self.ports.forge.as_ref(),
            self.remote.owner(),
            self.owner_template.take(),
        );
        self.owner_template = owner_template;
        self.notes.extend(note);
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
        // The adapters were built with the gateways at start.
        if !kelpie.gateways().iter().eq(self.gateways.iter()) {
            waiting.push("gateways");
        }
        if let Some(refused) = settings.rulings.refusal() {
            return Err(refused.into());
        }
        // The start refuses a browser this machine cannot give, so a reread does too.
        settings
            .browser
            .supported()
            .map_err(|reason| SettingsError::Invalid {
                setting: "browser.enabled",
                reason,
            })?;
        let gpu_metrics_url = kelpie.gpu_metrics_url.clone();
        let mut book = Agents::load(&self.paths.agents)?;
        book.hold_for(self.state.work_items.iter().map(|item| &item.agent))?;
        let mut agents = settings.role_agents(&book)?;
        let gateways = kelpie.gateways();
        let mut lineup = settings.lineup(&book, &gateways, &self.home)?;
        // The gateways the agents in use were listed under.
        let mut in_use = &gateways;
        // The gateways must hold as a start checks them.
        check_gateways(&gateways, &book, &lineup)?;
        gateways.check_triage_spam(&settings.triage.spam)?;
        // Agents the adapters cannot route yet wait with the gateways they name.
        if check_gateways(&self.gateways, &book, &lineup).is_err() {
            waiting.push("agents");
            settings.agents.clone_from(&self.settings.agents);
            // The listed contributions' reviewers are agents too, and the old book may lack them.
            (settings.contributions.reviewers).clone_from(&self.settings.contributions.reviewers);
            book.clone_from(&self.book);
            // The agents the runner has, under the new settings as a start reads them.
            agents = settings.role_agents(&book)?;
            in_use = &self.gateways;
            lineup = settings.lineup(&book, in_use, &self.home)?;
        }
        // A start refuses a hook that does not resolve, and a change to either table is checked as one.
        if settings.hooks != self.settings.hooks
            || settings.worker.guard_hooks != self.settings.worker.guard_hooks
        {
            super::guard_hooks::check_here(&settings)?;
        }
        let webhook = kelpie.webhook;
        let mut changed = changed::names((&self.settings, &self.webhook), (&settings, &webhook));
        let hooks = (settings.hooks != self.settings.hooks).then(|| {
            let (project, remote) = (&self.project, &self.remote);
            super::guard_hooks::built(project, remote, &settings, &self.metrics)
        });
        // The start's checks of the App and the agent files run on a changed table. The App
        // can lose the repo while the runner runs, which must not refuse an unrelated edit.
        let contribution_lineup = if settings.contributions != self.settings.contributions {
            open_checks::contributions(
                &settings,
                self.ports.voice.as_ref(),
                &self.remote,
                (&book, in_use),
                &self.home,
            )?
        } else if settings.contributions.is_off() {
            Vec::new()
        } else {
            settings.contribution_lineup(&book, in_use, &self.home)?
        };
        if settings.triage != self.settings.triage {
            open_checks::triage(&settings, self.ports.voice.as_ref(), &self.remote, &book)?;
        }
        // `allow` and `slack` are read live, so a change to either must replace the settings
        let (was, now) = (&self.settings.agents, &settings.agents);
        let picking = was.allow != now.allow || was.slack != now.slack;
        if agents != self.agents || lineup != self.lineup || book != self.book || picking {
            changed.push("agents");
        }
        if gpu_metrics_url != self.gpu.url() {
            changed.push("gpu_metrics_url");
        }
        // The default branch may have turned a plugin on or off since the last read.
        // Planned here and written once the checks below hold, so a reread that
        // fails leaves the plugin folders the runner is using as they were.
        let repo_plugins = RepoPlugins::plan(
            &settings.git.checkout,
            settings.git.base(),
            &self.paths.kelpie_home,
            &self.paths.skills,
            self.ports.git.as_ref(),
            &settings.repo_setup,
        );
        let plugins_head = default_head(&settings.git.checkout, settings.git.base());
        if repo_plugins != self.repo_plugins {
            changed.push("repo plugins");
        }
        if changed.is_empty() && waiting.is_empty() {
            self.plugins_head = plugins_head;
            return Ok(None);
        }
        let extra_instructions = instructions::read_extra(&settings)?;
        check_bots(&lineup, &self.ports)?;
        let listed = |lineup: &[ListedReviewer]| {
            // parity: review-bot.private-repos
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
        let had_labels = self.implementer_files();
        // A local start made no `agent:` label on the repo, so a board now on GitHub needs them.
        let to_github = settings.tickets.source != self.settings.tickets.source
            && settings.tickets.source == TicketSource::Github;
        let e2e_changed = settings.e2e != self.settings.e2e;
        // A start stamps or clears it the same way, so it follows `pick` past `off`.
        let was_triaging = self.state.triage.clone();
        if triage::settle_since(&mut self.state, &settings, self.ports.clock.now()) {
            if let Err(e) = self.store.save(&self.state) {
                self.state.triage = was_triaging;
                return Err(e.into());
            }
            if self.state.triage.is_none() {
                // The poll that listed them stops with `pick`, so they would stay as `status` says them.
                self.triage_due.clear();
                self.triage_queue.clear();
                self.flights.end_triage();
            }
        }
        // The listing was made under the old pick, and the next poll is up to a minute off.
        if settings.triage.pick != self.settings.triage.pick {
            self.triage_due.clear();
        }
        // A walk keeps the turn of a bot the lineup drops until it moves on, with its label on.
        if contribution_lineup != self.contribution_lineup {
            self.end_unlisted_turns(&contribution_lineup)?;
        }
        self.settings = settings;
        if e2e_changed {
            // The dog reads the host from this file, so it follows the settings.
            let file = self.paths.e2e();
            self.notes
                .extend(super::e2e::record(self.settings.e2e.as_ref(), &file));
            self.notes.extend(super::e2e::dog_cannot_find(
                self.settings.e2e.as_ref(),
                &self.paths,
            ));
        }
        // A call or a write already running holds the hooks it started with.
        if let Some(hooks) = hooks {
            self.hooks.replace(hooks);
        }
        self.refresh_features();
        // A work item open on a ticket keeps it: the port sends a call about one ticket where its id names.
        self.tickets_source.set(self.settings.tickets.source);
        self.lineup = lineup;
        // A call already running holds the reviewers it started with.
        self.contribution_lineup = contribution_lineup;
        self.agents = agents;
        if book != self.book {
            self.notes.extend(book.skipped());
        }
        self.book = book;
        // A new implementer file needs its `agent:` label made.
        if self.implementer_files() != had_labels || to_github {
            self.make_agent_labels();
            self.make_priority_labels();
        }
        self.notes.extend(self.left_out_notes());
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
        self.plugins_head = plugins_head;
        if repo_plugins != self.repo_plugins {
            let repo_plugins = repo_plugins.install(&self.paths.skills);
            for notice in repo_plugins.notices() {
                line.push_str(&format!("\n{notice}"));
            }
            self.repo_plugins = repo_plugins;
        }
        Ok(Some(line))
    }
}

impl Runner {
    // Sets which features the settings and `main`'s launch file turn on.
    fn refresh_features(&self) {
        let dev_server = super::dev_server::is_on(&self.settings);
        self.metrics.features(&self.settings, dev_server);
    }

    /// Loads the repo's plugins again if its default branch moved since they
    /// were read, which settings of kelpie's own do not announce
    ///
    /// Returns a line for the log when the plugins changed.
    pub fn reread_plugins(&mut self) -> Option<String> {
        let checkout = &self.settings.git.checkout;
        if default_head(checkout, self.settings.git.base()) == self.plugins_head {
            return None;
        }
        let planned = RepoPlugins::plan(
            checkout,
            self.settings.git.base(),
            &self.paths.kelpie_home,
            &self.paths.skills,
            self.ports.git.as_ref(),
            &self.settings.repo_setup,
        );
        // The plan fetched the branch, so this is the commit it read.
        self.plugins_head = default_head(checkout, self.settings.git.base());
        // A launch file the branch gained or lost turns the dev server on or off.
        self.refresh_features();
        if planned == self.repo_plugins {
            return None;
        }
        let planned = planned.install(&self.paths.skills);
        let mut line = String::from("the default branch changed: repo plugins now in effect");
        for notice in planned.notices() {
            line.push_str(&format!("\n{notice}"));
        }
        self.repo_plugins = planned;
        Some(line)
    }
}

// Whether `gateways` route every listed reviewer that names one.
pub(crate) fn check_gateways(
    gateways: &Gateways,
    book: &Agents,
    lineup: &[ListedReviewer],
) -> Result<(), SettingsError> {
    gateways.check_listed(book, lineup.iter().map(|r| &r.name))
}

/// Whether every listed reviewer names a gateway kelpie's settings hold and can reach. No
/// key is read, for a shell that holds none of them.
///
/// # Errors
///
/// A [`SettingsError`] naming the agent whose gateway is missing or cannot be reached.
pub fn check_gateway_shapes(
    gateways: &Gateways,
    book: &Agents,
    lineup: &[ListedReviewer],
) -> Result<(), SettingsError> {
    gateways.check_listed_without_keys(book, lineup.iter().map(|r| &r.name))
}

#[cfg(test)]
mod tests;
