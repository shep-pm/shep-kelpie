//! Dev servers a dead run left under kelpie's folders
//!
//! A runner killed while its shots ran never reaches its own cleanup, and
//! the server's processes are reparented to init. A process is taken for one
//! of those only when it sits under a folder kelpie owns, nothing runs it,
//! and it or something it started is the sandbox runtime or listens on a
//! port. A server the maintainer runs elsewhere matches none of that, and
//! neither does a name or a port on its own.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long a process gets to exit on SIGTERM before it gets SIGKILL
const GRACE: Duration = Duration::from_secs(3);

/// How often a stopping process is checked
const POLL: Duration = Duration::from_millis(50);

/// One row of the process table
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Proc {
    pub(super) pid: u32,
    pub(super) ppid: u32,
    pub(super) command: String,
}

/// Stops every orphaned dev server under `folders`, and what it started
///
/// `srt` is the sandbox runtime's command line, which every dev server kelpie
/// starts runs under.
pub(super) fn stop_under(folders: &[PathBuf], srt: &Path) {
    let folders: Vec<PathBuf> = folders
        .iter()
        .filter_map(|f| f.canonicalize().ok())
        .collect();
    if folders.is_empty() {
        return;
    }
    let stale = stale_servers(
        &processes(),
        &working_folders(),
        &listeners(),
        &folders,
        srt,
        std::process::id(),
    );
    stop(&stale);
}

// The pids to stop: each process that init adopted and that works under one
// of `folders`, with its descendants, when any of them is the sandbox
// runtime or listens on a port.
fn stale_servers(
    procs: &[Proc],
    cwds: &HashMap<u32, PathBuf>,
    listening: &HashSet<u32>,
    folders: &[PathBuf],
    srt: &Path,
    own: u32,
) -> Vec<u32> {
    let srt = srt.to_string_lossy();
    let mut children: HashMap<u32, Vec<&Proc>> = HashMap::new();
    for proc in procs {
        children.entry(proc.ppid).or_default().push(proc);
    }
    let mut stale = Vec::new();
    for root in procs {
        let ours = cwds
            .get(&root.pid)
            .is_some_and(|cwd| folders.iter().any(|f| cwd.starts_with(f)));
        if root.ppid != 1 || root.pid == own || !ours {
            continue;
        }
        let mut tree = vec![root];
        let mut next = 0;
        while let Some(proc) = tree.get(next) {
            next += 1;
            let below = children.get(&proc.pid).into_iter().flatten();
            tree.extend(below.filter(|p| p.pid != own));
        }
        let server = tree
            .iter()
            .any(|p| listening.contains(&p.pid) || p.command.contains(&*srt));
        if server {
            stale.extend(tree.iter().map(|p| p.pid));
        }
    }
    stale
}

fn processes() -> Vec<Proc> {
    let Some(text) = output("ps", &["-axo", "pid=,ppid=,command="]) else {
        return Vec::new();
    };
    text.lines().filter_map(parse_proc).collect()
}

fn parse_proc(line: &str) -> Option<Proc> {
    let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (ppid, command) = rest.trim_start().split_once(char::is_whitespace)?;
    Some(Proc {
        pid: pid.parse().ok()?,
        ppid: ppid.parse().ok()?,
        command: command.trim().to_owned(),
    })
}

// Each process's current folder, from `/proc` where there is one and from
// `lsof` where there is not.
fn working_folders() -> HashMap<u32, PathBuf> {
    if let Ok(entries) = fs::read_dir("/proc") {
        return entries
            .flatten()
            .filter_map(|e| {
                let pid = e.file_name().to_str()?.parse().ok()?;
                Some((pid, fs::read_link(e.path().join("cwd")).ok()?))
            })
            .collect();
    }
    output("lsof", &["-a", "-d", "cwd", "-Fpn"])
        .map(|text| parse_lsof_cwd(&text))
        .unwrap_or_default()
}

// `lsof -F` prints `p<pid>`, then one `n<name>` for each file it lists.
fn parse_lsof_cwd(text: &str) -> HashMap<u32, PathBuf> {
    let mut found = HashMap::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let (Some(name), Some(pid)) = (line.strip_prefix('n'), pid) {
            found.insert(pid, PathBuf::from(name));
        }
    }
    found
}

// The processes with a TCP socket in the listen state.
fn listeners() -> HashSet<u32> {
    let text = output("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fp"]).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.strip_prefix('p')?.parse().ok())
        .collect()
}

