//! The fake forge's repo labels, and the strict mode that refuses an issue
//! a label the repo lacks, as GitHub does

use std::sync::atomic::Ordering;

use super::FakeForge;
use crate::ports::ForgeError;

impl FakeForge {
    /// The repo's labels, those it started with and those made since
    pub(crate) fn repo_labels_now(&self) -> Vec<String> {
        self.repo_labels.lock().unwrap().clone()
    }

    pub(crate) fn set_repo_labels(&self, labels: &[&str]) {
        *self.repo_labels.lock().unwrap() = labels.iter().map(|&l| l.to_owned()).collect();
    }

    /// Makes adding a label the repo lacks to an issue, or opening an issue
    /// with one, fail, or work again. Off until a test turns it on.
    pub(crate) fn set_strict_labels(&self, strict: bool) {
        self.strict_labels.store(strict, Ordering::SeqCst);
    }

    /// Makes making a label fail, or work again
    pub(crate) fn set_label_creates_down(&self, down: bool) {
        self.label_creates_down.store(down, Ordering::SeqCst);
    }

    /// Every label kelpie asked to make, oldest first, made or refused
    pub(crate) fn label_creates(&self) -> Vec<String> {
        self.label_creates.lock().unwrap().clone()
    }

    /// How many times kelpie read the repo's labels
    pub(crate) fn label_reads(&self) -> usize {
        self.label_reads.load(Ordering::SeqCst)
    }

    pub(super) fn make_label(&self, name: &str) -> Result<(), ForgeError> {
        self.label_creates.lock().unwrap().push(name.to_owned());
        if self.label_creates_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed(
                "HTTP 403: Resource not accessible by integration".into(),
            ));
        }
        let mut labels = self.repo_labels.lock().unwrap();
        if labels.iter().any(|l| l == name) {
            return Err(ForgeError::Failed(format!(
                "label with name \"{name}\" already exists"
            )));
        }
        labels.push(name.to_owned());
        Ok(())
    }

    // In strict mode, refuses the first of `labels` the repo lacks, in
    // `gh issue create` and `gh issue edit`'s words.
    pub(super) fn refuse_missing(&self, labels: &[&str]) -> Result<(), ForgeError> {
        if !self.strict_labels.load(Ordering::SeqCst) {
            return Ok(());
        }
        let have = self.repo_labels_now();
        match labels.iter().find(|&&l| !have.iter().any(|h| h == l)) {
            Some(missing) => Err(ForgeError::Failed(format!(
                "could not add label: '{missing}' not found"
            ))),
            None => Ok(()),
        }
    }
}
