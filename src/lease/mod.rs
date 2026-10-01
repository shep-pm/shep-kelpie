//! Leases on the shared resources every project uses
//!
//! Three kinds of lease. The GPU lease is the lock the maintainer's qwen
//! scripts already take, which [`gpu`] reads and takes in their format,
//! and the dog stays out of it. A [`counted`] lease, `cargo-test`, is held
//! by a few commands at once, each asking at the dog's [`door`]. Every
//! other lease lives in the dog's [`book`], asked for and granted through
//! shep as [`wire`] lays out.

pub mod book;
pub mod cli;
pub mod counted;
pub mod door;
pub mod gpu;
pub mod saved;
pub mod window;
pub mod wire;

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::runner::ProjectName;

/// The GPU lease's name on the command line
pub const GPU: &str = "gpu";

/// The CodeRabbit lease's name: one review window for the whole account
pub const CODERABBIT: &str = "coderabbit";

/// What a book lease is for, as a runner and the dog name it
// wire format: changing this is a breaking change to runner metrics
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LeaseKind(String);

impl LeaseKind {
    /// The CodeRabbit window's lease
    pub fn coderabbit() -> Self {
        Self(CODERABBIT.to_owned())
    }

    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for LeaseKind {
    type Error = LeaseKindError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if value == GPU {
            return Err(LeaseKindError::Gpu);
        }
        if value == counted::CARGO_TEST {
            return Err(LeaseKindError::Counted);
        }
        if value.is_empty() || !value.chars().all(allowed) {
            return Err(LeaseKindError::Name(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

// A kind read back from the book file passes the same check as one asked for.
impl<'de> Deserialize<'de> for LeaseKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::try_from(name.as_str()).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for LeaseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a name is not a book lease's kind
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseKindError {
    /// Not lowercase letters, digits and `-`, carrying the name
    Name(String),
    /// `gpu`, which is the qwen scripts' lock and never the dog's
    Gpu,
    /// `cargo-test`, which is held for one command at the dog's door
    Counted,
}

impl fmt::Display for LeaseKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => write!(
                f,
                "{name:?} is not a lease kind: use lowercase letters, digits and -"
            ),
            Self::Gpu => f.write_str("the GPU lease is the qwen scripts' lock, not the dog's"),
            Self::Counted => f.write_str(
                "cargo-test is held for one command: run `shep kelpie lease run cargo-test -- <command>`",
            ),
        }
    }
}

impl core::error::Error for LeaseKindError {}

/// Which run of a runner is asking: its process id
///
/// A restart is a new process, so a new epoch, and the dog reclaims what
/// the old run held. shep's process events carry the pid too, so the dog
/// tells runs apart without relying on event order.
// wire format: changing this is a breaking change to runner metrics
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(pub u64);

/// Who holds or waits for a book lease
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// The maintainer, through `shep kelpie lease`
    Maintainer,
    /// One run of a project's runner
    Runner {
        /// The project, which is also its runner's sheep name
        project: ProjectName,
        /// Which run
        epoch: Epoch,
    },
}

impl Holder {
    /// Whether this is some run of `project`'s runner
    pub fn is_runner_of(&self, project: &ProjectName) -> bool {
        matches!(self, Self::Runner { project: p, .. } if p == project)
    }
}

// Status names a runner by its project alone: the epoch is bookkeeping.
impl Serialize for Holder {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "lowercase")]
        enum View<'a> {
            Maintainer,
            Runner(&'a str),
        }
        match self {
            Self::Maintainer => View::Maintainer,
            Self::Runner { project, .. } => View::Runner(project.as_str()),
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_is_lowercase_letters_digits_and_dashes() {
        for bad in ["", "Stand-In", "a.b", "a b", "a_b"] {
            assert_eq!(
                LeaseKind::try_from(bad),
                Err(LeaseKindError::Name(bad.into())),
                "{bad:?}"
            );
        }
        assert_eq!(
            LeaseKind::try_from("stand-in-2").unwrap().as_str(),
            "stand-in-2"
        );
    }

    #[test]
    fn gpu_is_never_a_book_lease() {
        assert_eq!(LeaseKind::try_from("gpu"), Err(LeaseKindError::Gpu));
    }

    #[test]
    fn cargo_test_is_never_a_book_lease() {
        assert_eq!(
            LeaseKind::try_from("cargo-test"),
            Err(LeaseKindError::Counted)
        );
    }

    #[test]
    fn a_holder_reads_as_the_maintainer_or_its_project() {
        let runner = Holder::Runner {
            project: ProjectName::try_from("koji").unwrap(),
            epoch: Epoch(7),
        };
        assert_eq!(
            serde_json::to_value([Holder::Maintainer, runner]).unwrap(),
            serde_json::json!(["maintainer", { "runner": "koji" }])
        );
    }
}
