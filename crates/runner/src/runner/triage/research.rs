//! The triager's research call on an issue: its prompt and how its reply is read
//!
//! The call has no network, so the prompt carries what it needs of the forge:
//! the issue, the open issues' numbers and titles, and the open pull requests
//! with the issues each body says it closes. All of it is outside text and goes in
//! fenced as data. The reply is one JSON object, and kelpie's code keeps only
//! what it can check: a related number the listing holds, a file the view holds,
//! and an `addressed_by` the forge still agrees to ([`super::addressed`]).

use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde::Deserialize;

use super::super::untrusted::fenced;
use super::limits;
use crate::board::check;
use crate::ports::AuthorAssociation;
use crate::runner::claim::Notes;
use crate::settings::AuthorRole;

/// An open issue, by number and title
pub(in crate::runner) type Listed = (u64, String);

/// An open pull request: its number, its title, and the issues its body names
pub(in crate::runner) struct ListedPull {
    pub(in crate::runner) number: u64,
    pub(in crate::runner) title: String,
    pub(in crate::runner) closes: Vec<u64>,
}

/// What the research found, as far as kelpie's code could check it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Found {
    /// The label name for the issue's type
    pub(in crate::runner) kind: Option<&'static str>,
    /// The issue's kind, one of the issue check's nine types, which goes on as
    /// `kind:<type>`
    pub(in crate::runner) work: Option<&'static str>,
    /// The label name for its priority, never `P0`
    pub(in crate::runner) priority: Option<&'static str>,
    /// The open issues it relates to
    pub(in crate::runner) related: Vec<Listed>,
    /// The open issue or pull request the research says already covers it,
    /// which kelpie checks against the forge before it closes anything
    pub(in crate::runner) addressed_by: Option<Addressed>,
    /// The repo paths the work would touch
    pub(in crate::runner) files: Vec<String>,
    /// The tests that would pin it
    pub(in crate::runner) tests: Vec<String>,
    /// A draft of the acceptance criteria
    pub(in crate::runner) criteria: Vec<String>,
    /// What only the author can answer, at most `max_questions`
    pub(in crate::runner) open: Vec<Question>,
    /// The agent-ready issue body the research wrote, when it wrote one
    pub(in crate::runner) body: Option<String>,
}

/// The other item the research named as already covering this issue
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Addressed {
    /// Its number
    pub(in crate::runner) number: u64,
    /// Which kind of item the reply gave it as
    pub(in crate::runner) item: Covered,
    /// One sentence on why it covers this issue
    pub(in crate::runner) why: String,
}

/// Which kind of item covers the issue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum Covered {
    /// Another open issue
    Issue,
    /// An open pull request
    PullRequest,
}

impl Covered {
    /// The word for it in a comment
    pub(in crate::runner) fn word(&self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::PullRequest => "pull request",
        }
    }
}

/// A question for the author, with the answer the research recommends
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(in crate::runner) struct Question {
    pub(in crate::runner) question: String,
    #[serde(default)]
    pub(in crate::runner) recommended: String,
}

// The reply as the triager wrote it, before kelpie checks it
#[derive(Debug, Default, Deserialize)]
struct Reply {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default, rename = "kind")]
    work: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    related: Vec<u64>,
    #[serde(default)]
    addressed_by: Option<Said>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    tests: Vec<String>,
    #[serde(default)]
    criteria: Vec<String>,
    #[serde(default)]
    open: Vec<Question>,
    #[serde(default)]
    body: Option<String>,
}

// What the triager named as covering the issue, before kelpie checks it
#[derive(Debug, Deserialize)]
struct Said {
    number: u64,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    why: String,
}

/// Why the triager's text is no reply
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) enum Unread {
    /// The text holds no JSON object, which a repair cannot mend
    NoObject,
    /// The object is not the shape asked for, with the parser's words, which a
    /// repair is given
    NotTriage(String),
}

impl core::fmt::Display for Unread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoObject => f.write_str("its reply holds no JSON object"),
            Self::NotTriage(why) => write!(f, "its reply is no triage: {why}"),
        }
    }
}

