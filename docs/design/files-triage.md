# Design: Triage

Part of the design index, `README.md`. Read the rows for the code you touch, then those files.

| file | covers | touch (`core/`: `crates/core/src/`, `dog/`: `crates/dog/src/dog/`, `adapters/`: `crates/adapters/src/`, `runner/`: `crates/runner/src/runner/`) |
|---|---|---|
| triage.md | settings, triager, pick rule, `since`, the verb, `status` | `core/{settings/triage,ports/issue}.rs`, `runner/{triage,open_checks.rs}` |
| triage-limits.md | limits | `runner/triage/limits.rs` |
| triage-pass.md | a pass: post, labels, duplicate, agent-ready | `runner/{triage,flight}/`, `core/ports/voice.rs` |
| triage-priority.md | the priority a pass puts on, and the labels made | `runner/triage/{comment,facts}.rs`, `runner/claim.rs` |
| triage-images.md | an issue's images | `runner/triage/images.rs`, `adapters/gh/attachment.rs` |
| triage-author.md | author's round: ask, answers, silence | `runner/triage/{ask,heard}.rs` |
| triage-local.md | local session | `runner/triage/local.rs` |
| triage-spam.md | the spam check | `runner/triage/spam.rs` |
| triage-reader.md | who a comment is for | `runner/triage/facts.rs` |
| triage-hand.md | `hand_ready` take-back | `runner/triage/hand.rs` |
| triage-check.md | the check on every issue | `runner/triage/checks.rs` |
