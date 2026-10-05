//! A review bot's keys: the bot, its window and how it is summoned

use super::Reviewer;
use crate::review_bot::{Bot, BotReviewer, ReviewWindow};

impl Reviewer {
    // A review bot is summoned on the pull request and answers there, within
    // its account's window: it runs no model of kelpie's and takes no lease
    // of the machine's.
    pub(super) fn bot(&self, prompted: bool) -> Result<BotReviewer, String> {
        self.on_its_own("bot", prompted)?;
        let others = self.model.is_some()
            || self.effort.is_some()
            || self.lease.is_some()
            || self.url.is_some()
            || self.context.is_some()
            || self.command.is_some()
            || self.ollama.is_some()
            || self.ollama_model.is_some();
        if others {
            return Err(
                "runs a review bot, which takes no `model`, `effort`, `lease`, \
                        `url`, `context`, `command`, `ollama` or `ollama_model`"
                    .into(),
            );
        }
        let (Some(bot), Some(reviews), Some(hours)) = (self.bot, self.reviews, self.hours) else {
            return Err(
                "runs a review bot, which needs which one as `bot` (`coderabbit`, \
                        `cubic` or `codex`), and its window as `reviews` in `hours`"
                    .into(),
            );
        };
        let reviews_on_ready = match (bot, self.reviews_on_ready) {
            (Bot::Codex, on) => on.unwrap_or(false),
            (_, None) => false,
            (_, Some(_)) => {
                return Err(format!(
                    "runs {bot}, which reviews only when summoned, so it takes no \
                     `reviews_on_ready`: only Codex does"
                ));
            }
        };
        Ok(BotReviewer {
            bot,
            window: ReviewWindow { reviews, hours },
            reviews_on_ready,
            rounds: self.rounds,
        })
    }

    // The keys only a review bot takes, refused naming the first set.
    pub(super) fn no_bot_keys(&self, harness: &str, yaml: &str) -> Result<(), String> {
        let set = [
            ("bot", self.bot.is_some()),
            ("reviews", self.reviews.is_some()),
            ("hours", self.hours.is_some()),
            ("reviews_on_ready", self.reviews_on_ready.is_some()),
            ("rounds", self.rounds.is_some()),
        ];
        match set.iter().find(|(_, set)| *set) {
            Some((key, _)) => Err(at_key(
                yaml,
                key,
                &format!(
                    "runs on {harness}, which takes no `bot`, `reviews`, `hours`, \
                     `reviews_on_ready` or `rounds`: those are a review bot's, on \
                     `harness: bot`"
                ),
            )),
            None => Ok(()),
        }
    }
}

// `message` led by `key` and followed by the line `yaml` sets it on, as the
// parser's own messages name a key and its line.
pub(super) fn at_key(yaml: &str, key: &str, message: &str) -> String {
    let sets = |line: &str| {
        line.strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    };
    match yaml.lines().position(sets) {
        Some(at) => format!("`{key}`: {message} at line {}", at + 1),
        None => message.to_owned(),
    }
}