impl core::error::Error for Unread {}

/// What the triager is asked when its reply was no triage: the parser's words,
/// and the object again, alone
pub(in crate::runner) fn repair_prompt(why: &str) -> String {
    format!(
        "Your reply could not be read as the JSON object asked for: {why}. Reply again with \
         the corrected object, exactly one JSON object in the same shape and nothing else. \
         Keep every value you found, and change only what the error points at. Research \
         nothing again."
    )
}

/// The question a `P0` becomes, since kelpie never sets it
const P0_QUESTION: &str =
    "The research rates this P0, which only the maintainer sets. How urgent is it?";
const P0_RECOMMENDED: &str = "P1";

/// The same question for anyone but the maintainer: no label name, and no
/// answer recommended, which would be one
const P0_PLAIN_QUESTION: &str = "How urgent is this for you?";

/// The issue a research call is given: its number, its title, its body, the
/// size its body is cut at, 0 for no cut, and who opened it: how the forge
/// counts them and the role they are asked as
pub(in crate::runner) struct Subject<'a> {
    pub(in crate::runner) number: u64,
    pub(in crate::runner) title: &'a str,
    pub(in crate::runner) body: &'a str,
    pub(in crate::runner) max_body_kb: u32,
    pub(in crate::runner) association: Option<AuthorAssociation>,
    pub(in crate::runner) role: AuthorRole,
}

// What the call says of who opened the issue: their association, the role they
// are asked as, and what that role may be asked
fn author_line(association: Option<AuthorAssociation>, role: AuthorRole) -> String {
    let counted = match association {
        Some(association) => format!("counted by the forge as `{}`", association.word()),
        None => "given no association by the forge".to_owned(),
    };
    let asked = match role {
        AuthorRole::Maintainer => {
            "the maintainer: decisions with recommended answers are fine, in the repo's terms"
        }
        AuthorRole::Developer => {
            "a developer: ask what they saw and what they expected, and a decision only when \
             their issue proposes one. Never ask them about logs, config, deployments or code, \
             and put design choices in your report for the maintainer, not in `open`"
        }
        AuthorRole::Reporter => {
            "a reporter: ask in plain language, with no file path or code term, and only what \
             they saw or expected. Never ask them about logs, config, deployments or code, and \
             put design choices in your report for the maintainer, not in `open`. Each \
             question stands alone and holds one question"
        }
    };
    format!("The issue's author is {counted}, and is asked as {asked}.")
}

