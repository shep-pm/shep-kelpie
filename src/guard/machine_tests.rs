//! What the guard refuses of this machine's, each the way the forge port refuses it
//!
//! Every path here is synthetic: no fixture names a real home.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::json;

use super::{Checkout, judge, local_paths};
use crate::confine::Verdict;
use crate::local_paths::LocalPaths;

const HOME: &str = "/Users/me";

fn judged(command: &str, home: &str) -> Verdict {
    let call = json!({ "tool_name": "Bash", "cwd": "/x", "tool_input": { "command": command } });
    let checkout = Checkout {
        git_common_dir: Path::new("/nowhere"),
        worktree: Path::new("/nowhere"),
    };
    judge(
        call.to_string().as_bytes(),
        Some(Path::new(home)),
        LocalPaths::new([Path::new(home)]),
        checkout,
    )
}

fn refusal(verdict: Verdict) -> String {
    match verdict {
        Verdict::Refuse(why) => why,
        Verdict::Allow => panic!("allowed"),
    }
}

// Put together here, so no fixture holds a dotted address.
const LAN_URL: &str = concat!("open http://192.", "168.1.20:3000");
const LOOPBACK_URL: &str = concat!("served at http://127.", "0.0.1:5173");

fn comment(text: &str) -> String {
    format!("gh pr comment 3 --body '{text}'")
}

#[test]
fn every_encoding_of_a_local_path_is_refused_in_a_comment() {
    for text in [
        "see /Users/me/.kelpie/wt/koji/7/src/a.rs",
        "at http://localhost:5173/%2FUsers%2Fme%2Fapp%2Fsrc%2Fmain.ts",
        "at /%2fusers%2fme/app",
        "at /%252FUsers%252Fme%252Fapp",
        r"built in C:\Users\alex\app",
        "built in c:/users/alex/app",
        concat!("in /ho", "me/alex/app/src"),
        "in /private/tmp/kelpie-1/x",
        "in /var/folders/zz/abc123/T/out.log",
        "in /private/var/folders/zz/abc123/T/out.log",
    ] {
        let why = refusal(judged(&comment(text), HOME));
        assert!(why.contains("a path on this machine"), "{text}: {why}");
        assert!(!why.contains(text), "{text}: {why}");
    }
}

#[test]
fn a_symlinked_home_is_refused_in_its_canonical_form() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real-home");
    fs::create_dir(&real).unwrap();
    let link = dir.path().join("link-home");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let canonical = fs::canonicalize(&real).unwrap();
    let command = comment(&format!("built in {}/wt", canonical.display()));
    let why = refusal(judged(&command, link.to_str().unwrap()));
    assert!(why.contains("a path on this machine"), "{why}");
}

#[test]
fn a_home_path_and_a_lan_address_are_refused_in_prose() {
    for (text, what) in [
        ("see ~/.ssh/config", "a path under the home folder"),
        (LAN_URL, "an address on a local network"),
        ("ssh alex@mac.local", "an address on a local network"),
    ] {
        for command in [
            comment(text),
            format!("gh issue create --title 'fix: x' --body '{text}'"),
            format!("git commit -m 'docs: {text}'"),
            format!("gh pr create --title 'fix: {text}' --body ok"),
        ] {
            let why = refusal(judged(&command, HOME));
            assert!(why.contains(what), "{command}: {why}");
            assert!(!why.contains(text), "{command}: {why}");
        }
    }
}

