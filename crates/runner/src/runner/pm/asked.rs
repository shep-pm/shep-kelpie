//! The rulings the maintainer puts to the project manager
//!
//! A ruling that waits on the maintainer wakes no project manager of itself.
//! A reply in its thread asks: `<id> pm recommend` for its pick, posted
//! there while the ruling stays the maintainer's, and `<id> pm options` for
//! other approaches, kept on the ruling. `<id> pm <n>` takes approach n, as
//! a `no <note>` or a question's answer. `<id> pm decide` hands the ruling
//! over as if `rulings.pm_decides` left it to the project manager. A wake
//! that fails or gives no answer says so in the thread.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::Wake;
use super::act::{WORDS, plain};
use super::maintainer::held_on;
use super::requests::Request;
use super::said::{Released, SaidCall, SaidHow};
use crate::pm_answer::{PmAnswer, WHY_LONGEST, from_fields};
use crate::ports::ThreadComment;
use crate::runner::voice::{answer_for, brief, inert, named};
use crate::runner::words::Wants;
use crate::runner::{Answer, Runner};
use crate::settings::RulingName;
use crate::state::{Ruling, RulingKind, Stuck};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// What a `pm` reply takes after `pm`
const TAKES: &str = "`pm` takes `recommend`, `options`, `decide` or an approach's number";

/// The most approaches a ruling keeps
const APPROACHES: usize = 5;

pub(in crate::runner) use crate::state::PmAsked as Asked;

/// A reply to a ruling that starts with `pm`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum PmReply {
    /// Asks the project manager
    Ask(Asked),
    /// Takes the approach of this number, 1 for the first
    Take(usize),
}

/// The `pm` reply `words` are to the ruling read by `number`, if they
/// start with `pm`, or with `number` and then `pm`
///
/// # Errors
///
/// What a `pm` reply takes, when the rest is none of them.
pub(in crate::runner) fn pm_reply(
    words: &str,
    number: TicketId,
) -> Option<Result<PmReply, String>> {
    let mut parts = words.split_whitespace().peekable();
    let numbered =
        |first: &&str| first.trim_start_matches('#').parse::<TicketId>().ok() == Some(number);
    if parts.peek().is_some_and(numbered) {
        parts.next();
    }
    if !parts.next()?.eq_ignore_ascii_case("pm") {
        return None;
    }
    let word = parts.next().map(str::to_ascii_lowercase);
    Some(match (word.as_deref(), parts.next()) {
        (Some("recommend"), None) => Ok(PmReply::Ask(Asked::Recommend)),
        (Some("options"), None) => Ok(PmReply::Ask(Asked::Options)),
        (Some("decide"), None) => Ok(PmReply::Ask(Asked::Decide)),
        (Some(n), None) => match n.parse::<usize>() {
            Ok(n) if n > 0 => Ok(PmReply::Take(n)),
            _ => Err(TAKES.to_owned()),
        },
        _ => Err(TAKES.to_owned()),
    })
}

/// What a wake for `asked` on ruling `id` tells the project manager,
/// before the ruling itself
pub(super) fn lead(asked: Asked, id: TicketId) -> String {
    match asked {
        Asked::Recommend => format!(
            "the maintainer asks which option you recommend for ruling {id}. Give it as your \
             vote on it in `votes`: kelpie posts it in the ruling's thread, and the maintainer \
             still answers the ruling:\n"
        ),
        Asked::Options => format!(
            "the maintainer finds none of ruling {id}'s options right and asks you for other \
             approaches. Give them as your vote on it in `votes`, `{{\"ruling\": {id}, \
             \"approaches\": [\"<what the worker should do instead, one line of at most 350 \
             characters>\"]}}`, at most {APPROACHES}: kelpie posts them in the ruling's thread, \
             and the one the maintainer takes goes to the worker as their note, or as their \
             answer to a question:\n"
        ),
        Asked::Decide => format!(
            "the maintainer hands ruling {id} to you to decide, as if the project left it to \
             you. Give your answer as your vote on it in `votes`:\n"
        ),
    }
}

