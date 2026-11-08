//! The registry stand-in: which packages are published, and what was asked
//!
//! It answers in memory, so a test of the triage poll never reaches a registry.
//! No package is published until a test says so.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::board::check::Needed;
use crate::ports::{Registry, RegistryError};

type Watch = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Book {
    /// The packages published, as the `Needs:` line writes each
    published: BTreeSet<String>,
    /// What every read fails with, while it is set
    failure: Option<RegistryError>,
    /// Each package asked of, as the `Needs:` line writes it, in order
    asked: Vec<String>,
    /// Run on every read, before it answers
    watch: Option<Watch>,
}

/// A registry that holds the packages a test publishes
#[derive(Clone, Default)]
pub struct FakeRegistry {
    book: Arc<Mutex<Book>>,
}

impl fmt::Debug for FakeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeRegistry").finish_non_exhaustive()
    }
}

impl FakeRegistry {
    /// One with nothing published
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `package`, written as the `Needs:` line writes it
    pub fn publish(&self, package: &str) {
        self.book
            .lock()
            .unwrap()
            .published
            .insert(package.to_owned());
    }

    /// Every read fails with `error`, until [`Self::recovers`]
    pub fn fails(&self, error: RegistryError) {
        self.book.lock().unwrap().failure = Some(error);
    }

    /// Reads answer again
    pub fn recovers(&self) {
        self.book.lock().unwrap().failure = None;
    }

    /// Runs `watch` on every read, before it answers and with no lock of the
    /// book held, so it can look at what the reader holds
    pub fn watch(&self, watch: impl Fn() + Send + Sync + 'static) {
        self.book.lock().unwrap().watch = Some(Arc::new(watch));
    }

    /// Each package asked of, in order, as the `Needs:` line writes it
    pub fn asked(&self) -> Vec<String> {
        self.book.lock().unwrap().asked.clone()
    }
}

impl Registry for FakeRegistry {
    fn published(&self, needed: &Needed) -> Result<bool, RegistryError> {
        let watch = {
            let mut book = self.book.lock().unwrap();
            book.asked.push(needed.to_string());
            book.watch.clone()
        };
        if let Some(watch) = watch {
            watch();
        }
        let book = self.book.lock().unwrap();
        if let Some(error) = &book.failure {
            return Err(error.clone());
        }
        Ok(book.published.contains(&needed.to_string()))
    }
}
