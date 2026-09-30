//! Which project's ruling a reply on the topic answers

use super::super::Runner;
use super::super::trigger::number;
use crate::state::ids::RulingIds;

/// A reply's text without its code, read as the project it names, if it
/// names one, the ruling's id and the answer's words
pub(super) fn read_reply(text: &str) -> Option<(Option<&str>, u64, &str)> {
    let (first, rest) = text.trim().split_once(char::is_whitespace)?;
    let (named, id, words) = match number(first) {
        Some(id) => (None, id, rest),
        None => {
            let (id, words) = rest.trim_start().split_once(char::is_whitespace)?;
            (Some(first), number(id)?, words)
        }
    };
    let words = words.trim();
    (!words.is_empty()).then_some((named, id, words))
}

/// Whose ruling a reply's id names
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Whose {
    /// This project's
    Ours,
    /// Another project's, or none open here: the reply is left alone
    Theirs,
    /// Open here, and maybe another project's too, for this reason
    Unsure(String),
}

impl Runner {
    // Whose ruling `id` is, for a reply naming `named` or no project. An id
    // from before claims is this project's when named and within its ids.
    // Unnamed, it is when open here, unsure when another project may hold it.
    pub(super) fn whose(&self, named: Option<&str>, id: u64) -> Whose {
        let project = self.project.as_str();
        if named.is_some_and(|named| named != project) {
            return Whose::Theirs;
        }
        let ids = RulingIds::under(&self.paths.kelpie_home);
        if let Some(owner) = ids.owner(id) {
            return if owner == project {
                Whose::Ours
            } else {
                Whose::Theirs
            };
        }
        let open_here = self.state.rulings.iter().any(|r| r.id == id);
        match named {
            Some(_) if id <= self.state.last_ruling => Whose::Ours,
            Some(_) => Whose::Theirs,
            None if !open_here => Whose::Theirs,
            None => {
                for (other, state) in ids.states() {
                    if other == project {
                        continue;
                    }
                    match state {
                        Ok(state) if state.rulings.iter().any(|r| r.id == id) => {
                            return Whose::Unsure(format!(
                                "ruling {id} is also waiting on {other}"
                            ));
                        }
                        Ok(_) => {}
                        Err(_) => {
                            return Whose::Unsure(format!("{other}'s rulings cannot be read"));
                        }
                    }
                }
                Whose::Ours
            }
        }
    }
}