impl Runner {
    /// Puts ruling `id` to the project manager for `asked`, as `comment`
    /// asks, reacting if it took and saying why not in its thread
    pub(in crate::runner) fn pm_asked_by(
        &mut self,
        comment: &ThreadComment,
        id: TicketId,
        asked: Asked,
    ) {
        let (who, thread) = (&comment.author, comment.thread);
        match self.pm_ask(id, asked) {
            Ok(()) => {
                if let Err(e) = self.ports.voice.thumbs_up(&self.remote, comment.id) {
                    self.notes
                        .push(format!("cannot react to {who}'s request on #{thread}: {e}"));
                }
            }
            Err(why) => {
                let number = self.ruling_number(id);
                let said =
                    format!("@{who} Ruling #{number} was not put to the project manager: {why}.");
                self.say(thread, &said);
            }
        }
    }

    // Queues the wake that asks the project manager, once at a time.
    fn pm_ask(&mut self, id: TicketId, asked: Asked) -> Result<(), String> {
        if self.agents.pm.is_none() {
            return Err("this project names no project manager".into());
        }
        let ruling = (self.state.rulings.iter().find(|r| r.id == id))
            .ok_or_else(|| "it no longer waits".to_owned())?;
        if self.pm_has_asked(id) {
            return Err("the project manager is already on it, and answers in this thread".into());
        }
        if asked == Asked::Decide {
            self.pm_may_decide(ruling)?;
        }
        let (text, request) = (brief(ruling).0, Request::of(asked, ruling));
        self.pm.asking.insert(id, self.ports.clock.now());
        self.mark_asked(id, Some(asked));
        self.pm_due(Wake::Answer {
            id,
            asked: text,
            request: Some(request),
        });
        Ok(())
    }

    /// Acts on the project manager's `votes` on the rulings in `requested`,
    /// which the maintainer asked about, and says what each did, for the log
    pub(super) fn pm_requested(
        &mut self,
        requested: &[(TicketId, Request)],
        votes: &[(TicketId, Map<String, Value>)],
    ) -> Vec<String> {
        let mut done = Vec::new();
        for &(id, request) in requested {
            self.pm.asking.remove(&id);
            let ruling = match self.ruling_asked(id, request) {
                Ok(ruling) => ruling,
                Err(why) => {
                    done.push(why);
                    continue;
                }
            };
            let fields = (votes.iter().rev()).find(|(vote, _)| *vote == id);
            let acted = match (fields, request.asked) {
                (Some((_, fields)), Asked::Recommend) => self.pm_recommend(&ruling, fields),
                (Some((_, fields)), Asked::Options) => self.pm_options(&ruling, fields),
                (Some((_, fields)), Asked::Decide) => self.pm_decide(&ruling, fields),
                (None, _) => Err("its reply held no answer to it".to_owned()),
            };
            match acted {
                Ok(line) => done.push(line),
                Err(why) => {
                    self.pm_did_not_answer(&ruling);
                    done.push(format!("gave no answer to ruling {id}: {why}"));
                }
            }
        }
        done
    }

    // Why the project manager may not decide `ruling`, where it may not: the
    // kinds the settings refuse it, and those `pm_answers_ruling` never gives it.
    pub(super) fn pm_may_decide(&self, ruling: &Ruling) -> Result<(), String> {
        never_decided(RulingName::of(&ruling.kind))?;
        if matches!(
            ruling.kind,
            RulingKind::Question { .. } | RulingKind::FollowUp { .. }
        ) {
            return Err("a question or follow-up wakes the project manager on its own wake".into());
        }
        if ruling.kind.is_contribution() {
            return Err("a contribution's ruling is never the project manager's".into());
        }
        if matches!(
            ruling.kind,
            RulingKind::Stuck(Stuck::TurnFailed { setup: true, .. })
        ) {
            return Err("kelpie could not set up the worker's turn, which only you can fix".into());
        }
        if self.draining() {
            return Err("the runner drains".into());
        }
        if brief(ruling).1 < 2 {
            return Err("it lists fewer than two options to pick from".into());
        }
        if self.pm_answered_before(ruling) {
            return Err("the project manager already answered this failed turn".into());
        }
        Ok(())
    }

    // Applies the project manager's answer to `ruling` as one the project
    // left to it would be, or posts its pick where the ruling stays the maintainer's.
    fn pm_decide(
        &mut self,
        ruling: &Ruling,
        fields: &Map<String, Value>,
    ) -> Result<String, String> {
        let id = ruling.id;
        // What may have changed since it was asked: a drain, an earlier answer.
        if let Err(why) = self.pm_may_decide(ruling) {
            let number = ruling.number();
            let said = format!(
                "The project manager's answer on ruling #{number} was not applied, since {why}. \
                 It is still yours."
            );
            self.pm_says(ruling, &said);
            return Ok(format!("left ruling {id} to the maintainer, since {why}"));
        }
        let answer = self.pm_vote(ruling, fields)?;
        let choice = answer.choice;
        match self.pm_decides(ruling, &answer) {
            Ok(()) => Ok(format!("answered ruling {id} with option {choice}")),
            Err(why) => {
                self.pm_recommends(id, &answer);
                self.post_recommended(ruling, &answer, Some(&why));
                Ok(format!(
                    "put ruling {id} back to the maintainer, recommending option {choice}, \
                     since {why}"
                ))
            }
        }
    }

