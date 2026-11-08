//! A merge ruling the gate raised with a warning on it, which only the
//! maintainer's yes passes: the project manager's yes goes back to the
//! maintainer with its pick, except on a ruling warning of rivals

use std::sync::Mutex;

use super::{answered, answers, asking, comments, left_to_pm, on_merge, pending, settled};
use crate::ports::Role;
use crate::runner::{CHECKS_SETTLE, Runner, step};
use crate::state::{Ruling, RulingKind};
use crate::test::{Rig, Scripted};

/// [`left_to_pm`], with ruling 71 warning as `warn` sets it
fn warned(project: &str, warn: impl FnOnce(&mut Ruling)) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = left_to_pm(project);
    {
        let mut runner = runner.lock().unwrap();
        let ruling = &mut runner.state.rulings[0];
        warn(ruling);
    }
    (rig, runner)
}

/// Has the project manager answer ruling 71 with a yes, and says what the
/// maintainer then sees: the card holds its pick, the ruling waits, and
/// nothing merged
fn yes_goes_to_the_maintainer(rig: &Rig, runner: &Mutex<Runner>) {
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);

    settled(rig, runner);
    for _ in 0..3 {
        step(runner).unwrap();
    }

    assert!(pending(rig), "the maintainer answers");
    assert_eq!(answered(rig), None, "{:?}", comments(rig));
    let card = asking(rig).expect("the ruling is posted for the maintainer");
    assert!(
        card.contains("**Project manager: Merge**. PM-WHY"),
        "{card}"
    );
    assert_eq!(rig.forge.merges(), [], "nothing was merged");
}

#[test]
fn a_yes_on_an_unread_head_goes_to_the_maintainer_and_merges_nothing() {
    let (rig, runner) = warned("pm-unread", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge { unread_head, .. } = k {
                *unread_head = true;
            }
        });
    });

    yes_goes_to_the_maintainer(&rig, &runner);
}

#[test]
fn a_yes_on_an_unreviewed_pass_goes_to_the_maintainer_and_merges_nothing() {
    let (rig, runner) = warned("pm-unreviewed", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge { unreviewed, .. } = k {
                *unreviewed = Some("the reviewer's turn failed".into());
            }
        });
    });

    yes_goes_to_the_maintainer(&rig, &runner);
}

#[test]
fn a_yes_past_open_bot_threads_goes_to_the_maintainer_and_merges_nothing() {
    let (rig, runner) = warned("pm-threads", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge {
                open_threads,
                held_by,
                ..
            } = k
            {
                *open_threads = Some("cubic left 1 P1 thread open".into());
                *held_by = vec!["thread-1".into()];
            }
        });
    });

    yes_goes_to_the_maintainer(&rig, &runner);
}

#[test]
fn a_yes_on_an_unread_fix_goes_to_the_maintainer_and_merges_nothing() {
    let (rig, runner) = warned("pm-note-fix", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge { note_fix, .. } = k {
                *note_fix = true;
            }
        });
    });
    yes_goes_to_the_maintainer(&rig, &runner);

    let (rig, runner) = warned("pm-late-fix", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge { late_fix, .. } = k {
                *late_fix = true;
            }
        });
    });
    yes_goes_to_the_maintainer(&rig, &runner);
}

#[test]
fn a_yes_on_a_borrowed_head_goes_to_the_maintainer_and_merges_nothing() {
    let (rig, runner) = warned("pm-borrowed", |r| r.borrowed = true);

    yes_goes_to_the_maintainer(&rig, &runner);
}

#[test]
fn a_send_back_on_a_warned_head_sends_the_worker_to_a_new_pass() {
    let (rig, runner) = warned("pm-send-back", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge { unread_head, .. } = k {
                *unread_head = true;
            }
        });
    });
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('B', false))]);
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich?\n</kelpie-question>\n",
    )]);

    settled(&rig, &runner);
    step(&runner).unwrap();

    assert!(answered(&rig).is_some(), "{:?}", comments(&rig));
    assert_eq!(asking(&rig), None, "the maintainer was never asked");
    let again = "the whole review reads your change again";
    assert!(
        rig.claude.calls().iter().any(|c| c.prompt.contains(again)),
        "the worker was never sent back to a new pass"
    );
}

#[test]
fn a_yes_on_a_rival_ruling_merges() {
    let (rig, runner) = warned("pm-rivals", |r| {
        on_merge(r, |k| {
            if let RulingKind::Merge { rivals, .. } = k {
                *rivals = vec![72];
            }
        });
    });
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);

    settled(&rig, &runner);
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();

    assert_eq!(asking(&rig), None, "the maintainer was never asked");
    assert!(answered(&rig).is_some(), "{:?}", comments(&rig));
    assert_eq!(rig.forge.merges().len(), 1, "the yes merged it");
}
