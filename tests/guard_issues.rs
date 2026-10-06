//! `kelpie guard --issues=<label>`, as the issue writer's hooks run it,
//! against the real binary: it runs only the commands it lists, and edits
//! and links only the issues its own session filed.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const KELPIE: &str = env!("CARGO_BIN_EXE_shep-kelpie");

const ALLOW: i32 = 0;
const REFUSE: i32 = 2;

// The hook, as the issue writer's settings file runs it for `status`.
fn hook(dir: &Path, status: &str, record: bool, call: &Value) -> (i32, String) {
    let mut hook = Command::new(KELPIE);
    hook.arg("guard")
        .arg(dir.join("repo.git"))
        .arg(dir)
        .arg(format!("--folder={}", dir.display()))
        .arg(format!("--issues={status}"))
        .arg("--agent=sonnet-high")
        .arg(format!("--ledger={}", dir.join("ledger.json").display()));
    if record {
        hook.arg("--record");
    }
    let mut hook = hook
        .env("HOME", dir.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(call.to_string().as_bytes())
        .unwrap();
    let out = hook.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.code().unwrap_or(-1), said)
}

fn judged(dir: &Path, status: &str, command: &str) -> (i32, String) {
    let call = json!({ "tool_name": "Bash", "cwd": dir, "tool_input": { "command": command } });
    hook(dir, status, false, &call)
}

// What the PostToolUse hook records once `command` printed `stdout`.
fn ran(dir: &Path, status: &str, command: &str, stdout: &str) {
    let call = json!({
        "tool_name": "Bash",
        "cwd": dir,
        "tool_input": { "command": command },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false },
    });
    assert_eq!(
        hook(dir, status, true, &call).0,
        0,
        "the recorder never refuses"
    );
}

const FILE: &str = "gh issue create --title x --label {status} --label agent:sonnet-high \
                    --body-file - <<'EOF'\n## Acceptance criteria\n- [ ] x\nEOF";

#[test]
fn the_issue_writers_hook_runs_only_what_it_lists() {
    for status in ["ready-for-human", "ready-for-agent"] {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let file = FILE.replace("{status}", status);
        // The session files #41 and reads its id, and reads #7's.
        ran(
            dir,
            status,
            &file,
            "https://github.com/acme/shep/issues/41\n",
        );
        ran(
            dir,
            status,
            "gh api 'repos/{owner}/{repo}/issues/41' --jq .id",
            "9041\n",
        );
        ran(
            dir,
            status,
            "gh api 'repos/{owner}/{repo}/issues/7' --jq .id",
            "9007\n",
        );
        // A forged record: a `cat` printing #8's URL beside a create.
        let url8 = "https://github.com/acme/shep/issues/8";
        let forged = format!("{file}\ncat <<'EOF'\n{url8}\nEOF");
        ran(dir, status, &forged, &format!("{url8}\n"));
        let cat = format!("cat <<'EOF'\n{url8}\nEOF");
        let table = [
            (cat.as_str(), REFUSE),
            (forged.as_str(), REFUSE),
            ("gh issue edit 8 --add-label agent:sonnet-high", REFUSE),
            (
                "git grep --open-files-in-pa='sh -c \"touch pwned\" #' x",
                REFUSE,
            ),
            ("git diff --no-index ~/.ssh/id_rsa /dev/null", REFUSE),
            ("git grep --no-index secret /etc", REFUSE),
            ("git blame --contents /etc/passwd README.md", REFUSE),
            ("git -C /any/dir log", REFUSE),
            ("git ls-files --exclude-from=/etc/passwd", REFUSE),
            ("git log -5", REFUSE),
            ("gh issue view -Racme/other 3", REFUSE),
            (
                "gh issue view https://github.com/acme/other/issues/3",
                REFUSE,
            ),
            ("gh issue view -cw 3", REFUSE),
            ("gh issue list -Racme/other", REFUSE),
            ("gh issue list --repo=acme/other", REFUSE),
            ("gh issue edit 7 --add-label ready-for-agent", REFUSE),
            ("gh issue edit 7 --add-label agent:sonnet-high", REFUSE),
            (
                "gh api -X POST 'repos/{owner}/{repo}/issues/41/sub_issues' -f sub_issue_id=123",
                REFUSE,
            ),
            (
                "gh api -X POST 'repos/{owner}/{repo}/issues/7/sub_issues' -F sub_issue_id=9041",
                REFUSE,
            ),
            ("cargo test", REFUSE),
            ("gh issue view 7 --comments", ALLOW),
            ("gh issue list --search 'lease book' --state all", ALLOW),
            (file.as_str(), ALLOW),
            ("gh issue edit 41 --add-label agent:sonnet-high", ALLOW),
            (
                "gh api -X POST 'repos/{owner}/{repo}/issues/41/dependencies/blocked_by' \
                 -F issue_id=9007",
                ALLOW,
            ),
        ];
        for (command, wanted) in table {
            let (code, said) = judged(dir, status, command);
            let verdict = if code == ALLOW { "allow" } else { "refuse" };
            eprintln!("{status} | {verdict} | {command:?}");
            assert_eq!(code, wanted, "{status}: {command}\n{said}");
        }
        assert!(!dir.join("pwned").exists());
    }
}
