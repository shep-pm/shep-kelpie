//! What the guard refuses to post, on either port, every path written as a
//! dev server or a platform writes it. Every path is synthetic.

use std::path::Path;

use super::*;
use crate::ports::Tickets;
use crate::test::FakeForge;

fn slug() -> ForgeSlug {
    ForgeSlug::try_from("shep-pm/lab".to_owned()).unwrap()
}

fn guarded() -> (Guarded, FakeForge) {
    let fake = FakeForge::new("/nowhere".into());
    let local = LocalPaths::new([Path::new("/Users/me/.kelpie")]);
    let source = Arc::new(fake.clone());
    (
        Guarded::new(
            Arc::clone(&source) as Arc<dyn Forge>,
            Arc::clone(&source) as Arc<dyn Tickets>,
            local,
        ),
        fake,
    )
}

#[test]
fn every_encoding_of_a_local_path_is_refused_at_every_post() {
    for text in [
        "at /Users/me/.kelpie/wt/koji/7/src/a.rs:3",
        "at /USERS/ME/.KELPIE/wt",
        "at http://localhost:5173/%2FUsers%2Fme%2F.kelpie%2Fwt%2Fsrc%2Fmain.ts",
        "at /%252FUsers%252Fme%252F.kelpie",
        r"built in C:\Users\alex\app",
        "built in c:/users/alex/app",
        concat!("in /ho", "me/alex/app/src"),
        "in /private/tmp/kelpie-1/x",
        "in /var/folders/zz/abc123/T/out.log",
        "in /private/var/folders/zz/abc123/T/out.log",
        concat!("open http://192.", "168.1.20:3000"),
        "ssh alex@mac.local",
    ] {
        let (forge, fake) = guarded();
        let repo = slug();
        let refused = |result: Result<(), ForgeError>| {
            assert!(
                matches!(result, Err(ForgeError::LocalPath { .. })),
                "{text}"
            );
        };
        refused(forge.comment(&repo, 3, text).map(drop));
        refused(forge.create_issue(&repo, "a title", text, &[]).map(drop));
        refused(forge.create_issue(&repo, text, "a body", &[]).map(drop));
        assert_eq!(fake.comments(), [], "{text}");
        assert!(fake.created().is_empty(), "{text}");
    }
}

#[test]
fn a_refusal_names_the_field_and_what_it_found() {
    let (forge, _) = guarded();
    fn refusal<T: core::fmt::Debug>(result: Result<T, ForgeError>) -> String {
        result.unwrap_err().to_string()
    }
    assert_eq!(
        refusal(forge.create_issue(&slug(), "a title", "see /private/tmp/x", &[])),
        "not posted: the issue's body names a path on this machine, which names its user"
    );
    assert_eq!(
        refusal(forge.create_issue(&slug(), "at mac.local:80", "a body", &[])),
        "not posted: the issue's title names an address on a local network"
    );
    assert_eq!(
        refusal(forge.comment(&slug(), 3, "in /Users/me/.kelpie/x")),
        "not posted: the comment names a path on this machine, which names its user"
    );
}

// A thread reply carries the worker's own words, so it is checked as any post is.
#[test]
fn a_reply_on_a_thread_naming_a_local_path_is_refused_and_an_ordinary_one_is_posted() {
    let (forge, fake) = guarded();
    fake.coderabbit.review(71, "abc", 1, &["Name the flag."]);
    let refused = forge.reply_to_thread(
        &slug(),
        "PRRT_71_0",
        "Declined: it lives in /Users/me/.kelpie/wt/koji/7/src/a.rs",
    );
    assert_eq!(
        refused.unwrap_err().to_string(),
        "not posted: the reply names a path on this machine, which names its user"
    );
    assert_eq!(fake.coderabbit.replies(), []);

    forge
        .reply_to_thread(&slug(), "PRRT_71_0", "Declined: the name is deliberate")
        .unwrap();
    assert_eq!(
        fake.coderabbit.replies(),
        [(
            "PRRT_71_0".to_owned(),
            "Declined: the name is deliberate".to_owned()
        )]
    );
}

#[test]
fn ordinary_text_is_posted() {
    let (forge, fake) = guarded();
    let text = concat!(
        "fixes src/ho",
        "me/mod.rs, served at http://127.",
        "0.0.1:5173"
    );
    forge.create_issue(&slug(), "a title", text, &[]).unwrap();
    let [issue] = fake.created().try_into().unwrap();
    assert_eq!(issue.body, text);
}

#[test]
fn a_path_under_the_home_folder_is_posted() {
    let (forge, fake) = guarded();
    let text = "the settings live in ~/.shep";
    forge.create_issue(&slug(), "a title", text, &[]).unwrap();
    let [issue] = fake.created().try_into().unwrap();
    assert_eq!(issue.body, text);
}

fn repo(slug: &str) -> ForgeSlug {
    ForgeSlug::try_from(slug.to_owned()).unwrap()
}