// A program's standard output, or nothing when it is missing or fails. `lsof`
// exits 1 when it lists nothing, which is not a failure here.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn signal(pids: &[u32], signal: &str) {
    if pids.is_empty() {
        return;
    }
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .args(pids.iter().map(u32::to_string))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn running(pids: &[u32]) -> Vec<u32> {
    let table = processes();
    pids.iter()
        .copied()
        .filter(|pid| table.iter().any(|p| p.pid == *pid))
        .collect()
}

// SIGTERM, then SIGKILL for whatever is still in the table after the grace.
fn stop(pids: &[u32]) {
    signal(pids, "TERM");
    let deadline = Instant::now() + GRACE;
    let mut left = running(pids);
    while !left.is_empty() && Instant::now() < deadline {
        thread::sleep(POLL);
        left = running(&left);
    }
    signal(&left, "KILL");
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "/k/tools/node_modules/@anthropic-ai/sandbox-runtime/dist/cli.js";

    fn proc(pid: u32, ppid: u32, command: &str) -> Proc {
        Proc {
            pid,
            ppid,
            command: command.to_owned(),
        }
    }

    fn cwds(rows: &[(u32, &str)]) -> HashMap<u32, PathBuf> {
        rows.iter().map(|(p, d)| (*p, PathBuf::from(d))).collect()
    }

    fn stale(procs: &[Proc], cwds: &HashMap<u32, PathBuf>, listening: &[u32]) -> Vec<u32> {
        let listening = listening.iter().copied().collect();
        let folders = [PathBuf::from("/k/wt/lab"), PathBuf::from("/k/targets/lab")];
        let mut found = stale_servers(procs, cwds, &listening, &folders, Path::new(SRT), 999);
        found.sort_unstable();
        found
    }

    #[test]
    fn an_orphaned_sandbox_wrapper_goes_with_everything_it_started() {
        let procs = [
            proc(
                10,
                1,
                &format!("node {SRT} -d --settings s.json -- bun run dev"),
            ),
            proc(11, 10, "sandbox-exec -p (version 1) bun run dev"),
            proc(12, 11, "bun run dev"),
            proc(13, 12, "node vite"),
        ];
        let cwd = cwds(&[(10, "/k/wt/lab/7/web"), (11, "/k/wt/lab/7/web")]);
        assert_eq!(stale(&procs, &cwd, &[]), [10, 11, 12, 13]);
    }

    #[test]
    fn an_orphaned_listener_goes_when_its_wrapper_is_already_gone() {
        let procs = [proc(12, 1, "bun run dev"), proc(13, 12, "node vite")];
        let cwd = cwds(&[(12, "/k/wt/lab/7"), (13, "/k/wt/lab/7")]);
        assert_eq!(stale(&procs, &cwd, &[13]), [12, 13]);
    }

    #[test]
    fn a_server_in_a_build_folder_counts_as_one_under_kelpie() {
        let procs = [proc(12, 1, "node server.js")];
        let cwd = cwds(&[(12, "/k/targets/lab/7")]);
        assert_eq!(stale(&procs, &cwd, &[12]), [12]);
    }

    #[test]
    fn a_server_the_maintainer_runs_elsewhere_is_left_alone() {
        let procs = [
            proc(20, 1, "bun run dev"),
            proc(21, 20, "node vite"),
            proc(30, 1, &format!("node {SRT} -- bun run dev")),
        ];
        let cwd = cwds(&[
            (20, "/Users/me/GitHub/lab"),
            (21, "/Users/me/GitHub/lab"),
            (30, "/Users/me/GitHub/other"),
        ]);
        assert_eq!(stale(&procs, &cwd, &[21]), Vec::<u32>::new());
    }

    #[test]
    fn a_folder_that_only_shares_a_name_prefix_is_not_kelpies() {
        let procs = [proc(12, 1, "bun run dev")];
        let cwd = cwds(&[(12, "/k/wt/lab-2/7")]);
        assert_eq!(stale(&procs, &cwd, &[12]), Vec::<u32>::new());
    }

    #[test]
    fn a_server_someone_still_runs_is_left_alone() {
        let procs = [proc(5, 400, "zsh"), proc(12, 5, "bun run dev")];
        let cwd = cwds(&[(12, "/k/wt/lab/7")]);
        assert_eq!(stale(&procs, &cwd, &[12]), Vec::<u32>::new());
    }

    #[test]
    fn an_orphan_under_kelpie_that_serves_nothing_is_left_alone() {
        let procs = [proc(12, 1, "cargo build"), proc(13, 12, "rustc")];
        let cwd = cwds(&[(12, "/k/wt/lab/7")]);
        assert_eq!(stale(&procs, &cwd, &[]), Vec::<u32>::new());
    }

    #[test]
    fn the_runner_itself_is_never_stopped() {
        let procs = [
            proc(999, 1, &format!("node {SRT}")),
            proc(1000, 999, "child"),
        ];
        let cwd = cwds(&[(999, "/k/wt/lab/7")]);
        assert_eq!(stale(&procs, &cwd, &[]), Vec::<u32>::new());
    }

    #[test]
    fn the_process_table_reads_pid_parent_and_command() {
        assert_eq!(
            parse_proc("  812     1 /usr/bin/node /k/cli.js --settings s.json"),
            Some(proc(812, 1, "/usr/bin/node /k/cli.js --settings s.json"))
        );
        assert_eq!(parse_proc("PID PPID COMMAND"), None);
    }

    #[test]
    fn lsof_names_each_process_folder() {
        let found = parse_lsof_cwd("p10\nfcwd\nn/k/wt/lab/7\np11\nfcwd\nn/Users/me\n");
        assert_eq!(found[&10], Path::new("/k/wt/lab/7"));
        assert_eq!(found[&11], Path::new("/Users/me"));
    }

    // Real processes. A launcher shell starts the stand-in for the sandbox
    // runtime in the background and exits, so init adopts it, as a dead
    // runner's child is. The stand-in and its server both ignore SIGTERM.
    fn orphan(srt: &Path, folder: &Path, pids: &Path) {
        let script = "echo $$ > \"$1\"; trap '' TERM; sleep 300 & echo $! >> \"$1\"; wait\n";
        fs::write(srt, script).unwrap();
        Command::new("sh")
            .args(["-c", "sh \"$0\" \"$1\" </dev/null >/dev/null 2>&1 &"])
            .arg(srt)
            .arg(pids)
            .current_dir(folder)
            .status()
            .unwrap();
    }

    fn started(pids: &Path) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = fs::read_to_string(pids).unwrap_or_default();
            let found: Vec<u32> = text.lines().filter_map(|l| l.parse().ok()).collect();
            if found.len() == 2 {
                return found;
            }
            assert!(Instant::now() < deadline, "the stand-in never started");
            thread::sleep(POLL);
        }
    }

    // A bun server under the real sandbox runtime, adopted by init as a
    // killed runner's would be.
    #[test]
    #[ignore = "needs kelpie's tools and bun: KELPIE_TOOLS=<dir> from `shep kelpie tools install`"]
    fn a_real_sandboxed_bun_server_is_stopped_with_its_port() {
        let tools = crate::preview::Tools::at(std::env::var_os("KELPIE_TOOLS").unwrap().into());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let settings = root.join("sandbox.json");
        let text = format!(
            r#"{{"network":{{"allowedDomains":[],"deniedDomains":[],"allowLocalBinding":true}},"filesystem":{{"denyRead":[],"allowWrite":["{}"],"denyWrite":[]}}}}"#,
            root.display()
        );
        fs::write(&settings, text).unwrap();
        let serve = format!(
            "Bun.serve({{port:{port},fetch:()=>new Response('up')}});setInterval(()=>{{}},1000)"
        );
        Command::new("sh")
            .args([
                "-c",
                "node \"$0\" -d --settings \"$1\" -- bun -e \"$2\" </dev/null >/dev/null 2>&1 &",
            ])
            .arg(tools.sandbox())
            .arg(&settings)
            .arg(&serve)
            .current_dir(&root)
            .status()
            .unwrap();
        let listening = || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !listening() {
            assert!(Instant::now() < deadline, "the server never came up");
            thread::sleep(POLL);
        }

        stop_under(&[root], &tools.sandbox());

        assert!(!listening(), "the server still answers on {port}");
    }

    #[test]
    fn a_dead_runs_server_is_stopped_and_one_outside_kelpies_folders_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (ours, theirs) = (root.join("wt"), root.join("elsewhere"));
        fs::create_dir_all(&ours).unwrap();
        fs::create_dir_all(&theirs).unwrap();
        let srt = root.join("cli.js");
        orphan(&srt, &ours, &root.join("ours.pids"));
        let ours_pids = started(&root.join("ours.pids"));
        orphan(&srt, &theirs, &root.join("theirs.pids"));
        let theirs_pids = started(&root.join("theirs.pids"));

        stop_under(&[ours], &srt);

        let left = running(&ours_pids);
        let bystanders = running(&theirs_pids);
        stop(&theirs_pids);
        assert!(
            left.is_empty(),
            "a server under kelpie's folder survived: {left:?}"
        );
        assert_eq!(bystanders, theirs_pids, "a server outside it was stopped");
    }
}
