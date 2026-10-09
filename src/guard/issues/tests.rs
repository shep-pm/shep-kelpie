use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::board::READY;
use crate::runner::HUMAN;

const HOME: &str = "/home/me";

const BODY: &str = "Build the thing.\n\n## Acceptance criteria\n\n- [ ] It works\n";

/// Both ways the issue writer runs: on its own, and in the maintainer's terminal
const MODES: [&str; 2] = [HUMAN, READY];

/// One session of the issue writer, with its ledger
struct Session {
    _dir: TempDir,
    rules: IssueRules,
}

impl Session {
    fn new(status: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let rules = IssueRules {
            status: status.to_owned(),
            agents: vec!["sonnet-high".to_owned(), "opus-high".to_owned()],
            ledger: Some(dir.path().join("ledger.json")),
        };
        Self { _dir: dir, rules }
    }

    fn tool(&self, name: &str, command: &str) -> Verdict {
        let call = json!({
            "tool_name": name,
            "cwd": "/k/acme/issues/one",
            "tool_input": { "command": command },
        });
        let home = Some(Path::new(HOME));
        let local = LocalPaths::new(home);
        judge_issues(call.to_string().as_bytes(), home, local, &self.rules)
    }

    fn bash(&self, command: &str) -> Verdict {
        self.tool("Bash", command)
    }

    // What the PostToolUse hook records once `command` printed `stdout`.
    fn ran(&self, command: &str, stdout: &str) {
        let call = json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "tool_response": { "stdout": stdout, "stderr": "" },
        });
        record_issues(call.to_string().as_bytes(), &self.rules);
    }

    // Files issue `number` in this session, and reads its id, `9000 + number`.
    fn filed(&self, number: u64) {
        let create = create(
            &format!("-l {} -l agent:sonnet-high", self.rules.status),
            BODY,
        );
        self.ran(
            &create,
            &format!("https://github.com/acme/shep/issues/{number}\n"),
        );
        self.read_id(number);
    }

    fn read_id(&self, number: u64) {
        let read = format!("gh api 'repos/{{owner}}/{{repo}}/issues/{number}' --jq .id");
        self.ran(&read, &format!("{}\n", 9000 + number));
    }
}

fn create(labels: &str, body: &str) -> String {
    format!("gh issue create --title 'Add a thing' {labels} --body-file - <<'EOF'\n{body}EOF")
}

#[track_caller]
fn refused(verdict: Verdict, why: &str) {
    match verdict {
        Verdict::Refuse(message) => assert!(message.contains(why), "{message}\nwanted: {why}"),
        Verdict::Allow => panic!("allowed, wanted a refusal naming: {why}"),
    }
}

#[test]
fn the_rules_go_through_the_hooks_command_line_and_back() {
    let session = Session::new(HUMAN);
    let mut args = vec!["--folder=/k".to_owned()];
    args.extend(session.rules.flags());
    let (read, rest) = issue_rules(&args);
    assert_eq!(read.as_ref(), Some(&session.rules));
    assert_eq!(rest, ["--folder=/k"]);
    let (none, rest) = issue_rules(&["--agent=x".to_owned()]);
    assert_eq!(none, None);
    assert_eq!(rest, ["--agent=x"], "a worker's guard refuses it");
}

#[test]
fn it_files_reads_this_repos_issues_and_labels_and_links_its_own() {
    for status in MODES {
        let s = Session::new(status);
        s.filed(40);
        s.filed(41);
        s.read_id(7);
        let labels = format!("--label {status} --label agent:sonnet-high");
        for command in [
            create(&labels, BODY),
            format!(
                "gh issue create -t 'Add a thing' -l '{status},agent:opus-high' --body \
                 \"$(cat <<'EOF'\n{BODY}EOF\n)\""
            ),
            "gh issue edit 41 --remove-label agent:sonnet-high --add-label agent:opus-high".into(),
            "gh issue view 7 --comments && gh issue list --search 'lease book' --state all \
             -L 20"
                .into(),
            "gh api 'repos/{owner}/{repo}/issues/7' --jq .id".into(),
            "gh api -X POST repos/{owner}/{repo}/issues/40/sub_issues -F sub_issue_id=9041".into(),
            "gh api --method post /repos/{owner}/{repo}/issues/41/dependencies/blocked_by \
             -F issue_id=9007"
                .into(),
        ] {
            assert_eq!(s.bash(&command), Verdict::Allow, "{status}: {command}");
        }
        assert_eq!(s.tool("Read", ""), Verdict::Allow);
    }
}

