//! What only the project manager does, refused in every shape a worker could write it

use std::os::unix::fs::PermissionsExt;

use super::*;

const TO_BASE: &str = "only the project manager changes `main`";
const MANAGER: &str = "only the project manager merges";

// A folder with nothing in it, for calls that need no worktree.
fn anywhere(command: &str) -> Verdict {
    bash_in(Path::new("/x"), command)
}

fn refused_with(verdict: Verdict, part: &str, command: &str) {
    let why = refusal(verdict);
    assert!(why.contains(part), "{command}: {why}");
}

// Each was confirmed live getting past the profile's prefix rules, or is a
// spelling git reads as the same push.
#[test]
fn a_push_to_the_base_branch_is_refused_in_every_shape() {
    for command in [
        "git push origin HEAD:main",
        "git -C . push origin HEAD:main",
        "/usr/bin/git push origin HEAD:main",
        "git --no-pager push origin HEAD:main",
        "env git push origin +HEAD:refs/heads/main",
        "git push origin HEAD:heads/main",
        "git push -u origin kelpie/7:main",
        "git push --repo=origin origin HEAD:main",
        "git push origin main",
        "git push origin refs/heads/main",
        "git push origin :main",
        "git push origin -- HEAD:main",
        "bash -c 'git push origin HEAD:main'",
        "cd /tmp && git push origin HEAD:main",
    ] {
        refused_with(anywhere(command), TO_BASE, command);
    }
}

#[test]
fn a_push_of_head_from_the_base_branch_is_refused() {
    let tree = WorkerTree::new();
    tree.git(&["switch", "--quiet", "--ignore-other-worktrees", "main"]);
    for command in [
        "git push origin HEAD",
        "git push origin @",
        "git push origin",
        "git push",
    ] {
        refused_with(tree.bash(command), TO_BASE, command);
    }
}

#[test]
fn a_push_of_the_workers_own_branch_goes_through() {
    let tree = WorkerTree::new();
    for command in [
        "git push origin HEAD",
        "git push origin HEAD:kelpie/7",
        "git push origin HEAD:mainline",
        "git push origin HEAD:refs/heads/main-notes",
        "git push",
    ] {
        assert_eq!(tree.bash(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn what_only_the_project_manager_does_in_gh_is_refused_in_every_shape() {
    for command in [
        "gh pr merge 1",
        "gh -R o/r pr merge 1",
        "gh pr -R o/r merge",
        "/opt/homebrew/bin/gh pr merge 1 -R o/r",
        "env GH_PAGER= gh pr ready 3",
        "gh --repo o/r pr ready 1",
        "gh pr ready --undo",
        "gh pr edit 3 --add-label 'review please'",
        "gh pr edit 3 --add-label='bug,Review  Please'",
        "gh issue edit 3 -l 'review please'",
        "gh pr create --draft --title 'fix: x' --label 'review please'",
    ] {
        refused_with(anywhere(command), MANAGER, command);
    }
    for (command, part) in [
        ("gh api", "gh api"),
        ("gh api repos/o/r/pulls/1/merge -X PUT", "gh api"),
        ("/opt/homebrew/bin/gh api --hostname h -X POST /x", "gh api"),
        ("gh --repo o/r api graphql -f query=x", "gh api"),
        ("gh auth token", "gh auth"),
        ("gh -R o/r auth status", "gh auth"),
        ("gh co 3", "not one"),
        ("gh ext exec merge", "alias or extension"),
        ("gh alias set m 'pr merge'", "alias or extension"),
    ] {
        refused_with(anywhere(command), part, command);
    }
}

#[test]
fn gh_a_worker_runs_goes_through() {
    for command in [
        "gh pr view 3",
        "gh pr checks 3 --watch",
        "gh pr edit 3 --add-label bug",
        "gh pr create --draft --title 'fix: x' --body y",
        "gh issue view 5 --comments",
        "gh --version",
        "gh help pr",
        "which gh",
    ] {
        assert_eq!(anywhere(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn git_or_gh_run_by_another_program_is_refused() {
    for command in [
        "find . -exec gh pr merge 1 ;",
        "caffeinate -i git push origin HEAD:main",
        "flock /tmp/l /usr/bin/git push origin HEAD",
    ] {
        refused_with(anywhere(command), "run through", command);
    }
    for command in [
        "G=gh; $G pr merge 1",
        "$(echo gh) pr merge 1",
        "`echo git` push",
    ] {
        refused_with(anywhere(command), "works out when it runs", command);
    }
}

// A folder holding one script, and a call run from it.
struct Scripts(TempDir);

impl Scripts {
    fn with(file: &str, text: &str) -> Self {
        let dir = Self(tempfile::tempdir().unwrap());
        dir.write(file, text);
        dir
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.0.path().join(file);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn bash(&self, command: &str) -> Verdict {
        bash_in(self.0.path(), command)
    }
}

#[test]
fn a_script_file_is_read_for_its_commands() {
    let dir = Scripts::with(
        "push.sh",
        "#!/usr/bin/env bash\ngit push origin HEAD:main\n",
    );
    for command in [
        "bash push.sh",
        "sh -e ./push.sh",
        "source push.sh",
        ". ./push.sh",
        "./push.sh",
        "timeout 5 ./push.sh",
    ] {
        refused_with(dir.bash(command), TO_BASE, command);
    }
    let dir = Scripts::with("merge", "gh pr merge 1\n");
    refused_with(dir.bash("./merge"), MANAGER, "a script with no #! line");
    let dir = Scripts::with("outer.sh", "bash inner.sh\n");
    dir.write("inner.sh", "gh pr ready 1\n");
    refused_with(
        dir.bash("bash outer.sh"),
        MANAGER,
        "a script run by a script",
    );
}

#[test]
fn a_script_a_worker_runs_goes_through() {
    let dir = Scripts::with("check.sh", "#!/bin/sh\ncargo test\ngit status\n");
    for command in [
        "bash check.sh",
        "./check.sh",
        "bash -c 'echo hi'",
        "bash <<'EOF'\necho hi\nEOF",
    ] {
        assert_eq!(dir.bash(command), Verdict::Allow, "{command}");
    }
    dir.write(
        "tool.py",
        "#!/usr/bin/env python3\nprint('gh pr merge 1')\n",
    );
    assert_eq!(
        dir.bash("./tool.py"),
        Verdict::Allow,
        "not a shell's script"
    );
}

#[test]
fn a_script_the_guard_cannot_read_is_refused() {
    let dir = Scripts::with("push.sh", "git push origin HEAD:main\n");
    for command in [
        "echo 'gh pr merge 1' > push.sh && bash push.sh",
        "cp other.sh push.sh; ./push.sh",
    ] {
        refused_with(dir.bash(command), "name it twice", command);
    }
    refused_with(
        dir.bash("bash missing.sh"),
        "cannot read the script",
        "missing",
    );
    for command in [
        "cat push.sh | bash",
        "bash < push.sh",
        "bash -s < push.sh",
        "bash <<< 'git push origin HEAD:main'",
    ] {
        refused_with(dir.bash(command), "reads from a pipe", command);
    }
    let heredoc = "bash <<'EOF'\ngit push origin HEAD:main\nEOF";
    refused_with(dir.bash(heredoc), TO_BASE, heredoc);
    dir.write("fish.sh", "#!/usr/bin/env fish\ngit push\n");
    refused_with(dir.bash("./fish.sh"), "`fish` script", "a fish script");
}
