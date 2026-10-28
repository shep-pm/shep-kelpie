# Design

Kelpie: the maintainer's dog; one runner per project builds ready issues. Asks only via rulings.

## Always

- Gates, locks, rate windows live in code; Claude only for work, judgement. ADR 0001
- Kelpie controls each agent's context growth. `runner/turn/`
- Naming: dog = adopted `kelpie` (lease dog); project = sheep; agent calls = lambs. One kelpie for all projects. ADR 0001 to 0004
- Binary is the dog with no args + `SHEP_DOG_NAME` and `SHEP_NAME` set; needs the `shep adopt` channel; names itself in the handshake, never `silent`. `src/sheep/mod.rs`
- Needs shep 0.17.0+ (ADR 0016); `upgrade` refuses builds it would. shep-pm/shep#688, #319, #656, #914
- Home: `$SHEP_HOME/kelpie` (`KELPIE_HOME`), shep plugin, folder per project; the dog's files stay under `$SHEP_HOME`. #253
- The shepherd is reached through the `Dial` port (`core/shepherd/connection.rs`): `Socket` is the real one; tests dial `FakeShepherd`. #587
- Socket path over 103 bytes: no start. #1147
- Home never moved from `~/.kelpie`: no start, names each old file, exit 78 (`stop_exit_codes`). #311, #345
- State file version 92 (`VERSION` in `core/state/mod.rs`); each change one way: rollback = finish or drop open items. `core/state/removed/mod.rs`
- Business code is Rust; Tauri GUI after MVP. #5
- A runner serves the typed protocol on `run/<project>.sock`; verbs fall back to `shep trigger`. ADR 0017. runner-socket.md
- Crates under `crates/` use nothing above them; the root holds the binary, verbs (`src/{flock,upgrade,sheep,doctor,issues}/`, `src/schema.rs`). ADR 0015
- No budget numbers in agent prompts; no worker merges/posts as the maintainer (fence.md, github-app.md).

## Files

Each area has an index file whose table names its files and the code each covers. Read the index for the code you touch, then the files its rows name.

| index | covers |
|---|---|
| files-work.md | work items, the board, the project manager, the runner loop, its lock and socket, handover |
| files-agents.md | agent files, harnesses, skills, plugins, the fence, the browser, Windows |
| files-review.md | review, CI, merge, the App, others' pull requests, the representative, bot bumps |
| files-triage.md | the triager, its pass, the author's round, the check on every issue |
| files-metrics.md | pacing, waits, spend, log lines, metrics |

Each file is under 8 KB; over it, split, and add a row to its index. A new area gets an index and a row here. A changed decision updates its bullet.

Elsewhere: `docs/design-log.md` (history, old formats), `docs/facts/`, `docs/adr/`, `GLOSSARY.md`.
