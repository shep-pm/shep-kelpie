//! Ruling ids, unique across every project under one kelpie home
//!
//! Each id given is a file `<kelpie home>/rulings/<id>` holding its project's
//! name, made with a link that fails when the name exists, so two runners
//! never give the same id. A new id goes past the highest claimed and past
//! every project's own `last_ruling`, so no id a state file holds is given
//! again. The files stay, so an id always names its project.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::{ProjectState, Ruling, StateError, StateStore};
use crate::totp::private_dir;

/// The ids given under one kelpie home, and the projects' state files beside them
#[derive(Debug, Clone)]
pub struct RulingIds {
    claims: PathBuf,
    projects: PathBuf,
}

impl RulingIds {
    /// The ids under `kelpie_home`
    pub fn under(kelpie_home: &Path) -> Self {
        Self {
            claims: kelpie_home.join("rulings"),
            projects: kelpie_home.join("projects"),
        }
    }

    /// A new id for a ruling of `project`, whose own highest is `floor`
    ///
    /// When no claim can be written the id is still past every one seen,
    /// only not held against a runner claiming at the same moment.
    pub fn claim(&self, project: &str, floor: u64) -> u64 {
        let mut id = self.highest().max(floor) + 1;
        if private_dir(&self.claims).is_err() {
            return id;
        }
        loop {
            match self.claim_one(project, id) {
                Ok(true) => return id,
                Ok(false) => id += 1,
                Err(_) => return id,
            }
        }
    }

    // Whether `id` is now `project`'s: false when another runner holds it.
    // The claim is written beside its name, synced, then linked into place,
    // so a reader never sees half a name.
    fn claim_one(&self, project: &str, id: u64) -> io::Result<bool> {
        let name = self.claims.join(id.to_string());
        let fresh = self
            .claims
            .join(format!(".{id}.{project}.{}", std::process::id()));
        let _ = fs::remove_file(&fresh);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&fresh)?;
        file.write_all(project.as_bytes())?;
        file.sync_all()?;
        let linked = fs::hard_link(&fresh, &name);
        let _ = fs::remove_file(&fresh);
        match linked {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The project ruling `id` was given to, when it was claimed
    pub fn owner(&self, id: u64) -> Option<String> {
        let name = fs::read_to_string(self.claims.join(id.to_string())).ok()?;
        (!name.is_empty()).then_some(name)
    }

    /// Every project with a state file, by name, and its state as read
    pub fn states(&self) -> Vec<(String, Result<ProjectState, StateError>)> {
        let Ok(entries) = fs::read_dir(&self.projects) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|name| {
                let store = StateStore::new(self.projects.join(&name).join("state.json"));
                let state = store.load().transpose()?;
                Some((name, state))
            })
            .collect()
    }

    /// Every open ruling in a state file that reads, with its project
    pub fn open(&self) -> Vec<(String, Ruling)> {
        let states = self.states().into_iter();
        let readable = states.filter_map(|(name, state)| Some((name, state.ok()?)));
        readable
            .flat_map(|(name, state)| state.rulings.into_iter().map(move |r| (name.clone(), r)))
            .collect()
    }

    // The highest id claimed, or any project's state file has used.
    fn highest(&self) -> u64 {
        let claimed = fs::read_dir(&self.claims)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u64>().ok());
        let used = self.states().into_iter().filter_map(|(_, state)| {
            let state = state.ok()?;
            let open = state.rulings.iter().map(|r| r.id).max().unwrap_or(0);
            Some(state.last_ruling.max(open))
        });
        claimed.chain(used).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Timestamp;
    use crate::state::RulingKind;

    // A project's state file holding open rulings `open`, with `last` as its last id.
    fn project(home: &Path, name: &str, last: u64, open: &[u64]) {
        let mut state = ProjectState::new(Timestamp(0));
        state.last_ruling = last;
        state.rulings = open
            .iter()
            .map(|&id| Ruling {
                id,
                issue: Some(7),
                question: format!("Ruling {id}?"),
                pull_request: None,
                kind: RulingKind::Closed,
                alerted: false,
                relayed: false,
                resend: false,
            })
            .collect();
        let path = home.join("projects").join(name).join("state.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        StateStore::new(path).save(&state).unwrap();
    }

    #[test]
    fn ids_are_unique_across_two_projects() {
        let home = tempfile::tempdir().unwrap();
        let ids = RulingIds::under(home.path());
        let given = [
            ids.claim("koji", 0),
            ids.claim("rotom", 0),
            ids.claim("koji", 1),
            ids.claim("rotom", 2),
        ];
        assert_eq!(given, [1, 2, 3, 4]);
        assert_eq!(ids.owner(2).as_deref(), Some("rotom"));
        assert_eq!(ids.owner(3).as_deref(), Some("koji"));
        assert_eq!(ids.owner(5), None);
    }

    #[test]
    fn new_ids_go_past_the_highest_any_project_used_before_claims() {
        let home = tempfile::tempdir().unwrap();
        project(home.path(), "lab", 14, &[14]);
        project(home.path(), "koji", 3, &[2, 3]);
        let ids = RulingIds::under(home.path());
        assert_eq!(ids.claim("koji", 3), 15);
        assert_eq!(ids.claim("lab", 14), 16);
        // An open ruling keeps its number.
        let open: Vec<(String, u64)> = ids.open().into_iter().map(|(p, r)| (p, r.id)).collect();
        assert_eq!(
            open,
            [("koji".into(), 2), ("koji".into(), 3), ("lab".into(), 14)]
        );
    }

    #[test]
    fn a_claim_taken_elsewhere_moves_on_to_the_next_id() {
        let home = tempfile::tempdir().unwrap();
        let ids = RulingIds::under(home.path());
        assert_eq!(ids.claim("koji", 0), 1);
        // Another runner that read before the first claim tries 1 as well.
        assert!(!ids.claim_one("rotom", 1).unwrap());
        assert_eq!(ids.owner(1).as_deref(), Some("koji"));
    }

    #[test]
    fn a_home_that_cannot_hold_claims_still_counts_past_every_project() {
        let home = tempfile::tempdir().unwrap();
        project(home.path(), "lab", 9, &[]);
        fs::write(home.path().join("rulings"), "not a folder").unwrap();
        let ids = RulingIds::under(home.path());
        assert_eq!(ids.claim("koji", 2), 10);
    }
}