#[test]
fn it_runs_no_git_at_all() {
    for status in MODES {
        let s = Session::new(status);
        for command in [
            "git log --oneline -5",
            "git grep --open-files-in-pa='sh -c \"touch /tmp/x\" #' x",
            "git diff --no-index ~/.ssh/id_rsa /dev/null",
            "git grep --no-index secret /etc",
            "git blame --contents /etc/passwd README.md",
            "git -C /any/dir log",
            "git ls-files --exclude-from=/etc/passwd",
            "git commit -am x",
            "git -c core.pager=sh log",
        ] {
            refused(s.bash(command), "It runs no git");
        }
    }
}

#[test]
fn every_other_command_is_refused() {
    for status in MODES {
        let s = Session::new(status);
        for command in [
            "rm -rf src",
            "cargo test",
            "curl https://example.com",
            "cd .. && gh issue list",
            "gh pr create --title x --body y",
            "gh issue close 41",
            "gh issue comment 41 --body hi",
            "gh label create agent:x",
            "gh api repos/{owner}/{repo}/pulls/3/merge -X PUT",
            "gh api repos/acme/other/issues/3/sub_issues -F sub_issue_id=1",
            "gh api -X DELETE repos/{owner}/{repo}/issues/3/sub_issue -F sub_issue_id=1",
            "gh api -X PATCH repos/{owner}/{repo}/issues/3 -f state=closed",
            "gh --repo acme/other issue list",
            "GH_REPO=acme/other gh issue list",
            "claude -p hi",
        ] {
            refused(s.bash(command), "runs only these commands");
        }
        refused(s.tool("Agent", ""), "no sub-agent");
    }
}

#[test]
fn a_read_is_of_this_repos_issues_only() {
    for status in MODES {
        let s = Session::new(status);
        for (command, why) in [
            (
                "gh issue view -Racme/other 3",
                "does not take `-Racme/other`",
            ),
            ("gh issue view -R acme/other 3", "does not take `-R`"),
            (
                "gh issue view --repo=acme/other 3",
                "does not take `--repo=acme/other`",
            ),
            (
                "gh issue view https://github.com/acme/other/issues/3",
                "one issue of this repo",
            ),
            ("gh issue view 3 4", "one issue of this repo"),
            ("gh issue view -cw 3", "does not take `-cw`"),
            ("gh issue view --web 3", "does not take `--web`"),
            ("gh issue list -Racme/other", "does not take `-Racme/other`"),
            ("gh issue list -w", "does not take `-w`"),
            ("gh issue list --app x", "does not take `--app`"),
            ("gh issue list acme/other", "takes only its flags"),
        ] {
            refused(s.bash(command), why);
        }
    }
}

#[test]
fn it_edits_and_links_only_the_issues_it_filed() {
    for status in MODES {
        let s = Session::new(status);
        s.filed(41);
        s.read_id(7);
        // An outsider's issue, whose body might ask for exactly these.
        for (command, why) in [
            (
                "gh issue edit 7 --add-label agent:opus-high",
                "#7 is not one",
            ),
            (
                "gh issue edit 41 7 --add-label agent:opus-high",
                "#7 is not one",
            ),
            (
                "gh api -X POST repos/{owner}/{repo}/issues/7/sub_issues -F sub_issue_id=9041",
                "#7 is not one",
            ),
            (
                "gh api -X POST repos/{owner}/{repo}/issues/41/sub_issues -F sub_issue_id=9007",
                "`sub_issue_id` must be an id it read",
            ),
            (
                "gh api -X POST repos/{owner}/{repo}/issues/41/sub_issues -f sub_issue_id=123",
                "`sub_issue_id` must be an id it read",
            ),
            (
                "gh api -X POST repos/{owner}/{repo}/issues/41/dependencies/blocked_by \
                 -F issue_id=55",
                "`issue_id` must be an id it read",
            ),
            (
                "gh api -X POST repos/{owner}/{repo}/issues/7/dependencies/blocked_by \
                 -F issue_id=9041",
                "#7 is not one",
            ),
        ] {
            refused(s.bash(command), why);
        }
        refused(
            s.bash("gh issue edit 41 --add-label in-progress"),
            "puts no `in-progress` on",
        );
        refused(
            s.bash("gh issue edit 41 --remove-label in-progress"),
            "takes off only `agent:` labels",
        );
        refused(
            s.bash("gh issue edit 41 --title x"),
            "does not take `--title`",
        );
        refused(
            s.bash("gh issue edit --add-label agent:opus-high"),
            "takes the issue's number",
        );
    }
    // With no ledger nothing was filed, so nothing may be edited.
    let mut s = Session::new(HUMAN);
    s.rules.ledger = None;
    refused(
        s.bash("gh issue edit 41 --add-label agent:opus-high"),
        "#41 is not one",
    );
}