    // Posts the project manager's pick for `ruling` in its thread.
    fn pm_recommend(
        &mut self,
        ruling: &Ruling,
        fields: &Map<String, Value>,
    ) -> Result<String, String> {
        let answer = self.pm_vote(ruling, fields)?;
        self.pm_recommends(ruling.id, &answer);
        self.post_recommended(ruling, &answer, None);
        let (choice, id) = (answer.choice, ruling.id);
        Ok(format!("recommended option {choice} on ruling {id}"))
    }

    // Keeps the approaches the project manager gave for `ruling` on it, and
    // posts them in its thread, inert, with how to take one.
    fn pm_options(
        &mut self,
        ruling: &Ruling,
        fields: &Map<String, Value>,
    ) -> Result<String, String> {
        let Some(Value::Array(items)) = fields.get("approaches") else {
            return Err("`approaches` is not a list".into());
        };
        let approaches: Vec<String> = (items.iter())
            .filter_map(Value::as_str)
            .map(|text| plain(&self.shown(text.to_owned()), WHY_LONGEST))
            .filter(|text| !text.is_empty())
            .take(APPROACHES)
            .collect();
        if approaches.is_empty() {
            return Err("it gave no approaches".into());
        }
        let id = ruling.id;
        let mut next = self.state.clone();
        if let Some(kept) = next.rulings.iter_mut().find(|r| r.id == id) {
            kept.approaches.clone_from(&approaches);
        }
        self.save(next)
            .map_err(|e| format!("cannot save its approaches: {e}"))?;
        let number = ruling.number();
        let listed: Vec<String> = (approaches.iter().zip(1..))
            .map(|(text, n)| format!("{n}. {}", inert(text)))
            .collect();
        let taken = match Wants::of(&ruling.kind) {
            Wants::Answer => "as your answer".to_owned(),
            Wants::Merge | Wants::YesOrNo => format!("as your `{number} no <note>`"),
        };
        let said = format!(
            "**Project manager: other approaches to ruling #{number}**\n\n{}\n\nReply \
             `{number} pm <n>` to take one {taken}.",
            listed.join("\n")
        );
        self.pm_says(ruling, &said);
        let count = approaches.len();
        Ok(format!("gave ruling {id} {count} other approaches"))
    }

    /// The answer approach `n` of ruling `id` gives: the maintainer's note,
    /// or a question's answer
    ///
    /// # Errors
    ///
    /// Why approach `n` is none, for the reply.
    pub(in crate::runner) fn approach(&self, id: TicketId, n: usize) -> Result<Answer, String> {
        let ruling = (self.state.rulings.iter().find(|r| r.id == id))
            .ok_or_else(|| "it no longer waits".to_owned())?;
        let number = ruling.number();
        if ruling.kind.is_contribution() {
            return Err(format!(
                "`{number} no` would only clear a contribution's ruling, so it takes no approach"
            ));
        }
        let Some(text) = n.checked_sub(1).and_then(|at| ruling.approaches.get(at)) else {
            return Err(match ruling.approaches.len() {
                0 => format!(
                    "the project manager gave it no approaches: reply `{number} pm options` for \
                     some"
                ),
                count => format!("it has {count} approaches, so `pm {n}` names none"),
            });
        };
        Ok(match Wants::of(&ruling.kind) {
            Wants::Answer => Answer::Text(text.clone()),
            Wants::Merge | Wants::YesOrNo => Answer::No(text.clone()),
        })
    }

    // The project manager's answer to `ruling` in `fields`, its why cut to
    // what a ruling carries.
    fn pm_vote(&self, ruling: &Ruling, fields: &Map<String, Value>) -> Result<PmAnswer, String> {
        let answer = from_fields(fields, brief(ruling).1)?;
        let why = plain(&self.shown(answer.why.clone()), WORDS);
        Ok(PmAnswer { why, ..answer })
    }

