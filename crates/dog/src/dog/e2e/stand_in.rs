//! Stand-ins for `ssh`, `rsync` and the hosts they reach, for tests
//!
//! Two scripts in a temp folder take the place of the programs, and a folder
//! each stands for a host's account: `ssh -tt <host> <command>` runs the
//! command in that folder, and `rsync` reads and writes `<host>:<path>` there.
//! The rsync one honours only the flags the dog's safety rests on,
//! `--exclude=.git` and `--no-links`, so a run that leaves one out copies
//! what the flag would have kept back. Both log every start with their
//! parent's process, which is the dog's when it starts them directly, and
//! keep the environment of their latest start.

use std::path::{Path, PathBuf};

use super::Remote;
use crate::spawn;
use crate::test::write_script;

// `@PID@` is the process the tests ask about, and `@RUN@` runs the command.
const SSH: &str = r#"#!/bin/sh
echo "ssh $PPID $*" >> '@DIR@/started.log'
env > '@DIR@/ssh.env'
echo @PID@ > '@DIR@/ssh.pid'
mkdir -p '@DIR@/hosts/'"$2"
cd '@DIR@/hosts/'"$2" || exit 97
@RUN@
"#;

// Unix: the script becomes the command, so killing ssh ends it.
const RUN_IN_PLACE: &str = r#"exec sh -c "$3""#;

// Windows: the dog kills the `.cmd` that starts this script and not the script,
// so a watcher ends the command when the dog lets go of ssh's input.
const RUN_AND_WATCH: &str = r#"exec 9<&0
sh -c "$3" &
run=$!
( cat <&9 >/dev/null 2>&1; kill $run ) >/dev/null 2>&1 &
wait $run"#;

const RSYNC: &str = r#"#!/bin/sh
echo "rsync $PPID $*" >> '@DIR@/started.log'
env > '@DIR@/rsync.env'
git=keep
links=copy
from=
to=
for arg; do
  from=$to
  to=$arg
  case "$arg" in
    --exclude=.git) git=skip ;;
    --no-links) links=skip ;;
  esac
