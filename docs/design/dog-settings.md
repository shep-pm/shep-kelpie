# The dog's settings

The dog rereads its `[kelpie]` section of `dogs.toml` and applies a change with no restart where it can. A runner rereads each wake (`runner/reread/`); the dog does the same on `LOAD_TICK` (30 seconds).

## What each key does after a change

| key | after a change | why |
|---|---|---|
| `leases.cargo-test` | live | `Desk::set_test_capacity`: a raise grants the waiters, a lower count keeps every holder and holds only new grants off |
| `agent_board.prefer` | live | `Board::set_prefer`: each pick copies the list out in its `View` |
| `gateways` | live | `Live::apply`: the availability task rebuilds `Sources::of(agents, gateways)` and takes it at the start of its next pass. Each model and account still named keeps its reading, a new one is unknown, one gone is dropped with its logged reason. `crate::spawn::hide` only adds, so a gateway taken out keeps its variable hidden. A new key variable reaches the dog's environment only from a fresh spawn, so its models read as unread until `shep disable kelpie` and `shep enable kelpie`, as at start, and `doctor`'s `dog_keys` line says so. The agent files stay as the dog started with them |
| `codex_home` | live | `Live::apply` builds the Codex meter on the new login, and the next pass reads it. A value `KelpieSettings::codex_home` refuses (a relative path, `~/.codex`) refuses the whole section, where the start leaves the Codex account unknown |
| `webhook.kind`, `url`, `ca_cert`, `token`, `trusted` | restart-only | the start checks them as one and refuses a bad one, and the dog posts only to the webhook it started with |
| `gpu_metrics_url` | not the dog's | only a runner reads it, at each wake |

`waiting_for_restart` in `reread/mod.rs` takes both settings apart without `..`, so a new `KelpieSettings` key is a compile error until it is placed in this table. Only the webhook keys are restart-only.

## The pass

- A task spawned from `serve` (`reread::begin`) runs off the dog's loop, since the ask waits on the shepherd. Its first pass is one `LOAD_TICK` after the start.
- The ask is `dog_config` through `&dyn Connection`, so `FakeShepherd` answers it in tests. `section::settings` takes the same.
- The baseline is the text the start read, so the first pass finds it unchanged.
- Text equal to the last good text does nothing. A refused text is checked again each pass, since a `ca_cert` file can appear with no edit.
- A changed text is read whole as the start reads it (`section::read_whole`): `check_ca_cert`, `check_trusted`, then `KelpieSettings::from_section`. The start keeps its salvage of a bad table; a reread never salvages, so one bad table refuses the whole section.
- A good read applies each live key that differs from the last applied, and the log names it (`applied leases.cargo-test: 5 at a time`), never a webhook value. The tickets a raise grants are woken with `door::wake`.
- A restart-only key whose value differs from the one the dog started with is not applied. It is logged once as `waiting for restart: <key>` and listed in `status`. A changed webhook is named by each key that differs, `webhook` alone when the table appears or goes. A key put back stops waiting.

## A reread that fails

- Refused: the shepherd refuses the section (`InvalidConfig`, `Unsupported`), the text does not parse whole, a start check fails, or `codex_home` is one the dog cannot use. Nothing is applied, so the old settings stand.
- The dog logs the reason once, so the maintainer learns now what would stop it at its next restart.
- The refusal keeps its reason and first time (`Refusal { reason, at, told }`, copied from `runner/refused.rs`, which the dog cannot use since the runner crate sits above it). `status` shows it as `settings_refused: { reason, at }` only while it stands, and a good read clears it.
- The webhook the dog started with gets one alert per reason, titled `kelpie: dog settings change refused`. A post that fails is tried again next pass, no webhook means nothing to post, and the post runs in `spawn_blocking`.
- An ask that returns no section (closed, timed out, `NotFound`) keeps the settings and logs once per reason, with no alert and no status entry. Only an empty section text means defaults.

## `status`

`waiting_for_restart` is always there, a list of bare key names, empty when none. `settings_refused` is there only while a refusal stands.