    // Posts the project manager's pick for `ruling` in its thread, where the
    // maintainer still answers it, saying why it is theirs where `since` does.
    fn post_recommended(&mut self, ruling: &Ruling, answer: &PmAnswer, since: Option<&str>) {
        let (number, key) = (ruling.number(), answer.choice);
        let mut blocks = vec![match answer.why.is_empty() {
            true => format!("**Project manager: {key}**"),
            false => format!("**Project manager: {key}**. {}", inert(&answer.why)),
        }];
        blocks.extend(reply_form(ruling, key).map(|form| format!("It would answer {form}.")));
        blocks.push(match since {
            Some(why) => format!(
                "Ruling #{number} is still yours, since {why}: answer it as its comment says."
            ),
            None => format!("Ruling #{number} is still yours: answer it as its comment says."),
        });
        self.pm_says(ruling, &blocks.join("\n\n"));
    }

    /// Says in `ruling`'s thread that the project manager did not answer
    /// what the maintainer asked of it
    pub(super) fn pm_did_not_answer(&mut self, ruling: &Ruling) {
        let number = ruling.number();
        let said = format!(
            "The project manager did not answer what you asked of ruling #{number}, which is \
             still yours."
        );
        self.pm_says(ruling, &said);
    }

    /// Acts on the project manager's `said` calls on the maintainer's words
    /// that `told` held for this wake, and says what each did, for the log
    ///
    /// A call on a thread no held line names is dropped. What it cannot carry
    /// out, and held words it made no call on, are named in the thread, where
    /// the ruling and the work item wait for the maintainer.
    pub(super) fn pm_said(
        &mut self,
        told: &[String],
        said: &[SaidCall],
    ) -> (Vec<String>, Vec<String>) {
        let held: Vec<(u64, u64)> = told.iter().filter_map(|line| held_on(line)).collect();
        let (mut acted, mut dropped) = (Vec::new(), Vec::new());
        let mut threads: Vec<u64> = Vec::new();
        for call in said {
            let (thread, how) = (call.thread, call.call.as_str());
            if !held.iter().any(|(held, _)| *held == thread) {
                dropped.push(format!(
                    "said {how} on #{thread}: the maintainer's words on it were not held for this wake"
                ));
            } else if threads.contains(&thread) {
                dropped.push(format!(
                    "said {how} on #{thread}: the answer names it twice"
                ));
            } else {
                threads.push(thread);
                let comments: Vec<u64> = (held.iter())
                    .filter(|(held, _)| *held == thread)
                    .map(|(_, comment)| *comment)
                    .collect();
                match self.pm_said_on(call, &comments) {
                    Ok(done) => acted.push(done),
                    Err(why) => dropped.push(why),
                }
            }
        }
        for (thread, _) in &held {
            if threads.contains(thread) {
                continue;
            }
            threads.push(*thread);
            let said = "The project manager read your comment and made no call on it, so nothing \
                        changed. It is still yours.";
            self.pm_says_on(*thread, said);
            dropped.push(format!(
                "made no call on the maintainer's words on #{thread}"
            ));
        }
        (acted, dropped)
    }

    // What one call did, or why it was refused, which it has said in the thread.
    fn pm_said_on(&mut self, call: &SaidCall, comments: &[u64]) -> Result<String, String> {
        let thread = call.thread;
        let why = plain(&self.shown(call.why.clone()), WORDS);
        let note = self.shown(call.note.clone());
        let ruling = (self.state.rulings.iter())
            .find(|r| {
                r.pull_request == Some(thread) || r.issue.and_then(|t| t.github()) == Some(thread)
            })
            .cloned();
        let done = match (&ruling, call.call) {
            (_, SaidHow::Nothing) => Err("it made no call".to_owned()),
            (Some(ruling), SaidHow::SendBack) => self.pm_sent_back(ruling, &note, &why),
            (Some(ruling), SaidHow::Settle) => self.pm_settled(ruling, &why),
            (Some(ruling), SaidHow::Park) => {
                let said = format!(
                    "**Project manager: parked**. Ruling #{} waits for you. {}",
                    ruling.number(),
                    inert(&why)
                );
                self.pm_says(ruling, &said);
                Ok(format!("parked on ruling {}: {why}", ruling.id))
            }
            (None, SaidHow::Settle) => Err("no ruling waits on the thread to settle".to_owned()),
            (None, how) => {
                let released = Released {
                    call: how,
                    note,
                    why: why.clone(),
                };
                self.pm_releases(thread, comments, released)
                    .map(|()| format!("{} on #{thread}: {why}", how.as_str()))
            }
        };
        done.map_err(|refused| {
            let how = call.call.as_str();
            let said = format!(
                "The project manager's call on your comment was {how}, and it was not carried \
                 out: {}. It is still yours.",
                inert(&refused)
            );
            self.pm_says_on(thread, &said);
            format!("said {how} on #{thread}: {refused}: {why}")
        })
    }

