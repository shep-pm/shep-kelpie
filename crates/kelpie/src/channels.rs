//! Which ways a ruling reaches the maintainer
//!
//! A project's settings and kelpie's own settings can each name the
//! channels, and the project's wins. With neither naming any, rulings go to
//! every channel, as they did before the choice existed.

use std::collections::BTreeSet;

use serde::Deserialize;

/// One way to reach the maintainer
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// A post to the webhook in kelpie's settings
    Webhook,
    /// A message to the relay session, which answers through the Claude app
    Relay,
}

/// The channels rulings go to: at least one
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<Channel>")]
pub struct Channels(BTreeSet<Channel>);

impl Channels {
    /// Whether rulings go through `channel`
    #[inline]
    pub fn has(&self, channel: Channel) -> bool {
        self.0.contains(&channel)
    }
}

impl Default for Channels {
    fn default() -> Self {
        Self([Channel::Webhook, Channel::Relay].into())
    }
}

impl TryFrom<Vec<Channel>> for Channels {
    type Error = &'static str;

    fn try_from(value: Vec<Channel>) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err("must name at least one of `webhook` and `relay`");
        }
        Ok(Self(value.into_iter().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Holder {
        channels: Channels,
    }

    fn parse(list: &str) -> Result<Channels, String> {
        toml::from_str::<Holder>(&format!("channels = {list}"))
            .map(|h| h.channels)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn each_channel_can_be_chosen_alone_or_together() {
        let only = |c: Channels| (c.has(Channel::Webhook), c.has(Channel::Relay));
        assert_eq!(only(parse(r#"["webhook"]"#).unwrap()), (true, false));
        assert_eq!(only(parse(r#"["relay"]"#).unwrap()), (false, true));
        assert_eq!(
            only(parse(r#"["relay", "webhook", "relay"]"#).unwrap()),
            (true, true)
        );
        assert_eq!(only(Channels::default()), (true, true));
    }

    #[test]
    fn no_channel_and_unknown_channels_are_refused() {
        assert!(parse("[]").unwrap_err().contains("at least one"));
        assert!(parse(r#"["pigeon"]"#).is_err());
    }
}
