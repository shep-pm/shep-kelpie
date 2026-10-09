//! What the forge port refuses, every path written as a dev server or a
//! platform writes it. Every path is synthetic.

use std::path::Path;

use super::*;
use crate::test::FakeForge;

fn slug() -> ForgeSlug {
    ForgeSlug::try_from("shep-pm/lab".to_owned()).unwrap()
}

fn guarded() -> (Guarded, FakeForge) {
    let fake = FakeForge::new("/nowhere".into());
    let local = LocalPaths::new([Path::new("/Users/me/.kelpie")]);
    (Guarded::new(Box::new(fake.clone()), local), fake)
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
        "see ~/.ssh/config",
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
        refused(forge.comment(&repo, 3, text));
        refused(forge.post_comment(&repo, 3, text).map(drop));
        refused(forge.create_issue(&repo, "a title", text, &[]).map(drop));
        refused(forge.create_issue(&repo, text, "a body", &[]).map(drop));
        assert_eq!(fake.comments(), [], "{text}");
        assert!(fake.created().is_empty(), "{text}");
    }
}

#[test]
fn a_refusal_names_the_field_and_what_it_found() {
    let (forge, _) = guarded();
    let refusal = |result: Result<u64, ForgeError>| result.unwrap_err().to_string();
    assert_eq!(
        refusal(forge.create_issue(&slug(), "a title", "see ~/notes", &[])),
        "not posted: the issue's body names a path under the home folder"
    );
    assert_eq!(
        refusal(forge.create_issue(&slug(), "at mac.local:80", "a body", &[])),
        "not posted: the issue's title names an address on a local network"
    );
    assert_eq!(
        refusal(forge.post_comment(&slug(), 3, "in /Users/me/.kelpie/x")),
        "not posted: the comment names a path on this machine, which names its user"
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