#[test]
fn ordinary_text_goes_through() {
    for text in [
        concat!("fixes the page at src/ho", "me/mod.rs"),
        LOOPBACK_URL,
        concat!("see https://example.com/ho", "me/page"),
        "the acme of it",
    ] {
        assert_eq!(judged(&comment(text), HOME), Verdict::Allow, "{text}");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(dir).args(args).status();
    assert!(status.unwrap().success(), "git {args:?}");
}

// A worktree with one staged file, judged for a commit of it.
fn commit_of(text: &str) -> Verdict {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet"]);
    fs::write(repo.join("README.md"), "a project\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "commit",
            "--quiet",
            "-m",
            "first",
        ],
    );
    git(
        &repo,
        &["worktree", "add", "--quiet", "-b", "kelpie/7", "../wt"],
    );
    let wt = dir.path().join("wt");
    fs::write(wt.join("notes.md"), text).unwrap();
    git(&wt, &["add", "notes.md"]);
    let call = json!({ "tool_name": "Bash", "cwd": wt, "tool_input": {
        "command": "git commit -m 'docs: notes'",
    } });
    let common = repo.join(".git");
    let checkout = Checkout {
        git_common_dir: &common,
        worktree: &wt,
    };
    judge(
        call.to_string().as_bytes(),
        Some(Path::new(HOME)),
        LocalPaths::new([Path::new(HOME)]),
        checkout,
    )
}

#[test]
fn a_commit_adding_an_encoded_home_is_refused_naming_the_file() {
    for line in ["at /%2FUsers%2Fme%2Fapp", "at /%252FUsers%252Fme%252Fapp"] {
        let why = refusal(commit_of(&format!("{line}\n")));
        assert!(why.contains("`notes.md`"), "{line}: {why}");
    }
}

// A repo's fixtures name other people's homes all the time, and a worker
// reflowing one is not leaking anything.
#[test]
fn a_commit_adding_another_users_path_goes_through_and_the_same_text_in_a_body_does_not() {
    for line in [
        r"C:\Users\alex\app",
        "/private/tmp/kelpie-1/x",
        "/var/folders/zz/abc123/T/x",
        concat!("/ho", "me/alex/x"),
    ] {
        assert_eq!(commit_of(&format!("{line}\n")), Verdict::Allow, "{line}");
        let why = refusal(judged(&comment(line), HOME));
        assert!(why.contains("a path on this machine"), "{line}: {why}");
    }
}

#[test]
fn the_folders_the_hook_is_given_are_kept_off_the_forge_too() {
    let args = ["--folder=/srv/kelpie", "--folder=/srv/checkout"].map(str::to_owned);
    let local = local_paths(Some(Path::new(HOME)), &args).unwrap();
    let call = |text: &str| {
        let command = comment(text);
        let call =
            json!({ "tool_name": "Bash", "cwd": "/x", "tool_input": { "command": command } });
        let checkout = Checkout {
            git_common_dir: Path::new("/nowhere"),
            worktree: Path::new("/nowhere"),
        };
        judge(
            call.to_string().as_bytes(),
            Some(Path::new(HOME)),
            local.clone(),
            checkout,
        )
    };
    for text in [
        "see /srv/kelpie/wt/koji/7",
        "see /srv/checkout/src",
        "see /Users/me/x",
    ] {
        let why = refusal(call(text));
        assert!(why.contains("a path on this machine"), "{text}: {why}");
    }
    assert_eq!(call("see /srv/kelpies/x"), Verdict::Allow);
    for arg in ["--folder", "--name=Acme Corp"] {
        let err = local_paths(None, &[arg.to_owned()]).unwrap_err();
        assert!(err.contains("does not take"), "{err}");
    }
}

#[test]
fn a_refusal_says_what_to_fix() {
    let said = |text: &str| refusal(judged(&comment(text), HOME));
    for text in ["see /Users/me/x", "see ~/.ssh/config"] {
        assert!(said(text).contains("from its root"), "{text}");
    }
    let why = said(LAN_URL);
    assert!(why.contains("Take it out"), "{why}");
    assert!(!why.contains("from its root"), "{why}");
}

#[test]
fn a_commit_adding_a_word_the_project_once_kept_private_goes_through() {
    assert_eq!(commit_of("for Acme Corp\n"), Verdict::Allow);
}

#[test]
fn a_commit_adding_a_home_path_or_an_address_is_not_refused() {
    let text = "docs say ~/.kelpie, a fixture uses an address, and self.local is a field\n";
    assert_eq!(commit_of(text), Verdict::Allow);
}
