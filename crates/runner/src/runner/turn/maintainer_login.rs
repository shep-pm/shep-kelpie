//! The maintainer's gh login, handed to a Windows worker as `GH_TOKEN`
//!
//! A Unix worker with no App token reads the maintainer's `gh` config folder.
//! On Windows the token is in Credential Manager, which the worker's sandbox
//! user cannot read, so kelpie reads it with `gh auth token` for each call.

use super::*;

impl Runner {
    // The token the worker holds as `GH_TOKEN` in place of an App token, none
    // where the App covers the repo or the worker reads `gh`'s folder instead.
    pub(super) fn maintainers_login(
        &self,
        app_token: Option<&InstallationToken>,
    ) -> Result<Option<InstallationToken>, String> {
        if app_token.is_some() || !self.login_as_variable {
            return Ok(None);
        }
        (self.ports.forge.login_token()).map(Some).map_err(|e| {
            format!(
                "cannot give the worker your gh login: {e}: run `gh auth login`, or set up \
                     kelpie's App with `shep kelpie github setup`"
            )
        })
    }

    /// Hands the worker the maintainer's login as `GH_TOKEN` when `on`, as a
    /// Windows worker with no App token is, whatever the platform
    #[cfg(any(test, feature = "test-support"))]
    pub fn hand_login_as_variable(&mut self, on: bool) {
        self.login_as_variable = on;
    }
}
