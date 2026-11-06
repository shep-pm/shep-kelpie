//! A repo's GitHub notifications, which belong to a user and not to the App
//!
//! `shep kelpie notifications clear` reads and writes them as the
//! maintainer's login. They are a port of their own so that [`Forge`] and its
//! wrappers stay as they are.

#[cfg(doc)]
use super::Forge;
use super::ForgeError;
use crate::settings::ForgeSlug;

/// One notification in the login's inbox
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The thread's id, which GitHub writes as a JSON string of digits
    pub id: String,
}

/// The login's notifications for one repo
pub trait Notifications: Send + Sync {
    /// Every notification the login holds for `repo`, read ones included
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn list(&self, repo: &ForgeSlug) -> Result<Vec<Notification>, ForgeError>;

    /// Marks `notification` done, which takes it out of the inbox
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or refuses.
    fn mark_done(&self, notification: &Notification) -> Result<(), ForgeError>;
}
