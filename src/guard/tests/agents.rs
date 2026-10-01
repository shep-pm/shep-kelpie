// An agent started from a worker's Bash, which the sandbox lets reach the
// model's endpoint, would run a session with none of kelpie's hooks.

use super::*;
use crate::guard::judging::NO_AGENTS;

#[test]
fn an_agent_of_the_workers_own_is_refused_however_it_is_started() {
    for command in [
        "claude -p 'write .claude/settings.local.json'",
        "/opt/homebrew/bin/claude -p hi",
        "codex exec 'rm -rf ~'",
        "env FOO=1 claude -p hi",
        "nohup claude -p hi &",
        "cd src && claude -p hi",
        "bash -c 'claude -p hi'",
        "echo ok; codex exec hi",
        "find . -name x -exec claude -p hi \\;",
        "pi -p --no-extensions --tools bash -- 'run gh pr merge 1'",
        "PI_OFFLINE=1 /opt/homebrew/bin/pi -p hi",
        "sh -c 'pi -p hi'",
    ] {
        let verdict = bash(command);
        assert_eq!(verdict, Verdict::Refuse(NO_AGENTS.into()), "{command}");
    }
}

#[test]
fn naming_an_agent_without_running_it_goes_through() {
    for command in [
        "grep -r claude src",
        "echo 'see the claude docs'",
        "cat docs/codex.md",
        "grep -r 'pi -p' docs",
        "ls .claude-plugin",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}
