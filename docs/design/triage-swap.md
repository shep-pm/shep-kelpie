# The waiting label's one-time swap

Kelpie's waiting label moved from `needs-triage` to `needs-approval` (#1851, #1855). The settings and the pick rule are in `triage.md`, the issue check in `triage-check.md`.

- `board::TRIAGE` is `needs-approval`, the label of an issue waiting on the maintainer's approval. `board::TRIAGE_GO` is `needs-triage`, a person's ask for a triage: it is outside `TRIAGE_LABELS`, so it never reads as a decision. `crates/core/src/board/mod.rs`
- `Runner::swap_labels` runs in `settle_on_forge` ahead of the other label calls, for every GitHub project whatever `pick` says, since follow-ups and dependency issues use the label too. A local store swaps nothing. One `repo_labels` read tells the case, and the repo's labels are the marker, so no state field records it and `pick = off` clearing `state.triage` loses nothing:
  - both labels there: done, no write. A repo with a hand-made `needs-approval` beside `needs-triage` counts as swapped.
  - `needs-triage` only: the saved `Triaged.seen` and the in-memory `triage_tries` seen are mapped first and saved, so `stood` does not read the renamed label as one put on by hand. Then `rename_label`, then `needs-triage` is made new. Mapping again changes nothing.
  - `needs-approval` only: a swap cut short, so `needs-triage` is made.
  - neither: no write, as each is made when first used.
- A swap that fails, a `repo_labels` read included, sets `Run::label_swap_owed`, in memory. `apply_poll` retries it at its top, before the listing is read. While owed, `triage_due` starts no pass, `apply_poll` resumes no saved pass and takes nothing back (their refusal would count as the issue's failure, and a given-up issue is not tried again until a restart), and `Writer::make_missing_labels` refuses `needs-approval` for every maker, follow-ups and `file` in `dependencies/acts.rs` included, since a label made early would read as a finished swap. `Runner::writer` copies the flag to the `Writer`.
- `check::key` skips triage labels, so the rename reruns no check.
- Rollback is one way: an older kelpie finds both labels and reads `needs-approval` as no label of its own.
- `runner/triage/swap.rs`, `runner/triage/{mod,poll_read}.rs`, `runner/voice/writer.rs`
