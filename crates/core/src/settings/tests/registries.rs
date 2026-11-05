//! `worker.package_registries`, which is gone: detection is always on

use super::{EXAMPLE, parse_err};

#[test]
fn the_old_key_is_refused_by_name() {
    let text = EXAMPLE.replace(
        "build_env = {}\n",
        "build_env = {}\npackage_registries = true\n",
    );
    assert!(
        text.contains("package_registries = true"),
        "the example's worker table moved"
    );

    let err = parse_err(&text);

    assert!(
        err.contains("`worker.package_registries` is no longer a setting"),
        "{err}"
    );
    assert!(err.contains("delete it"), "{err}");
}
