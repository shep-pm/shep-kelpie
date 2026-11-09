//! `[app.dogs.kelpie.git]`'s `catch_up`, which says when a branch behind `main` is caught up

use super::{EXAMPLE, GIT, parse, parse_err};
use crate::settings::CatchUp;

// The example with `catch_up` set under the git table.
fn with(value: &str) -> String {
    EXAMPLE.replace(GIT, &format!("{GIT}catch_up = \"{value}\"\n"))
}

#[test]
fn a_missing_catch_up_is_always() {
    assert_eq!(parse(EXAMPLE).unwrap().git.catch_up, CatchUp::Always);
}

#[test]
fn catch_up_reads_always_and_conflicts() {
    assert_eq!(
        parse(&with("always")).unwrap().git.catch_up,
        CatchUp::Always
    );
    assert_eq!(
        parse(&with("conflicts")).unwrap().git.catch_up,
        CatchUp::Conflicts
    );
}

#[test]
fn catch_up_refuses_any_other_word() {
    for word in ["never", "Always", ""] {
        let err = parse_err(&with(word));
        assert!(err.contains("catch_up") || err.contains(word), "{err}");
        assert!(err.contains("always"), "names the choices: {err}");
    }
}
