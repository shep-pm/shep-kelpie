# Crates use only what is below them

This settles #1049, which supersedes #899. It reopens the one-crate layout CLAUDE.md stated and no ADR had decided.

The code was one crate of 55 top-level modules, and a module could reach any other. A reviewer had to remember which way each dependency pointed. The maintainer wants boundaries Cargo enforces: measured on main, 55 references in 21 module pairs pointed the wrong way for the map below, and the runner held 344 of the 949 files under `src/`.

The decision:

- The root `Cargo.toml` is a Cargo workspace, and a crate uses only what is below or beside it in the map. A module that reaches the wrong way fails to build.
- The map is flat on purpose. The leaves `process` (`spawn`, `terminal`, `tools`, `worktree`), `facts` (`mcp_use`, `request_use`, `tool_error`), `panel`, `worker_folder` and `trim` use no other member. `core` uses the leaves. `runner`, `dog` and `adapters` sit beside each other and use `core` and the leaves they need. The root package is the binary, the one crate that reaches every other and the composition root that builds the real adapters.
- `lease`, `usage`, `metrics`, `guard`, `profile`, `skills`, `board`, `state` and `hooks` stay in `core`. As siblings they would cost 13 to 42 cuts each, and in `core` they cost 0 to 3.
- A harness, a forge or a ticket source later joins as one more sibling that implements a `core` port trait.
- A crate's folder is `crates/<part>`, its package is the repo's name plus `-<part>`, and its modules keep their names, so test paths, `cargo test --lib <module path>` and the module names in the docs stay valid.
- Every member takes its version and edition from `[workspace.package]` and its lints from `[workspace.lints]`, so the build stamp and `version --json` print what they printed. No member is published.
- The test rig splits by the lowest crate that can hold each part. `crates/test_support` is a leaf used only as a dev-dependency: temp folders, stand-in scripts and git helpers, which name nothing but std and `tempfile`. The rest sits behind a `test-support` feature in the crate whose types it names.
- A crate's manifest names what its line in the map names and nothing else, and a dev-dependency never points at a crate that depends on the crate, which would build two copies of it. Nothing depends on the root, which is the only package with a binary.
- Each crate root re-exports what it uses from below under the old module names, so a move rewrites no `use` path. One later slice drops them, filed once the last move lands.
- Visibility widens and behaviour does not: an item another crate needs becomes `pub`, with the docs and `Debug` the lints require.

## The maintainer's answers

- Package names: a package named `adapters` clashes with the module `adapters` the root re-exports, so each takes the repo's name plus `-<part>`. Issues and commits name each crate by its folder, since the project's `command` hook refuses the repo's name in text bound for GitHub.
- The version: every member inherits it, so `Build::this()` and `version --json` are unchanged.
- The re-exports: each crate root keeps them until the later slice that drops them.
- Work in flight: kelpie keeps working these files, so a move starts only when no in-progress pull request touches its files, and open branches merge main after each.

## Considered options

- **Seven crates in layers (#899).** It needed 43 links cut and a guard crate before the first move. The flat map needs 27 cuts, and a later plugin joins as a sibling instead of a layer.
- **The root package as the test rig's home.** A dev-dependency cycle (crate X dev-depends on a crate that depends on X) builds two copies of X, so the rig's types would not match X's own.
- **One `test_support` crate that depends on `core`.** The same cycle, since `core`'s own tests use the rig.
- **Splitting `core` now.** It keeps loops (`ports` and `settings`, `ports` and `board`, `board` and `state`, `metrics` and `usage` among them). Splitting it is its own later job, and nothing here makes it harder.
- **Dropping the re-exports in each move.** Every move would then rewrite `use` paths across the tree, and a branch open on main would conflict on each.

## Consequences

- A leaf's manifest names no other member, which the build holds, since a crate cannot name one above or beside it.
- `cargo fmt --all`, `cargo clippy --all-targets` and `cargo test` run from the root cover every member through `default-members`. A member alone is `-p`.
- Scripts that scan `src/` scan every member's `src/` too, and a test that names a type above its crate moves to the crate that holds both.

ADR 0017 adds `protocol` to the map, between `core` and `runner`: it holds the runner's typed requests, answers and owned status, and names only `core`, serde and serde_json.

ADR 0017 also adds `client`, beside `runner`: it speaks the runner's socket and names only `protocol` and `core`. The root uses it.
