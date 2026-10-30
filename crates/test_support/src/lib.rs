//! Dev-only helpers for tests: temp folders, stand-in scripts and git
//!
//! Each one names nothing but std and `tempfile`, so a test in any crate can
//! use them. Uses no other member of the workspace, and is named only in
//! `[dev-dependencies]`.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

// The folder the helpers sit in, so a test's path reads `test::tempdir::tests::...`
// as it did before they moved here
mod test;

#[cfg(windows)]
pub use test::open_to_administrators;
#[cfg(unix)]
pub use test::signalled;
pub use test::{
    CALL_PATIENCE, Elsewhere, Fake, copy_dir, cut_from_main, exited, git, linked_worktree,
    main_commits, make_fifo, open_to_others, short_tempdir, waits_for, write_fake, write_in,
    write_script,
};
