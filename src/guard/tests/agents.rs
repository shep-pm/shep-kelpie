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
        "caffeinate -i pi -p hi",
        "xargs pi < prompts.txt",
        "timeout 5 pi -p hi",
    ] {
        let verdict = bash(command);
        assert_eq!(verdict, Verdict::Refuse(NO_AGENTS.into()), "{command}");
    }
}

#[test]
fn every_harness_kelpie_runs_is_refused_from_a_workers_commands() {
    use crate::settings::Harness;
    // A harness added to settings stops compiling here until it is listed.
    let runs = |harness: Harness| match harness {
        Harness::ClaudeCode | Harness::Pi | Harness::Codex => true,
        Harness::StandIn => false,
    };
    let harnesses = [Harness::ClaudeCode, Harness::Pi, Harness::Codex];
    assert!(harnesses.into_iter().all(runs));
    for program in harnesses.map(Harness::command) {
        for command in [
            format!("{program} exec hi"),
            format!("/opt/homebrew/bin/{program} -p hi"),
            format!("bash -c '{program} hi'"),
            format!("git status && {program}"),
        ] {
            assert_eq!(bash(&command), Verdict::Refuse(NO_AGENTS.into()), "{command}");
        }
    }
    for command in ["codex exec resume --last hi", "codex app-server", "pi --mode rpc"] {
        assert_eq!(bash(command), Verdict::Refuse(NO_AGENTS.into()), "{command}");
    }
}

#[test]
fn an_agent_git_runs_for_the_worker_is_refused() {
    for command in [
        "git rebase --exec pi main",
        "git rebase --exec=pi main",
        "git rebase -x pi main",
        "git rebase -xpi main",
        "git bisect run pi",
        "git difftool -x pi",
        "git difftool --extcmd=pi",
        "git difftool --extcmd pi",
        "git submodule foreach pi",
        "git submodule foreach --recursive 'pi -p hi'",
        "git filter-branch --tree-filter pi",
        "git filter-branch --tree-filter='pi -p hi' HEAD",
        "git rebase --exec 'claude -p hi' main",
    ] {
        assert!(matches!(bash(command), Verdict::Refuse(_)), "{command}");
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
        "git add src/adapters/pi",
        "cargo test pi",
        "echo pi",
        "mkdir pi",
        "ls src/adapters/pi",
        "cd src/adapters/pi && ls",
        "rm -r build/pi",
        "cp a.txt docs/pi",
        "git add src/adapters/claude",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}