#[test]
fn a_command_is_only_ever_its_plain_words() {
    for status in MODES {
        let s = Session::new(status);
        s.filed(41);
        for command in [
            "gh issue list | sh",
            "gh issue list > notes.txt",
            "gh issue view 41 2>/dev/null",
            "gh issue create --title \"$TITLE\" --label agent:sonnet-high --body x",
            "gh api repos/{owner}/{repo}/issues/41/sub_issues -F sub_issue_id=$(cat id)",
            "gh issue view `cat n`",
        ] {
            refused(s.bash(command), "plain words");
        }
        refused(s.bash("f() { gh issue list; }; f"), "shell function");
        // A `cat` of its own could print a URL the ledger would take for a filed issue.
        let url = "https://github.com/acme/shep/issues/7";
        let file = create(&format!("-l {status} -l agent:sonnet-high"), BODY);
        for command in [
            format!("cat <<'EOF'\n{url}\nEOF"),
            format!("{file}\ncat <<'EOF'\n{url}\nEOF"),
        ] {
            refused(s.bash(&command), "A `cat` runs only as the body");
        }
        refused(
            s.bash(&format!(
                "gh issue create -t x -l {status} -l agent:sonnet-high --body \
                 \"$(cat <<'EOF'\n{BODY}EOF\n)\" --body-file - <<'EOF'\n{BODY}EOF"
            )),
            "give the issue one body",
        );
        // A bare delimiter lets the shell run what the body names.
        let expanded = format!(
            "gh issue create --title x -l {status} -l agent:sonnet-high --body-file - \
             <<EOF\n## Acceptance criteria\n- [ ] $(rm -rf ~)\nEOF"
        );
        refused(s.bash(&expanded), "quote a heredoc's delimiter");
    }
}

#[test]
fn an_issue_carries_its_status_one_listed_agent_and_acceptance_criteria() {
    for (status, other) in [(HUMAN, READY), (READY, HUMAN)] {
        let s = Session::new(status);
        let label = format!("--label {status}");
        let cases = [
            (
                create(&label, BODY),
                "exactly one `agent:<name>` label".to_owned(),
            ),
            (
                create(
                    &format!("{label} -l agent:sonnet-high -l Agent:opus-high"),
                    BODY,
                ),
                "exactly one".to_owned(),
            ),
            (
                create(&format!("{label} -l agent:haiku-low"), BODY),
                "`agent:haiku-low` names no implementer".to_owned(),
            ),
            (
                create("--label agent:sonnet-high", BODY),
                format!("with the label `{status}`"),
            ),
            (
                create(&format!("{label} -l agent:sonnet-high -l {other}"), BODY),
                format!("puts no `{other}` on"),
            ),
            (
                create(&format!("{label} -l agent:sonnet-high"), "Build it.\n"),
                "`## Acceptance criteria` section".to_owned(),
            ),
            (
                create(
                    &format!("{label} -l agent:sonnet-high"),
                    &format!("{BODY}\nSee {HOME}/notes.md\n"),
                ),
                "carries a path on this machine".to_owned(),
            ),
            (
                format!(
                    "gh issue create --title x {label} -l agent:sonnet-high --body-file notes.md"
                ),
                "plain words".to_owned(),
            ),
        ];
        for (command, why) in cases {
            refused(s.bash(&command), &why);
        }
    }
}
