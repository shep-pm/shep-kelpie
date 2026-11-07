# codebase-memory-mcp

DeusData/codebase-memory-mcp, MIT per its badge, written in C. Indexing is local CPU and costs no tokens ("average repo in milliseconds", the Linux kernel in 3 minutes), stored in SQLite under `~/.cache/codebase-memory-mcp/`, and a background watcher reindexes on git changes. It has 17 MCP tools, including `detect_changes` (git diff to affected symbols) and `trace_path`. Its installer writes to agent config files, so load it through `--mcp-config` instead. Claude Code lists its tools as deferred behind a tool search: in the implementation series no worker looked them up unprompted, in 12 runs out of 12.

## Pinned for kelpie (#599)

- Pinned release `v0.11.0`. `tools install` downloads the archive for the machine (`darwin-arm64`, `darwin-amd64`, `linux-amd64-portable`, `linux-arm64-portable`) and refuses one whose SHA-256 is not the one in `crates/process/src/tools/graph.rs`. Windows and other CPUs get no server.
- Kelpie starts it as an MCP stdio server with `CBM_CACHE_DIR` (the index folder) and `CBM_ALLOWED_ROOT` (the worktree) set, and indexes first with `codebase-memory-mcp cli index_repository --repo-path <worktree>`. Its README says a repo indexed from outside a session is not watched, so kelpie indexes again before every call.
- Kelpie gives it to the reviewers and the triager, where it is installed, and never to a worker's own working turns (#991, #1834). The reason is the definitions, not the index: a call whose server connects sends the 17 tool definitions on every request of every turn, 16,470 bytes on Claude Code 2.1.302 and 2.1.303 and 17,122 characters on pi, and no kelpie worker or reviewer session on the maintainer's machine has answered one of them (`mcp-servers.md`).
- Measured 2026-10-19 on Claude Code 2.1.302 and 2.1.303: a call whose server connects sends those 17 definitions in the request's `tools` array, beside a `DeferredToolPlaceholder` of 204 bytes, so the deferral above does not keep them out of a request. Before #948 no session held them at all, because the server never connected inside a call.
- Not measured: how long a second index of a changed worktree takes.

## Offered to the local model, called almost never (#600, measured 2026-10-17)

The refactor matrix in the experiments repo, night2: one task, splitting shep-kelpie into a workspace at `04e5794152`, on pi with the local model (`iq3_xxs`), at least 60 minutes per arm and stopped at 120. Each arm had the tool set below as MCP servers; rust-analyzer ran through the experiments repo's `refactor/lsp_mcp.py`. Every arm's session listed the tools it was given.

| arm | minutes | tool calls | graph calls | rust-analyzer calls |
|---|---|---|---|---|
| none | 88 | 484 | - | - |
| graph | 74 | 477 | 3 | - |
| rust-analyzer | 59 | 280 | - | 0 |
| both | 75 | 357 | 2 | 0 |

- 5 graph calls and no rust-analyzer call in 1,114 calls. The rest is `bash`, `read`, `edit` and `write`.
- With so few calls, neither tool can have changed what an arm did, so the arms were not graded against each other.
- The same holds in production: pi workers had the graph from #949 and made 0 of 2,408 calls to it (#966).
- Not measured: Claude Code's own LSP tool on this task, or a prompt that tells the worker to use either tool.

So the graph stays off for a worker's turns by default (`code_graph.worker`, `docs/design/fence.md`). A tool the worker does not call only costs its definitions in every request.

## The daemon (measured on macOS, v0.11.0, 2026-10-15)

- The server and its CLI talk to a daemon, which the first one to need it starts and which stops when its last client disconnects. There is one daemon per runtime folder, `CBM_RUNTIME_DIR` (default `/private/tmp/cbm-daemon-<uid>` on macOS), and it serves one cache folder. Many repos can share one cache. A second cache under the same runtime folder fails with "active account daemon uses a different cache directory". A separate `CBM_RUNTIME_DIR` per cache runs side by side.
- In production this broke every index from the moment an interactive Claude Code session started the default daemon without kelpie's `CBM_CACHE_DIR`. Kelpie now gives each work item its own runtime folder, `<project>/cbm/<issue>`, so no other session's daemon can hold it.
- The runtime folder must exist (the server will not create it). Its mode does not matter.
- The socket is `<runtime>/cbm-daemon-<uid>/cbm-<16 hex>.sock` and its path must fit the 104-byte macOS limit. A longer one fails with "secure CLI coordination could not be created (endpoint)", which names no path, so kelpie adds the path to a failed index's error when it is too long.
- Inside a call's sandbox the server has to start its own daemon, and `socket_bind` fails with EPERM unless the runtime folder is writable and listed in `network.allowUnixSockets`. With both, it starts. The folder alone in `allowUnixSockets` is enough, but it must be the real path (`/private/tmp`, not `/tmp`). Not measured on Linux, where srt refuses every Unix socket (#488).
- That was measured with a runtime folder in `/private/tmp`. Kelpie's folders sit under the shepherd's home, which every call's `denyRead` holds (`~/.shep/**`), and there the server never started (#948). Before it starts, it checks each folder above its cache folder and its runtime folder, and srt allows only a folder's metadata inside a read deny. With those folders unreadable it exits with "exact executable identity could not be verified (cache-private) - <cache>: ancestry component validation failed (errno 1)", and with only the cache's readable, "secure daemon endpoint could not be created". Claude Code reports that as `CONNECTION_CLOSED`. Folders above the worktree (`CBM_ALLOWED_ROOT`) and the binary need nothing.
- srt has no rule for one folder alone: a path without glob characters in `allowRead` is the folder and all it holds. On macOS a glob becomes a regex of the whole path, so a folder with one character bracketed (`/k/graph[s]`) is readable and listable while nothing in it is. With each folder above both folders given that way, the server answers `initialize`, `list_projects`, `search_graph` and `get_code_snippet`.
- `search_code` makes its scratch folder at `/tmp/cbm-search-XXXXXX`: `cbm_tmpdir()` is `/tmp` on every POSIX system, whatever `TMPDIR` says (`src/foundation/compat.h` at v0.11.0, and on its `main` on 2026-10-15). In a sandbox that cannot write there it fails with "cannot create temp file (Operation not permitted)". The index watcher (`cbm-watcher-git-XXXXXX`) and an artifact export (`cbm-artifact-XXXXXX`) write there too. With `/private/tmp/cbm-search-*/**` in `allowWrite` the folder can be made and nothing written in it; `/private/tmp/cbm-search-*` and `/private/tmp/cbm-search-*/*` together let `search_code` answer.
- The daemon's socket is 41 bytes longer than the runtime folder, so under macOS's default `TMPDIR` (`/private/var/folders/…/T/`) a test's runtime folder is too deep for it. `adapters::mcp::live` says so and skips unless `TMPDIR` is short.
- `ui_enabled` defaults to true and is saved per cache folder. With it on, the daemon serves a web UI on `127.0.0.1:9749`. Kelpie runs `config set ui_enabled false` with the work item's `CBM_CACHE_DIR` before each index, and the index then runs with no UI.
- `cli delete_project --project <name>` removes one project from a cache.
