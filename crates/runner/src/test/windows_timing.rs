use std::process::Command;
use std::time::Instant;

use super::*;

fn timed<T>(name: &str, run: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = run();
    eprintln!("TIMING {name}: {:?}", start.elapsed());
    value
}

#[test]
fn windows_timing_probe() {
    for sample in 0..3 {
        eprintln!("TIMING sample {sample}");
        let dir = timed("tempdir", || tempfile::tempdir().unwrap());
        timed("100 files", || {
            for n in 0..100 {
                std::fs::write(dir.path().join(n.to_string()), "data").unwrap();
            }
        });
        for (program, args) in [
            (std::path::PathBuf::from("git"), vec!["--version"]),
            (crate::spawn::sh(), vec!["-c", "exit 0"]),
        ] {
            timed(&format!("process {program:?}"), || {
                assert!(
                    Command::new(program)
                        .args(args)
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
            });
        }
        let fake = write_fake(dir.path(), "fake", &Fake::new());
        timed("stand-in", || {
            assert!(Command::new(fake).output().unwrap().status.success());
        });
        let rig = timed("Rig::new", || Rig::new("timing"));
        let runner = timed("Rig::open", || rig.open().unwrap());
        timed("add", || rig.ask(&runner, "add", Some("7")));
        timed("step", || crate::runner::step(&runner).unwrap());
        timed("drop runner", || drop(runner));
        timed("drop rig", || drop(rig));
        timed("drop tempdir", || drop(dir));
    }
}