done
here() {
  case "$1" in
    [A-Za-z]:*) echo "$1" ;;
    *:*) mkdir -p '@DIR@/hosts/'"${1%%:*}"; echo '@DIR@/hosts/'"${1%%:*}/${1#*:}" ;;
    *) echo "$1" ;;
  esac
}
src=$(here "$from")
dst=$(here "$to")
[ -d "$src" ] || { echo "rsync: change_dir \"$src\" failed: No such file or directory" >&2; exit 23; }
mkdir -p "$dst"
(cd "$src" && find . -print) | while IFS= read -r path; do
  [ "$path" = . ] && continue
  case "$path" in ./.git|./.git/*) [ $git = skip ] && continue ;; esac
  if [ -L "$src/$path" ] && [ $links = skip ]; then continue; fi
  if [ -d "$src/$path" ]; then mkdir -p "$dst/$path"; else cp "$src/$path" "$dst/$path"; fi
done
"#;

/// The scripts and hosts, in a temp folder that goes with the value
pub struct StandIn {
    _dir: tempfile::TempDir,
    base: PathBuf,
    /// The scripts, for the dog to start in place of the two programs
    pub remote: Remote,
}

impl StandIn {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = plain(dir.path().canonicalize().unwrap());
        let script = |name: &str, text: &str| {
            let text = text.replace("@DIR@", &base.display().to_string());
            start_with(&base, name, &text)
        };
        let ssh = if cfg!(windows) {
            // The Windows process of the script is not the one `$$` names.
            SSH.replace("@PID@", "$(cat /proc/$$/winpid)")
                .replace("@RUN@", RUN_AND_WATCH)
        } else {
            SSH.replace("@PID@", "$$").replace("@RUN@", RUN_IN_PLACE)
        };
        let remote = Remote {
            ssh: script("ssh", &ssh),
            rsync: script("rsync", RSYNC),
        };
        Self {
            _dir: dir,
            base,
            remote,
        }
    }

    /// The folder the dog reads project folders under
    pub fn kelpie_home(&self) -> PathBuf {
        self.base().join("kelpie")
    }

    /// A folder outside every project and host
    pub fn outside(&self) -> PathBuf {
        let outside = self.base().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        outside
    }

    /// The account folder of `host`
    pub fn host_home(&self, host: &str) -> PathBuf {
        self.base().join("hosts").join(host)
    }

    /// Worktree `n` of `project`, whose `e2e.json` holds `table` and whose
    /// command is `scripts/e2e-remote.sh` running `script`
    pub fn project(&self, project: &str, table: &str, n: u32, script: &str) -> PathBuf {
        let folder = self.kelpie_home().join(project);
        let worktree = folder.join("worktrees").join(n.to_string());
        let command = worktree.join("scripts/e2e-remote.sh");
        std::fs::create_dir_all(command.parent().unwrap()).unwrap();
        std::fs::write(folder.join("e2e.json"), table).unwrap();
        std::fs::write(&command, format!("#!/bin/sh\n{script}\n")).unwrap();
        executable(&command);
        std::fs::write(worktree.join(".git"), "gitdir: /elsewhere\n").unwrap();
        worktree
    }

    /// A script line that waits for [`StandIn::open_gate`]
    pub fn wait_at_gate(&self) -> String {
        let base = self.base().display().to_string();
        format!("while [ ! -e '{base}/gate' ] && [ -d '{base}' ]; do sleep 0.05; done")
    }

    pub fn open_gate(&self) {
        std::fs::write(self.base().join("gate"), "").unwrap();
    }

    /// Every program the scripts were started as, oldest first, as
    /// `<name> <parent process> <arguments>`
    pub fn started(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.base().join("started.log")).unwrap_or_default();
        log.lines().map(str::to_owned).collect()
    }

    /// The environment the latest `program` started with, `ssh` or `rsync`
    pub fn env_of(&self, program: &str) -> String {
        std::fs::read_to_string(self.base().join(format!("{program}.env"))).unwrap_or_default()
    }

    /// The process of the latest `ssh`, once there is one
    pub fn ssh_pid(&self) -> Option<u32> {
        let pid = std::fs::read_to_string(self.base().join("ssh.pid")).ok()?;
        pid.trim().parse().ok()
    }

    fn base(&self) -> &Path {
        &self.base
    }
}

/// Writes `text` as the program `name` in `base`, and returns the path the dog
/// starts
///
/// Windows starts a `.cmd` file, so there the script is `<name>.sh`, run by the
/// `.cmd` through Git for Windows' `sh`. `MSYS` makes its `ln -s` a real link,
/// which the rsync stand-in's `--no-links` and the dog's own copy have to meet.
fn start_with(base: &Path, name: &str, text: &str) -> PathBuf {
    if !cfg!(windows) {
        let path = base.join(name);
        write_script(&path, text);
        return path;
    }
    let script = base.join(format!("{name}.sh"));
    write_script(&script, text);
    let path = base.join(format!("{name}.cmd"));
    let shim = format!(
        "@echo off\r\nset MSYS=winsymlinks:nativestrict\r\n\"{}\" \"{}\" %*\r\n\
         exit /b %errorlevel%\r\n",
        spawn::sh().display(),
        script.display()
    );
    std::fs::write(&path, shim).unwrap();
    path
}

// `canonicalize` on Windows gives a `\\?\` path, which the `sh` of the scripts
// does not take.
fn plain(path: PathBuf) -> PathBuf {
    match path.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// Makes the script at `path` something the dog can start
#[cfg(unix)]
fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// A file on Windows has no mode to set: `sh` starts the project's script by
// its first line.
#[cfg(windows)]
fn executable(_path: &Path) {}

/// Whether process `pid` is gone, reaped by the dog that started it
#[cfg(unix)]
pub fn ended(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid.try_into().unwrap()).unwrap();
    rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH)
}

/// Whether process `pid` is gone, as `OpenProcess` answers
#[cfg(windows)]
pub fn ended(pid: u32) -> bool {
    !shep_kelpie_core::tree::alive(pid)
}