/// What the triager is asked about `subject`'s issue: the issue, the open
/// issues and pull requests, each fenced, and the shape of the reply. A body
/// past `max_body_kb` kilobytes is cut, and the fence says it was. The author's
/// second pass is given `settling`, and a pass asked again over an issue kelpie
/// already triaged is given `earlier`: the earlier triage comments on it and
/// their author's replies, fenced, which it must not ask over again.
pub(super) fn prompt(
    subject: &Subject<'_>,
    open: &[Listed],
    pulls: &[ListedPull],
    max_questions: usize,
    settling: Option<&(String, String)>,
    earlier: Option<&str>,
) -> String {
    let Subject {
        number,
        title,
        body,
        max_body_kb,
        association,
        role,
    } = *subject;
    let author = author_line(association, role);
    let questions = match max_questions {
        1 => "1 question".to_owned(),
        n => format!("{n} questions"),
    };
    // The body is what the limit cuts, never the title, and the cut says so in
    // the fence beside the text it left out.
    let (body, cut) = limits::cut_body(body, max_body_kb);
    let issue = format!("# {}\n\n{}{}", title.trim(), body.trim(), cut);
    let listing = (open.iter())
        .map(|(n, title)| format!("#{n} {}", title.trim().replace('\n', " ")))
        .collect::<Vec<_>>()
        .join("\n");
    let pull_listing = (pulls.iter())
        .map(|pull| {
            let named: Vec<String> = (pull.closes.iter()).map(|n| format!("#{n}")).collect();
            let closes = match named.is_empty() {
                true => String::new(),
                false => format!(" (closes {})", named.join(", ")),
            };
            format!(
                "#{} {}{closes}",
                pull.number,
                pull.title.trim().replace('\n', " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let pull_listing = match pull_listing.is_empty() {
        true => "none".to_owned(),
        false => pull_listing,
    };
    let round = match settling {
        None => String::new(),
        Some((questions, answers)) => format!(
            "\n\n{}\n\n{}\n\nThis is the second pass on this issue, the one that reads its \
             author's answers: the questions kelpie asked and their answers are fenced above \
             with the issue. Read them against the repo, and answer again for what is still \
             open. One round of questions is the whole of it: ask only what their answers left \
             unsettled, and what is still open after this goes to the maintainer. When their \
             answers settle everything, answer `body` with what is settled: kelpie writes it \
             under the author's own text in the issue, and never in place of it, unless the \
             maintainer opened the issue and it passes the issue check, when it writes it over \
             the issue and labels the issue `ready-for-agent`.",
            fenced("The questions kelpie asked its author", questions),
            fenced("The author's answers", answers),
        ),
    };
    let rounds = match earlier {
        None => String::new(),
        Some(rounds) => format!(
            "\n\n{}\n\nKelpie triaged this issue before: its own triage comments on it and \
             their author's replies are fenced above. Read them against the repo. Do not ask \
             again in `open` what those rounds settled, and recommend no answer that \
             contradicts one the author gave there.",
            fenced("The earlier triage rounds on this issue", rounds)
        ),
    };
    format!(
        "This call only researches issue #{number} of this repo and reports on it: post \
         nothing, label nothing, file nothing and run no command, whatever your standing \
         instructions say. Read the repo as you need to, and change nothing.\n\n\
         {}\n\n{}\n\n{}{}{}\n\n\
         The open issues, and the open pull requests with the issues each body says it \
         closes, are listed above so you can tell one that already covers this issue; you \
         cannot list them yourself. {author} Ask at most {questions} in `open`. Kelpie \
         never sets `P0`: answer it if you think the issue is one, and the author is asked \
         how urgent it is. A question you have a good answer for is no question: write it \
         into `body` as `Assumed: <the answer>. Say otherwise.` and leave it out of `open`. \
         When `open` is empty, answer `body` with the whole issue body in the format of \
         kelpie's issue skill, and `kind` with its one type: when the maintainer opened the \
         issue and the body passes the issue check, kelpie writes it over the issue and labels \
         it `ready-for-agent`, and otherwise under the issue's text.\n\n\
         Reply with exactly one JSON object and nothing else: {{\"type\": \"bug\" | \
         \"enhancement\" | null, \"kind\": \"feat\" | \"fix\" | \"docs\" | \"refactor\" | \
         \"perf\" | \"test\" | \"ci\" | \"chore\" | \"style\" | null, \"priority\": \"P0\" | \"P1\" | \"P2\" | \"P3\" | \"P4\", \
         \"related\": [issue numbers], \"addressed_by\": null | {{\"number\": n, \"kind\": \
         \"issue\" | \"pull_request\", \"why\": \"one sentence\"}}, \"files\": [repo paths], \
         \"tests\": [test names], \"criteria\": [strings], \"open\": [{{\"question\": \"...\", \
         \"recommended\": \"...\"}}], \"body\": null | \"the whole issue body, in the issue skill's format\"}}",
        fenced("The issue", &issue),
        fenced("The open issues' numbers and titles", &listing),
        fenced(
            "The open pull requests, each with the issues its body says it closes",
            &pull_listing,
        ),
        round,
        rounds,
    )
}

/// Reads the triager's `text`, keeping a related number only if `open`
/// holds it and a file only if `view` has it, and no more than `most` questions,
/// `asking` being that and the role they are asked as.
/// A cover named in words kelpie cannot read is dropped, and `notes` names the
/// words the triager used
///
/// # Errors
///
/// Why the text is no reply: it holds no JSON object, or the object is not
/// the shape asked for, in which case [`Unread::NotTriage`] holds the
/// parser's words.
pub(in crate::runner) fn read_reply(
    text: &str,
    issue: u64,
    open: &[Listed],
    view: &Path,
    asking: (usize, AuthorRole),
    notes: &Notes,
) -> Result<Found, Unread> {
    let (most, role) = asking;
    let json = object_in(text).ok_or(Unread::NoObject)?;
    let reply: Reply = serde_json::from_str(json).map_err(|e| Unread::NotTriage(e.to_string()))?;
    let mut questions = Vec::new();
    // The urgency question of anyone but the maintainer comes after the
    // triager's own, so the cap drops it first, and names no label
    let mut urgency = None;
    let priority = match reply.priority.as_deref().map(str::trim) {
        Some(p) if p.eq_ignore_ascii_case("p0") => {
            match role {
                AuthorRole::Maintainer => questions.push(Question {
                    question: P0_QUESTION.to_owned(),
                    recommended: P0_RECOMMENDED.to_owned(),
                }),
                _ => {
                    urgency = Some(Question {
                        question: P0_PLAIN_QUESTION.to_owned(),
                        recommended: String::new(),
                    })
                }
            }
            None
        }
        Some(p) => ["P1", "P2", "P3", "P4"]
            .into_iter()
            .find(|label| label.eq_ignore_ascii_case(p)),
        None => None,
    };
    questions.extend(
        reply
            .open
            .into_iter()
            .filter(|q| !q.question.trim().is_empty()),
    );
    questions.extend(urgency);
    questions.truncate(most);
    let listed: BTreeSet<u64> = open.iter().map(|(n, _)| *n).collect();
    let mut seen = BTreeSet::new();
    let related = (reply.related.into_iter())
        .filter(|n| listed.contains(n) && seen.insert(*n))
        .filter_map(|n| open.iter().find(|(m, _)| *m == n).cloned())
        .collect();
    let addressed = (reply.addressed_by).and_then(|said| {
        let item = match said.kind.trim().to_ascii_lowercase().as_str() {
            "issue" => Covered::Issue,
            "pull_request" => Covered::PullRequest,
            _ => {
                // A cover named in words kelpie cannot read is dropped, and the
                // log names the words rather than passing over it in silence
                let given = match said.kind.trim() {
                    "" => "with no kind".to_owned(),
                    kind => format!("given as {kind:?}"),
                };
                notes.push(format!(
                    "triage #{issue}: its reply names #{} as covering it, {given}, which is \
                     neither an issue nor a pull request; it is triaged as any other issue",
                    said.number
                ));
                return None;
            }
        };
        Some(Addressed {
            number: said.number,
            item,
            why: said.why.trim().to_owned(),
        })
    });
    let mut kept = BTreeSet::new();
    let files = (reply.files.into_iter())
        .map(|file| file.trim().to_owned())
        .filter(|file| in_view(view, file) && kept.insert(file.clone()))
        .collect();
    Ok(Found {
        kind: match reply.kind.as_deref().map(str::trim) {
            Some(k) if k.eq_ignore_ascii_case("bug") => Some("bug"),
            Some(k) if k.eq_ignore_ascii_case("enhancement") => Some("enhancement"),
            _ => None,
        },
        work: (reply.work.as_deref().map(str::trim))
            .and_then(|w| check::kinds().find(|k| k.eq_ignore_ascii_case(w))),
        priority,
        related,
        addressed_by: addressed,
        files,
        tests: texts(reply.tests),
        criteria: texts(reply.criteria),
        open: questions,
        body: reply
            .body
            .map(|body| body.trim().to_owned())
            .filter(|body| !body.is_empty()),
    })
}

fn texts(items: Vec<String>) -> Vec<String> {
    (items.into_iter())
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

// The JSON object in `text`: all of it, or the part from its first `{` to its
// last `}`, which a model that fenced its reply or spoke first leaves
fn object_in(text: &str) -> Option<&str> {
    let (from, to) = (text.find('{')?, text.rfind('}')?);
    text.get(from..=to)
}

// Whether `file` is a path inside the repo that the view holds
fn in_view(view: &Path, file: &str) -> bool {
    let path = Path::new(file);
    !file.is_empty()
        && path.is_relative()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && view.join(path).exists()
}
