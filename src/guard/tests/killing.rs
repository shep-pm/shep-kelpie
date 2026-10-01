// `pkill -f x` hits every match on the machine, other sessions' processes
// among them, so a worker stops only a process it started, by pid.

use super::*;
use crate::guard::judging::NO_KILLING_BY_NAME;

#[test]
fn stopping_a_process_by_name_or_pattern_is_refused() {
    for command in [
        "pkill -f x",
        "pkill -f \"target/debug/deps/shep_kelpie\"",
        "killall cargo",
        "kill $(pgrep x)",
        "kill -9 $(pgrep -f x)",
        "kill `pgrep x`",
        "/usr/bin/pkill x",
        "sudo killall node",
        "cd src && pkill x",
        "bash -c 'pkill x'",
        "pgrep x | xargs kill",
        "pgrep x | xargs -n 1 kill -9",
        "find /proc -name x -exec kill {} \\;",
    ] {
        let verdict = bash(command);
        assert_eq!(
            verdict,
            Verdict::Refuse(NO_KILLING_BY_NAME.into()),
            "{command}"
        );
    }
}

#[test]
fn stopping_a_process_by_its_pid_goes_through() {
    for command in [
        "kill 1234",
        "kill -9 1234",
        "kill -TERM 1234 5678",
        "kill -s KILL 1234",
        "kill -SIGINT 1234",
        "kill %1",
        "kill -l",
        "kill $!",
        "kill $pid",
        "kill \"$pid\"",
        "kill ${PID}",
        "kill -9 \"${pid}\"",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}

#[test]
fn naming_a_killer_without_running_it_goes_through() {
    for command in [
        "grep -r pkill src",
        "echo kill the lease",
        "cat docs/killall.md",
    ] {
        assert_eq!(bash(command), Verdict::Allow, "{command}");
    }
}
