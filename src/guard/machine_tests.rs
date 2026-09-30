//! What the guard refuses of this machine's, each the way the forge port refuses it
//!
//! Every path here is synthetic: no fixture names a real home.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::json;

use super::{Checkout, judge};
use crate::confine::Verdict;

const HOME: &str = "/Users/me";

fn judged(command: &str, home: &str, names: &[&str]) -> Verdict {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    let call = json!({ "tool_name": "Bash", "cwd": "/x", "tool_input": { "command": command } });
    let checkout = Checkout {
        git_common_dir: Path::new("/nowhere"),
        worktree: Path::new("/nowhere"),
    };
    judge(
        call.to_string().as_bytes(),
        Some(Path::new(home)),
        &names,
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
        "in /home/alex/app/src",
        "in /private/tmp/kelpie-1/x",
        "in /var/folders/zz/abc123/T/out.log",
        "in /private/var/folders/zz/abc123/T/out.log",
    ] {
        let why = refusal(judged(&comment(text), HOME, &[]));
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
    let why = refusal(judged(&command, link.to_str().unwrap(), &[]));
    assert!(why.contains("a path on this machine"), "{why}");
}

#[test]
fn a_home_path_a_lan_address_and_a_private_name_are_refused_in_prose() {
    for (text, what) in [
        ("see ~/.ssh/config", "a path under the home folder"),
        (LAN_URL, "an address on a local network"),
        ("ssh alex@mac.local", "an address on a local network"),
        (
            "for Acme Corp only",
            "a name on this project's private list",
        ),
    ] {
        for command in [
            comment(text),
            format!("gh issue create --title 'fix: x' --body '{text}'"),
            format!("git commit -m 'docs: {text}'"),
            format!("gh pr create --title 'fix: {text}' --body ok"),
        ] {
            let why = refusal(judged(&command, HOME, &["acme corp"]));
            assert!(why.contains(what), "{command}: {why}");
            assert!(!why.contains(text), "{command}: {why}");
        }
    }
}

#[test]
fn ordinary_text_goes_through() {
    for text in [
        "fixes the page at src/home/mod.rs",
        LOOPBACK_URL,
        "see https://example.com/home/page",
        "the acme of it",
    ] {
        assert_eq!(
            judged(&comment(text), HOME, &["acme corp"]),
            Verdict::Allow,
            "{text}"
        );
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(dir).args(args).status();
    assert!(status.unwrap().success(), "git {args:?}");
}

// A worktree with one staged file, judged for a commit of it.
fn commit_of(text: &str, names: &[&str]) -> Verdict {
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
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
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
        &names,
        checkout,
    )
}

#[test]
fn a_commit_adding_an_encoded_or_system_path_is_refused_naming_the_file() {
    for line in [
        "at /%2FUsers%2Fme%2Fapp",
        r"C:\Users\alex\app",
        "/private/tmp/kelpie-1/x",
        "/var/folders/zz/abc123/T/x",
        "/home/alex/x",
    ] {
        let why = refusal(commit_of(&format!("{line}\n"), &[]));
        assert!(why.contains("`notes.md`"), "{line}: {why}");
    }
}

#[test]
fn a_commit_adding_a_private_name_is_refused() {
    let why = refusal(commit_of("for Acme Corp\n", &["acme corp"]));
    assert!(
        why.contains("a name on this project's private list"),
        "{why}"
    );
    assert!(why.contains("`notes.md`"), "{why}");
}

#[test]
fn a_commit_adding_a_home_path_or_an_address_is_not_refused() {
    let text = "docs say ~/.kelpie, a fixture uses an address, and self.local is a field\n";
    assert_eq!(commit_of(text, &[]), Verdict::Allow);
}