    /// The comments the project manager called on with no ruling waiting,
    /// which the next comment read carries out
    pub(in crate::runner) fn pm_released(&self) -> &BTreeMap<u64, Released> {
        &self.pm.released
    }

    /// Forgets the calls on `comments`, which a comment read has carried out
    pub(in crate::runner) fn pm_carried_out(&mut self, comments: &[u64]) {
        self.pm
            .released
            .retain(|comment, _| !comments.contains(comment));
    }

    // Hands the comments back to the next comment read, as `released` says,
    // for the work item whose pull request is `thread`.
    fn pm_releases(
        &mut self,
        thread: u64,
        comments: &[u64],
        released: Released,
    ) -> Result<(), String> {
        let open = |phase: &Phase| !matches!(phase, Phase::Done { .. });
        let item = (self.state.work_items.iter())
            .find(|i| i.pull_request == Some(thread) && open(&i.phase))
            .map(|i| i.issue);
        let Some(item) = item else {
            return Err(format!("no open work item has pull request #{thread}"));
        };
        let mut next = self.state.clone();
        if let Some(item) = next.item_mut(item) {
            item.talked.comments.retain(|c| !comments.contains(c));
        }
        if next != self.state {
            self.save(next)
                .map_err(|e| format!("it could not be saved: {e}"))?;
        }
        for comment in comments {
            self.pm.released.insert(*comment, released.clone());
        }
        Ok(())
    }

    // Posts `text` in `ruling`'s thread, to the maintainer where they are known.
    pub(super) fn pm_says(&mut self, ruling: &Ruling, text: &str) {
        let Some(thread) = (ruling.pull_request).or_else(|| ruling.issue.and_then(|t| t.github()))
        else {
            return;
        };
        self.pm_says_on(thread, text);
    }

    // Posts `text` on `thread`, to the maintainer where they are known.
    fn pm_says_on(&mut self, thread: u64, text: &str) {
        let text = match self.maintainer() {
            Ok(login) => format!("@{login} {text}"),
            Err(_) => text.to_owned(),
        };
        self.say(thread, &text);
    }
}

/// Whether the project manager may decide a ruling named `name`
///
/// # Errors
///
/// The reason, for a kind the project manager never decides.
pub(super) fn never_decided(name: RulingName) -> Result<(), String> {
    match name.pm_default() {
        Some(_) => Ok(()),
        None => Err(format!(
            "the project manager never decides a `{}` ruling",
            name.as_str()
        )),
    }
}

// How the maintainer answers `ruling` with option `key`, in its thread.
fn reply_form(ruling: &Ruling, key: char) -> Option<String> {
    let name = inert(&named(&ruling.kind, key)?);
    Some(match answer_for(&ruling.kind, key, "<note>")? {
        Answer::Text(_) => format!("with the option {name}"),
        answer => format!("`{}` ({name})", answer.params(ruling.number())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::ticket;

    #[test]
    fn a_pm_reply_is_read_with_or_without_the_rulings_number() {
        let read = |words| pm_reply(words, ticket(71));
        assert_eq!(
            read("pm recommend"),
            Some(Ok(PmReply::Ask(Asked::Recommend)))
        );
        assert_eq!(
            read("71 PM Recommend"),
            Some(Ok(PmReply::Ask(Asked::Recommend)))
        );
        assert_eq!(read("pm options"), Some(Ok(PmReply::Ask(Asked::Options))));
        assert_eq!(read("pm decide"), Some(Ok(PmReply::Ask(Asked::Decide))));
        assert_eq!(read("71 pm 2"), Some(Ok(PmReply::Take(2))));
        assert_eq!(read("pm 0"), Some(Err(TAKES.to_owned())));
        assert_eq!(read("pm"), Some(Err(TAKES.to_owned())));
        assert_eq!(read("pm recommend now"), Some(Err(TAKES.to_owned())));
        assert_eq!(read("yes"), None);
        assert_eq!(read("72 pm recommend"), None);
    }
}
