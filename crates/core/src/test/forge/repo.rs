//! The fake forge's repo-wide answers: its visibility, whether the account may
//! push, its default branch and labels, and who the account is

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use super::FakeForge;
use crate::ports::{FolderEntry, ForgeError, Timestamp, Visibility};
use crate::settings::ForgeSlug;

impl FakeForge {
    /// Sets the repository's visibility
    pub fn set_visibility(&self, visibility: Visibility) {
        *self.visibility.lock().unwrap() = visibility;
    }

    /// Makes `repo` alone `visibility`, whatever the rest are
    pub fn set_visibility_of(&self, repo: &ForgeSlug, visibility: Visibility) {
        let key = repo.as_str().to_lowercase();
        self.visibilities.lock().unwrap().insert(key, visibility);
    }

    /// Whether the account may push to the repo, which it may until a test says not
    pub fn set_can_push(&self, pushes: bool) {
        self.pushes.store(pushes, Ordering::SeqCst);
    }

    /// When a review bot last reviewed on the repo, or `None` when it never has; a fixed
    /// time until a test says
    pub fn set_last_bot_review(&self, at: Option<Timestamp>) {
        *self.bot_reviewed.lock().unwrap() = at;
    }

    /// How many times a review bot's last review was asked for
    pub fn bot_review_reads(&self) -> usize {
        self.bot_review_reads.load(Ordering::SeqCst)
    }

    /// Makes asking who the account is fail with `error`, as `gh` does when it is logged out
    pub fn set_viewer_down(&self, error: ForgeError) {
        *self.viewer_down.lock().unwrap() = Some(error);
    }

    /// Makes reading the account's token (`gh auth token`) fail with `error`
    pub fn set_login_token_down(&self, error: ForgeError) {
        *self.login_token_down.lock().unwrap() = Some(error);
    }

    /// Lets asking who the account is succeed again, after [`Self::set_viewer_down`]
    pub fn set_viewer_up(&self) {
        *self.viewer_down.lock().unwrap() = None;
    }

    /// Sets the repository's default branch
    pub fn set_default_branch(&self, branch: &str) {
        *self.default_branch.lock().unwrap() = branch.to_owned();
    }

    /// Gives the repo these branches besides its default branch
    pub fn set_branches(&self, branches: &[&str]) {
        *self.branches.lock().unwrap() = branches.iter().map(|&b| b.to_owned()).collect();
    }

    /// The repo's labels, those it started with and those made since
    pub fn repo_labels_now(&self) -> Vec<String> {
        self.repo_labels.lock().unwrap().clone()
    }

    /// Sets the labels the repository holds
    pub fn set_repo_labels(&self, labels: &[&str]) {
        *self.repo_labels.lock().unwrap() = labels.iter().map(|&l| l.to_owned()).collect();
    }

    /// Puts file `path` holding `text` on the default branch of `repo`, which
    /// then exists on the forge
    pub fn put_file(&self, repo: &str, path: &str, text: &str) {
        let mut files = self.files.lock().unwrap();
        let repo = files.entry(repo.to_lowercase()).or_default();
        repo.insert(path.to_owned(), text.to_owned());
    }

    /// What folder `path` of `repo` holds, as the forge lists it: nothing where no file is in it
    pub fn folder_of(&self, repo: &ForgeSlug, path: &str) -> Option<Vec<FolderEntry>> {
        let files = self.files.lock().unwrap();
        let repo = files.get(&repo.as_str().to_lowercase())?;
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut entries: BTreeMap<String, bool> = BTreeMap::new();
        for file in repo.keys().filter_map(|f| f.strip_prefix(&prefix)) {
            match file.split_once('/') {
                Some((dir, _)) => entries.insert(dir.to_owned(), true),
                None => entries.insert(file.to_owned(), false),
            };
        }
        let entries: Vec<FolderEntry> = (entries.into_iter())
            .map(|(name, is_dir)| FolderEntry { name, is_dir })
            .collect();
        (!entries.is_empty()).then_some(entries)
    }
}
