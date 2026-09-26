//! `kelpie runner <project>`: a project's runner, run as a sheep
//! `kelpie dog`: the kelpie dog, run as a sheep

#![forbid(unsafe_code)]

use std::process::ExitCode;

const USAGE: &str = "usage: kelpie runner <project> | kelpie dog";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [role, project] if role == "runner" => kelpie::sheep::run(project),
        [role] if role == "dog" => kelpie::dog::run(),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
