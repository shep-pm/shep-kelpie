# A process's program is not its first argument

- On macOS 26.2, three `sleep`s started as `exec -a /fake/argv0 /bin/sleep`, as `./sleeplink` (a symlink to `/bin/sleep`) and as `./sleep` from `/bin` all print their given argv[0] under `ps -o comm=` and `ps -o args=`. `lsof -d txt -Fpn` prints `/bin/sleep` for all three.
- `lsof -d txt -Fpn` prints, per process, `p<pid>` then an `ftxt` / `n<path>` pair for the executable, then pairs for `dyld` and each library. The first `n` after `p` is the program.
- `lsof -d txt -Fpn` over every process took 0.29 s against 0.05 s for `ps -A -o pid=,args=`.
- On Linux the program is the target of `/proc/<pid>/exe`, with ` (deleted)` appended when its file was removed.
- `ps -o pid= -p <pids>` exits 1 and prints nothing when none of the pids is there.