// A public destination, in a flock with a private project and a public one.
fn guarded_in_a_flock() -> (Guarded, FakeForge) {
    let (forge, fake) = guarded();
    fake.set_visibility_of(&repo("acme/vault"), Visibility::Private);
    let flock = vec![repo("shep-pm/lab"), repo("acme/vault"), repo("acme/site")];
    (forge.with_flock(flock), fake)
}

#[test]
fn the_name_of_a_private_project_is_refused_on_a_public_repo() {
    for text in [
        "ported from acme/vault",
        "ported from ACME/Vault.",
        "the vault project does it",
        "(vault)",
        "see https://github.com/acme/vault/pull/3",
        "git clone https://github.com/acme/vault.git",
        "git clone git@github.com:acme/vault.git.",
    ] {
        let (forge, fake) = guarded_in_a_flock();
        let refusal = forge.comment(&slug(), 3, text).unwrap_err();
        assert!(matches!(refusal, ForgeError::LocalPath { .. }), "{text}");
        assert_eq!(
            refusal.to_string(),
            "not posted: the comment names a repo GitHub marks private, and this repo is public"
        );
        assert_eq!(fake.comments(), [], "{text}");
    }
}

#[test]
fn the_name_of_a_private_project_is_refused_in_every_field_of_a_post() {
    let (forge, fake) = guarded_in_a_flock();
    let text = "ported from acme/vault";
    assert!(forge.create_issue(&slug(), text, "a body", &[]).is_err());
    assert!(forge.create_issue(&slug(), "a title", text, &[]).is_err());
    assert!(forge.reply_to_thread(&slug(), "PRRT_1_0", text).is_err());
    let open = |title, body| forge.create_pull_request(&slug(), "kelpie/1", "main", title, body);
    assert!(open(text, "a body").is_err());
    assert!(open("a title", text).is_err());
    assert!(fake.created().is_empty());
}

#[test]
fn the_name_of_a_private_project_is_posted_on_a_repo_that_is_private_too() {
    let (forge, fake) = guarded_in_a_flock();
    fake.set_visibility_of(&slug(), Visibility::Private);
    let text = "ported from acme/vault";
    forge.create_issue(&slug(), "a title", text, &[]).unwrap();
    assert_eq!(fake.created().len(), 1);
}

#[test]
fn kelpies_own_name_is_posted_though_a_private_project_shares_it() {
    let (forge, fake) = guarded();
    fake.set_visibility_of(&repo("acme/kelpie"), Visibility::Private);
    let forge = forge.with_flock(vec![repo("shep-pm/lab"), repo("acme/kelpie")]);
    let text = "kelpie planned the split again, and `shep kelpie rule 3 yes` tries it again";
    forge.create_issue(&slug(), "a title", text, &[]).unwrap();
    assert_eq!(fake.created().len(), 1);
    let full = "ported from acme/kelpie";
    assert!(forge.create_issue(&slug(), "a title", full, &[]).is_err());
    assert_eq!(fake.created().len(), 1);
}

#[test]
fn the_name_of_a_public_project_or_a_longer_word_is_posted() {
    for text in [
        "ported from acme/site",
        "the vaulted ceiling, vault-door and my_vault",
        "see vault.rs and the vault.example.com page",
        "ported from other/vault",
        "the vault/foo folder and /other/acme/vault",
        "ported from acme/vault.github and acme/vault.gitignore",
    ] {
        let (forge, fake) = guarded_in_a_flock();
        forge.create_issue(&slug(), "a title", text, &[]).unwrap();
        assert_eq!(fake.created().len(), 1, "{text}");
    }
}

// The destination's own name is what its posts say, whatever another owner's repo of
// the same name is.
#[test]
fn a_repo_sharing_the_destination_s_name_is_named_by_its_full_slug_only() {
    let (forge, fake) = guarded();
    fake.set_visibility_of(&repo("other/lab"), Visibility::Private);
    let forge = forge.with_flock(vec![repo("shep-pm/lab"), repo("other/lab")]);
    let post = |text| forge.create_issue(&slug(), "a title", text, &[]);
    post("the lab project, or lab.").unwrap();
    assert!(post("ported from other/lab").is_err());
}

#[test]
fn a_post_naming_a_flock_repo_is_refused_when_the_forge_cannot_say_who_sees_it() {
    let (forge, fake) = guarded_in_a_flock();
    fake.set_down(true);
    let refusal = forge.comment(&slug(), 3, "ported from acme/vault");
    assert!(matches!(refusal, Err(ForgeError::Failed(_))));
    fake.set_down(false);
    assert_eq!(fake.comments(), []);
}

#[test]
fn a_post_naming_no_flock_repo_never_asks_the_forge_who_sees_one() {
    let (forge, fake) = guarded_in_a_flock();
    let text = "a plain comment";
    forge.create_issue(&slug(), "a title", text, &[]).unwrap();
    assert_eq!(fake.calls(), 0);
}
