// Git and gh reached past the program at the front of a command: behind a
// wrapper, from a shell's stdin, by a built name, or run by git itself.

use super::*;

// A worktree whose one unpushed commit adds the home folder's path.
fn unpushed() -> WorkerTree {
    let tree = WorkerTree::new();
    tree.write("notes.md", "/home/me/x\n");
    tree.git(&["add", "notes.md"]);
    tree.git(&["commit", "--quiet", "-m", "docs: notes"]);
    tree
}

// Each command is refused for the push it runs, naming the file it sends.
fn sends_notes(tree: &WorkerTree, commands: &[&str]) {
    for command in commands {
        let why = refusal(tree.bash(command));
        assert!(why.contains("`notes.md`"), "{command}: {why}");
    }
}

fn refused(commands: &[&str]) {
    for command in commands {
        assert!(
            matches!(bash(command), Verdict::Refuse(_)),
            "{command} was let through"
        );
    }
}

fn allowed(commands: &[&str]) {
    for command in commands {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn git_and_gh_behind_a_wrapper_the_guard_knows_are_judged() {
    sends_notes(
        &unpushed(),
        &[
            "script -q /dev/null git push origin HEAD",
            "caffeinate -i git push origin HEAD",
            "stdbuf -o0 git push origin HEAD",
            "stdbuf --output=L git push origin HEAD",
            "sudo -u me git push origin HEAD",
            "doas git push origin HEAD",
        ],
    );
    refused(&[
        "script -q /dev/null gh pr create --title Parser",
        "caffeinate -i gh pr create --title Parser",
        "stdbuf -oL gh pr create --title Parser",
        "sudo gh pr create --title Parser",
        "doas -u me gh pr create --title Parser",
        "script -c 'git push' /dev/null",
    ]);
}

#[test]
fn git_and_gh_behind_a_wrapper_the_guard_does_not_know_are_judged() {
    let tree = unpushed();
    for command in [
        "unbuffer git push origin HEAD",
        "chrt -f 1 git push origin HEAD",
        "unbuffer bash -c 'git push origin HEAD'",
        "unbuffer sh <<< 'git push origin HEAD'",
    ] {
        assert!(
            matches!(tree.bash(command), Verdict::Refuse(_)),
            "{command} was let through"
        );
    }
    refused(&[
        "unbuffer gh pr create --title Parser",
        "flock /tmp/l gh pr new --title Parser",
        "unbuffer git -c core.fsmonitor=x status",
    ]);
}

#[test]
fn a_word_that_only_names_git_or_a_shell_is_not_refused() {
    allowed(&[
        "rg -n git src",
        "grep -rn git --include=*.rs .",
        "which git gh",
        "echo git log",
        "ls -l /bin/sh",
        "man bash",
        "cargo test git",
        "grep -c git README.md",
    ]);
}

#[test]
fn a_shell_given_a_string_on_stdin_runs_it() {
    sends_notes(
        &unpushed(),
        &[
            "sh <<< 'git push origin HEAD'",
            "bash <<<'git push origin HEAD'",
            "bash -s <<< \"git push origin HEAD\"",
            "zsh 0<<< 'git push origin HEAD'",
        ],
    );
    refused(&["sh <<< 'gh pr create --title Parser'"]);
    allowed(&["sh <<< 'cargo test'", "bash README.md <<< input"]);
}

#[test]
fn a_shell_reading_its_commands_from_a_pipe_or_file_is_refused() {
    refused(&[
        "echo 'git push origin HEAD' | sh",
        "echo 'gh pr create --title Parser' | bash",
        "bash -s < f.sh",
        "bash < f.sh",
        "cat f.sh | bash -s",
        "cat f.sh | bash -e -o pipefail",
        "bash",
        "bash <(printf x)",
        "ksh < f.sh",
        "echo x | fish",
        "bash /dev/stdin < f.sh",
        ". /dev/stdin < f.sh",
        "source <(printf x)",
        "echo 'git push' | sudo -s",
        "echo 'git push' | sudo -i -u me",
        "echo 'git push' | doas -s",
        "echo 'git push' | script -q /dev/null",
        "echo 'git push' | busybox sh",
        "su -c 'git push'",
        "echo HEAD | xargs -I{} sh -c 'git push origin {}; gh pr create --title Parser'",
    ]);
    allowed(&[
        "bash README.md",
        "bash --version",
        "sh -n README.md",
        "bash -c 'cargo test'",
        "bash README.md < input.txt",
        "ksh script.ksh",
        "source README.md",
        "sudo -u me cargo test",
        "busybox ls",
    ]);
}

#[test]
fn a_command_name_the_shell_works_out_is_refused() {
    refused(&[
        "$(echo git) push origin HEAD",
        "G=git; $G push origin HEAD",
        "`echo git` push origin HEAD",
        "\"$G\" push origin HEAD",
        "${G} push",
        "$(echo gh) pr create --title Parser",
        "G=gh; $G pr create --title Parser",
        "sudo $G push",
        "S='git push'; bash -c \"$S\"",
        "S='git push'; sh <<< \"$S\"",
    ]);
    allowed(&["echo $HOME", "cargo test -- $X", "A=$(pwd) cargo test"]);
}

#[test]
fn text_git_runs_as_a_command_is_read_as_a_script() {
    sends_notes(
        &unpushed(),
        &[
            "git rebase --exec 'git push origin HEAD' HEAD~1",
            "git rebase -x 'git push origin HEAD' HEAD~1",
            "git rebase --exe='git push origin HEAD' HEAD~1",
            "git difftool -y -x 'git push origin HEAD; true' HEAD~1",
            "git difftool --extcmd='git push origin HEAD' HEAD~1",
            "git bisect run git push origin HEAD",
            "git bisect run sh -c 'git push origin HEAD'",
            "git ls-remote --upload-pack='git push origin HEAD; git-upload-pack' .",
            "git fetch --upload-pack 'git push origin HEAD' .",
            "git clone -u 'git push origin HEAD' . x",
            "git archive --remote=. --exec='git push origin HEAD' HEAD",
            "GIT_PAGER='git push origin HEAD; cat' git -p log -1",
            "EDITOR='git push origin HEAD #' git commit --amend",
            "export GIT_SEQUENCE_EDITOR='git push origin HEAD'; git rebase -i HEAD~1",
        ],
    );
    refused(&["git bisect run $G push"]);
    allowed(&[
        "GIT_EDITOR=true git rebase -i HEAD~1",
        "GIT_SEQUENCE_EDITOR=\"sed -i 's/pick/fixup/'\" git rebase -i HEAD~2",
        "GIT_PAGER=cat git log",
        "git rebase --exec 'cargo test' main",
        "git bisect run cargo test",
    ]);
}

#[test]
fn git_config_set_in_the_command_is_read_from_a_known_list() {
    refused(&[
        "git -c core.fsmonitor='git push; false' status",
        "git -c core.hooksPath=hooks checkout -b x",
        "git -c Core.FSMonitor=x status",
        "git --config-env=core.sshCommand=X fetch",
        "git --config-env core.pager=X log",
        "git -c include.path=f log",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0='git push' git status",
        "GIT_CONFIG_PARAMETERS=\"'core.fsmonitor'='git push'\" git status",
        "export GIT_CONFIG_COUNT=1; git status",
        "env GIT_CONFIG_COUNT=1 git status",
        "GIT_ALLOW_PROTOCOL=ext git ls-remote 'ext::sh -c git% push'",
    ]);
    allowed(&[
        "git -c color.ui=never log",
        "git -c core.quotepath=off status",
        "git -c advice.detachedHead=false switch --detach",
    ]);
}

#[test]
fn a_commit_or_push_after_git_makes_a_repo_in_the_same_call_is_refused() {
    let tree = WorkerTree::new();
    fs::create_dir(tree.path().join("docs")).unwrap();
    for command in [
        "cd docs && git init -q && git add . && git commit -m x && git push ../o.git HEAD",
        "cd docs && git clone -q ../../origin.git . && git push origin HEAD",
        "git worktree add docs && cd docs && git commit --allow-empty -m x",
    ] {
        let why = refusal(tree.bash(command));
        assert!(why.contains("makes a repo"), "{command}: {why}");
    }
}

#[test]
fn a_bare_repo_in_the_worktree_is_a_repo_of_its_own() {
    let tree = WorkerTree::new();
    tree.git(&["init", "--quiet", "--bare", "b"]);
    for command in [
        "git -C b push ../origin.git HEAD:refs/heads/x",
        "cd b && git push",
    ] {
        let why = refusal(tree.bash(command));
        assert!(
            why.contains("outside this worktree's own repo"),
            "{command}: {why}"
        );
    }
}

#[test]
fn a_git_variable_in_front_of_a_shell_reaches_its_script() {
    for command in [
        "GIT_DIR=../other.git bash -c 'git push origin HEAD'",
        "env GIT_WORK_TREE=x sh <<< 'git commit -m x'",
    ] {
        let why = refusal(bash(command));
        assert!(why.contains("own git"), "{command}: {why}");
    }
}

#[test]
fn push_config_that_sends_more_than_the_push_names_is_refused() {
    for (key, value) in [
        ("remote.origin.push", "refs/heads/*:refs/heads/*"),
        ("remote.origin.mirror", "true"),
        ("push.default", "matching"),
        ("push.followTags", "true"),
        ("push.recurseSubmodules", "on-demand"),
    ] {
        let tree = WorkerTree::new();
        tree.git(&["config", key, value]);
        let why = refusal(tree.bash("git push origin HEAD"));
        assert!(why.contains(&key.to_lowercase()), "{key}: {why}");
    }
    let tree = WorkerTree::new();
    tree.git(&["config", "push.default", "simple"]);
    tree.git(&["config", "push.followTags", "false"]);
    assert_eq!(tree.bash("git push origin HEAD"), Verdict::Allow);
}

// Every git a command names is judged, so a command naming many is capped.
#[test]
fn a_command_naming_git_many_times_is_refused_fast() {
    let line = format!("echo {}", "git commit -m x ".repeat(MAX_COMMANDS + 1));
    let started = std::time::Instant::now();
    let why = refusal(bash_in(Path::new("/"), &line));
    assert!(why.contains("too many commands"), "{why}");
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
}

#[test]
fn a_shell_fed_stdin_behind_a_wrapper_the_guard_does_not_know_is_refused() {
    refused(&[
        "echo 'git push origin HEAD' | unbuffer sh",
        "echo 'git push origin HEAD' | arch -arm64 bash",
        "unbuffer bash -s < f.sh",
        "echo 'git push' |& unbuffer zsh",
    ]);
    allowed(&[
        "ps aux | grep bash",
        "cat notes.md | grep -c sh",
        "man bash",
        "ls -l /bin/sh || unbuffer bash",
    ]);
}

#[test]
fn a_pager_git_grep_opens_files_in_is_read_as_a_script() {
    sends_notes(&unpushed(), &["git grep -O'git push origin HEAD' notes"]);
    allowed(&["git grep -O notes", "git grep -Oless notes"]);
}
