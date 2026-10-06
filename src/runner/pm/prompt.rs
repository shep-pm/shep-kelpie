//! What a wake says to the project manager: why it was woken, where to read,
//! and the answer kelpie takes

use std::fmt::Write as _;

use super::Wake;
use crate::ports::PM_NOTES;

/// The answer kelpie reads, as the first wake of a session shows it
const ANSWER: &str = "\
```
{\"pick\": <the ready issue to start next, or null to start nothing now>,
 \"hold\": [<every ready issue that should wait until an open work item closes; [] holds none>],
 \"unstick\": null or {\"item\": <a stuck work item's issue>, \"action\": \"retry\" | \"re-scope\" | \"ask\" | \"none\", \"why\": \"<one sentence>\"},
 \"reply\": null or \"<a message to the maintainer, if one is due>\",
 \"why\": \"<two sentences at most>\"}
```

`retry` runs the item's turn again as it is. `re-scope` and `ask` put it to \
the maintainer, with your `why` as what you propose or ask. `none` leaves it to \
resolve itself. Kelpie checks every answer against the board, and drops and \
logs any part that names an issue the board does not show as ready, or an item \
that is not stuck.";

/// The prompt for a wake for `wakes`, carrying `told`: in full for a new
/// session, shorter for one resumed
pub(super) fn wake(project: &str, wakes: &[Wake], told: &[String], fresh: bool) -> String {
    let mut out = String::new();
    match fresh {
        true => {
            let _ = write!(
                out,
                "You are the project manager for {project}. Kelpie woke you because:\n\n"
            );
        }
        false => out.push_str("Kelpie woke you again because:\n\n"),
    }
    for wake in wakes {
        match wake {
            Wake::Told => {}
            other => {
                let _ = writeln!(out, "- {}", reason(other));
            }
        }
    }
    for note in told {
        out.push_str("- the maintainer told you:\n\n");
        for line in note.lines() {
            let _ = writeln!(out, "  > {line}");
        }
        out.push('\n');
    }
    match fresh {
        true => {
            let _ = write!(
                out,
                "\nRead `board.md`, which kelpie wrote as the board stood when it woke you, \
                 and `{PM_NOTES}`, what you wrote on earlier wakes. Add to the end of \
                 `{PM_NOTES}` anything a later wake must remember, above all what the \
                 maintainer told you.\n\nThen reply with one JSON object and nothing after \
                 it:\n\n{ANSWER}\n"
            );
        }
        false => {
            let _ = write!(
                out,
                "\n`board.md` has been written again for now. Reread what you need, and \
                 `{PM_NOTES}`, adding to its end what a later wake must remember. Then \
                 reply with the same JSON object as before and nothing after it.\n"
            );
        }
    }
    out
}

/// One reason for a wake, in words
pub(super) fn reason(wake: &Wake) -> String {
    match wake {
        Wake::Pick(ready) => format!(
            "a slot is free and {ready} ready issues could fill it: pick one, or none to \
             start nothing now"
        ),
        Wake::Conflict(a, b, files) => format!(
            "the branches of #{a} and #{b} conflict in {}",
            files.join(", ")
        ),
        Wake::Stuck(issue, what) => format!("#{issue} is stuck: {what}"),
        Wake::Told => "the maintainer told you something".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_wake_names_its_reasons_the_files_and_the_answer() {
        let wakes = [
            Wake::Pick(3),
            Wake::Conflict(7, 9, vec!["src/a.rs".into(), "src/b.rs".into()]),
            Wake::Told,
        ];
        let text = wake(
            "koji",
            &wakes,
            &["Hold #12 until\nthe release.".into()],
            true,
        );
        assert!(text.starts_with(
            "You are the project manager for koji. Kelpie woke you because:\n\n\
             - a slot is free and 3 ready issues could fill it: pick one, or none to start \
             nothing now\n\
             - the branches of #7 and #9 conflict in src/a.rs, src/b.rs\n\
             - the maintainer told you:\n\n  > Hold #12 until\n  > the release.\n\n"
        ));
        assert!(text.contains("`board.md`") && text.contains("`pm-notes.md`"));
        assert!(text.contains("\"unstick\": null or {\"item\""), "{text}");
    }

    #[test]
    fn a_resumed_wake_points_back_at_the_answer_it_already_has() {
        let text = wake(
            "koji",
            &[Wake::Stuck(7, "its turn failed".into())],
            &[],
            false,
        );
        assert!(
            text.starts_with("Kelpie woke you again because:\n\n- #7 is stuck: its turn failed\n")
        );
        assert!(text.contains("the same JSON object as before"));
        assert!(!text.contains("\"pick\""));
    }
}
