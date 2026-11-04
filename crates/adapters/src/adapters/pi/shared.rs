//! The arguments pi has in a terminal and headless alike

use std::ffi::OsString;
use std::path::PathBuf;

use super::{Files, PROVIDER, mcp, mcp_config, tools_of};
use crate::adapters::skills::skill_dirs;
use crate::ports::{AgentCall, COMPACT};

// pi's own id for the model: its Anthropic provider on the API, kelpie's on a server
fn model_arg(call: &AgentCall) -> String {
    match call.harness.bills_api() {
        true => format!("anthropic/{}", call.model),
        false => format!("{PROVIDER}/{}", call.model),
    }
}

/// What a call's pi has in a terminal and headless alike: its model, tools,
/// instructions and guard
pub(super) fn profile(call: &AgentCall, files: &Files, skill: Option<PathBuf>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "--model".into(),
        model_arg(call).into(),
        "--thinking".into(),
        call.effort.as_str().into(),
        "--no-extensions".into(),
        "--no-skills".into(),
        "--no-prompt-templates".into(),
        "--no-themes".into(),
        "--no-approve".into(),
        "--offline".into(),
        "--session-dir".into(),
        files.sessions().into(),
    ];
    let tools = tools_of(call);
    let mcp = mcp_config(call);
    match tools {
        Some(names) => {
            let mut names = names.to_owned();
            if mcp.is_some() {
                for server in mcp::tool_names(call) {
                    names.extend([",", &server]);
                }
            }
            argv.extend(["--tools".into(), names.into()]);
        }
        None => argv.push("--no-tools".into()),
    }
    match mcp {
        Some(_) => argv.extend(["--extension".into(), mcp::EXTENSION.into()]),
        None => argv.push("--no-mcp".into()),
    }
    // pi builds its system prompt afresh on every call, a resumed one too.
    if let Some(instructions) = &call.instructions {
        argv.extend(["--append-system-prompt".into(), instructions.into()]);
    }
    if call.reach.fence.is_some() {
        argv.extend(["--extension".into(), files.guard().into()]);
    }
    // The step's skill comes first, so its name wins a collision. Its text is in
    // the prompt, but any other skill needs `read` to open and only costs context.
    if let Some(skill) = skill {
        argv.extend(["--skill".into(), skill.into()]);
    }
    if tools.is_some() && call.prompt != COMPACT {
        for dir in skill_dirs(call) {
            argv.extend(["--skill".into(), dir.into()]);
        }
    }
    argv
}
