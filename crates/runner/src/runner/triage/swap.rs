//! The waiting label's one-time swap
//!
//! Kelpie's waiting label moved from `needs-triage` to `needs-approval`, and
//! `needs-triage` now asks for a triage. Open issues on the old label would read
//! as approvals, so each project renames the label once at its start. The repo's
//! labels are the marker: no state records that a swap ran.

use std::collections::BTreeSet;

use super::super::Runner;
use crate::board::{TRIAGE, TRIAGE_GO};
use crate::ports::NewLabel;
use crate::settings::TriagePick;

// Made on a repo once its old label is renamed, when only the new one is there, and
// under `pick = "label"` on a repo with neither
const NEEDS_TRIAGE_GO: NewLabel<'static> = NewLabel {
    name: TRIAGE_GO,
    color: "fbca04",
    description: "Ask kelpie to triage this issue",
};

impl Runner {
    // Renames the repo's `needs-triage` to `needs-approval` where only the old one
    // exists, and makes `needs-triage` new, which `pick = "label"` also does on a repo
    // with neither, for the maintainer to use. A project on the local store has no
    // labels on its repo. A swap that fails is owed again at each poll, and until it
    // is done nothing makes `needs-approval`, which would read as a swap finished.
    pub(in crate::runner) fn swap_labels(&mut self) {
        if self.local_store() {
            return;
        }
        let swapped = self.swap_once();
        self.triage_run.label_swap_owed = swapped.is_err();
        if let Err(why) = swapped {
            self.notes.push(why);
        }
    }

    fn swap_once(&mut self) -> Result<(), String> {
        let slug = self.remote.as_str().to_owned();
        let have = (self.ports.forge.repo_labels(&self.remote))
            .map_err(|e| format!("cannot read {slug}'s labels to swap `{TRIAGE_GO}`: {e}"))?;
        let has = |name: &str| have.iter().any(|h| h.eq_ignore_ascii_case(name));
        match (has(TRIAGE), has(TRIAGE_GO)) {
            (true, true) => Ok(()),
            (false, false) if self.settings.triage.pick == TriagePick::Label => {
                self.make_missing_labels([NEEDS_TRIAGE_GO])
            }
            (false, false) => Ok(()),
            (true, false) => self.make_missing_labels([NEEDS_TRIAGE_GO]),
            (false, true) => {
                self.map_seen_labels().map_err(|e| {
                    format!("cannot save the swap of `{TRIAGE_GO}` to `{TRIAGE}`: {e}")
                })?;
                (self.writer().rename_label(TRIAGE_GO, TRIAGE)).map_err(|e| {
                    format!("cannot rename `{TRIAGE_GO}` to `{TRIAGE}` on {slug}: {e}")
                })?;
                self.make_missing_labels([NEEDS_TRIAGE_GO])
            }
        }
    }

    // Gives each record's `seen` the new name for the old one, so `stood` does not
    // read the renamed label as one put on by hand. Mapping again changes nothing.
    fn map_seen_labels(&mut self) -> Result<(), crate::state::StateError> {
        let map = |seen: &mut Vec<String>| {
            for label in seen.iter_mut().filter(|l| *l == TRIAGE_GO) {
                TRIAGE.clone_into(label);
            }
            let mut first = BTreeSet::new();
            seen.retain(|label| first.insert(label.clone()));
        };
        for tries in self.triage_tries.values_mut() {
            if let Some(seen) = tries.seen.as_mut() {
                map(seen);
            }
        }
        let mut next = self.state.clone();
        let records = next.triage.iter_mut().flat_map(|t| t.issues.iter_mut());
        for record in records {
            map(&mut record.seen);
        }
        match next == self.state {
            true => Ok(()),
            false => self.save(next),
        }
    }
}
