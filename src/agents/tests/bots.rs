//! Review bots' agent files: one per bot, named for it, holding its window

use super::{COMMAND, REVIEWER, folder, name, refused};
use crate::agents::{Agents, Role, Runs};
use crate::review_bot::{Bot, BotReviewer, ReviewWindow};

const CUBIC: &str = "---\nrole: reviewer\nharness: bot\nbot: cubic\nreviews: 5\nhours: 24\n\
                     paths: [\"docs/**\"]\nrounds: 2\n---\n";

#[test]
fn kelpies_own_review_bots_are_written_and_hold_each_bots_window() {
    let agents = Agents::embedded();
    let window = |reviews, hours| ReviewWindow {
        reviews: std::num::NonZeroU32::new(reviews).unwrap(),
        hours: std::num::NonZeroU32::new(hours).unwrap(),
    };
    for (bot, reviews, hours) in [
        (Bot::Coderabbit, 1, 1),
        (Bot::Cubic, 20, 720),
        (Bot::Codex, 10, 168),
    ] {
        let agent = agents.get(&name(bot.as_str())).unwrap();
        assert_eq!(agent.role, Role::Reviewer);
        assert_eq!(
            agent.runs,
            Runs::Bot(BotReviewer {
                bot,
                window: window(reviews, hours),
                reviews_on_ready: false,
                rounds: None,
            })
        );
        assert_eq!((agent.runs.session(), agent.runs.local()), (None, None));
        assert_eq!(agent.prompt, None, "a bot writes its own prompt");
    }
}

#[test]
fn a_bot_file_takes_its_paths_and_rounds_and_codex_its_reviews_on_ready() {
    let codex = "---\nrole: reviewer\nharness: bot\nbot: codex\nreviews: 10\nhours: 168\n\
                 reviews_on_ready: true\n---\n";
    let dir = folder(&[("cubic.md", CUBIC), ("codex.md", codex)]);
    let agents = Agents::load(dir.path()).unwrap();
    let cubic = agents.get(&name("cubic")).unwrap();
    let defined = cubic.runs.bot().unwrap();
    assert_eq!(defined.bot, Bot::Cubic);
    assert_eq!(defined.window.seconds(), 24 * 3600);
    assert_eq!(defined.rounds.map(std::num::NonZeroU32::get), Some(2));
    assert_eq!(cubic.paths[0].as_str(), "docs/**");
    let codex = agents.get(&name("codex")).unwrap().runs.bot().unwrap();
    assert!(codex.reviews_on_ready);
}

#[test]
fn a_bot_file_is_named_for_its_bot() {
    let err = refused(&[("rabbit.md", &CUBIC.replace("bot: cubic", "bot: coderabbit"))]);
    assert!(
        err.ends_with(
            "rabbit.md: `bot`: runs coderabbit, so it must be named `coderabbit.md`: each \
             bot has one file, which holds its account's window at line 4"
        ),
        "{err}"
    );
}

#[test]
fn a_bot_files_error_names_the_bot_key_and_its_line() {
    let err = refused(&[(
        "cubic.md",
        &CUBIC.replace("hours: 24\n", "hours: 24\nreviews_on_ready: true\n"),
    )]);
    assert!(
        err.ends_with(
            "cubic.md: `bot`: runs cubic, which reviews only when summoned, so it takes no \
             `reviews_on_ready`: only Codex does at line 4"
        ),
        "{err}"
    );
    let session = REVIEWER.replace("effort: high\n", "effort: high\nrounds: 1\n");
    let err = refused(&[("mine.md", &session)]);
    assert!(
        err.contains("mine.md: `rounds`: runs on claude-code"),
        "{err}"
    );
    assert!(err.ends_with("at line 6"), "{err}");
}

#[test]
fn a_bot_whose_keys_its_harness_cannot_use_is_refused_naming_why() {
    let cases = [
        (
            format!("{CUBIC}Review it.\n"),
            "runs on bot, which writes its own prompt: leave the body empty",
        ),
        (
            CUBIC.replace("hours: 24\n", "hours: 24\nmodel: m\n"),
            "runs a review bot, which takes no `model`, `effort`, `lease`",
        ),
        (
            CUBIC.replace("hours: 24\n", "hours: 24\nlease: gpu\n"),
            "runs a review bot, which takes no `model`, `effort`, `lease`",
        ),
        (
            CUBIC.replace("hours: 24\n", ""),
            "runs a review bot, which needs which one as `bot`",
        ),
        (
            CUBIC.replace("reviews: 5\n", "reviews: 0\n"),
            "expected a nonzero u32 at line 5",
        ),
        (
            CUBIC.replace("hours: 24\n", "hours: 24\nreviews_on_ready: true\n"),
            "runs cubic, which reviews only when summoned, so it takes no `reviews_on_ready`",
        ),
        (
            CUBIC.replace("hours: 24\n", "hours: 24\nsecond_look: true\n"),
            "runs on bot, which cannot be shown its own findings",
        ),
        (
            CUBIC.replace("bot: cubic", "bot: sourcery"),
            "`bot`: unknown variant `sourcery`",
        ),
        (
            REVIEWER.replace("effort: high\n", "effort: high\nrounds: 1\n"),
            "runs on claude-code, which takes no `bot`, `reviews`, `hours`",
        ),
        (
            COMMAND.replace(
                "command: ~/bin/review\n",
                "command: ~/bin/review\nbot: cubic\n",
            ),
            "runs on command, which takes no `bot`, `reviews`, `hours`",
        ),
    ];
    for (text, why) in cases {
        let err = refused(&[("cubic.md", &text)]);
        assert!(err.contains("cubic.md: "), "{err}");
        assert!(err.contains(why), "{err}\nwanted: {why}");
    }
}
