//! Putting a planning call's pick on an issue as its `worker:` label
//!
//! A repo starts with none of the picks' labels, and the forge refuses to
//! add a label the repo lacks, so the first write of each makes it.

use crate::ports::{Forge, NewLabel};
use crate::runner::Runner;
use crate::settings::ForgeSlug;

/// The repo's labels, read the first time a plan's pick needs them
#[derive(Debug, Default)]
pub(super) struct RepoLabels(Option<Vec<String>>);

impl RepoLabels {
    // Makes `label` on `repo` unless it has it. Err says why it could not.
    fn make(
        &mut self,
        forge: &dyn Forge,
        repo: &ForgeSlug,
        label: &NewLabel,
    ) -> Result<(), String> {
        let names = match self.0.take() {
            Some(names) => names,
            None => forge
                .repo_labels(repo)
                .map_err(|e| format!("could not read the repo's labels: {e}"))?,
        };
        let names = self.0.insert(names);
        if names.iter().any(|n| n == label.name) {
            return Ok(());
        }
        forge
            .create_label(repo, label)
            .map_err(|e| format!("could not make label `{}`: {e}", label.name))?;
        names.push(label.name.to_owned());
        Ok(())
    }
}

impl Runner {
    // Puts `label` on issue `number`, making it on the repo first when it
    // lacks it. The write is tried even when that fails, since another may
    // have made it. Err says why the label did not land.
    pub(super) fn put_pick(
        &self,
        known: &mut RepoLabels,
        number: u64,
        label: &NewLabel,
    ) -> Result<(), String> {
        let (forge, repo) = (&*self.ports.forge, &self.settings.forge);
        let made = known.make(forge, repo, label);
        let put = forge.set_issue_label(repo, number, label.name, true);
        put.map_err(|e| {
            made.err()
                .unwrap_or_else(|| format!("cannot add `{}` to #{number}: {e}", label.name))
        })
    }

    // The worker an issue with no `worker:` label runs on, as a prompt or a
    // comment names it.
    pub(super) fn default_worker(&self) -> String {
        let named = "the project's default worker";
        match self.labelled_worker(&[]) {
            Ok(w) => format!("{named}, `{}` at {} effort", w.model, w.effort.as_str()),
            Err(_) => named.to_owned(),
        }
    }
}
