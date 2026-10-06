//! The issue writer's prompt: its file's body, then what kelpie adds for
//! the project and the way it runs

use super::{Mode, Project, Writer};
use crate::board::AGENT_LABEL;
use crate::settings::{Implementer, RoleAgents};

/// The issue writer's whole prompt for `project`: its file's body, then the
/// project's implementers, the commands it has and how `mode` ends
pub fn instructions(writer: &Writer, project: &Project<'_>, mode: Mode) -> String {
    let status = mode.status();
    let ending = match mode {
        Mode::Headless => format!(
            "You run on your own, and no one answers a question. Where the request \
             leaves a choice open, take the one the repo's docs point to, and say in the \
             issue what you chose and why. File every issue with `{status}`, which holds \
             it for the maintainer to read before any agent takes it.\n\n\
             When you have filed everything, end with one JSON object and nothing \
             else:\n\
             {{\"filed\": [<each issue's number, a parent first>], \"note\": \"<one \
             sentence for the maintainer, or empty>\"}}\n\
             If the request needs nothing filed, such as one an open issue already \
             covers, file nothing and say why in the note."
        ),
        Mode::Interactive => format!(
            "You are working with the maintainer in their terminal. Research first, \
             then propose the issues: each one's title, what it delivers, its agent and \
             what blocks it. Ask whether the scope and any split are right, and file only \
             what the maintainer agrees, with `{status}`, which puts it on kelpie's \
             board. Then list what you filed, by number."
        ),
    };
    format!(
        "{}\n\n--- this project ---\n\n{}\n\n{}\n\n{ending}\n",
        writer.prompt.trim_end(),
        implementers(project.agents),
        commands(status),
    )
}

// The listed implementers, the default first, as an `agent:` label names them.
fn implementers(agents: &RoleAgents) -> String {
    let default = &agents.default_implementer;
    let line = |i: &Implementer| {
        let what = match i.is_local() {
            true => ", a local model, which runs only the issues labelled for it",
            false if i.name == default.name => ", the default",
            false => "",
        };
        format!(
            "- `{}`: {} at {} effort{what}",
            i.name,
            i.model.model.as_str(),
            i.model.effort.as_str()
        )
    };
    let rest = (agents.implementers.iter()).filter(|i| i.name != default.name);
    let lines: Vec<String> = std::iter::once(default).chain(rest).map(line).collect();
    format!(
        "The implementers this project lists, the default first. Each issue's \
         `{AGENT_LABEL}` label names one of them, and no other:\n\n{}",
        lines.join("\n")
    )
}

// The commands the guard runs, as the issue writer should write them.
fn commands(status: &str) -> String {
    format!(
        "You have Read, Grep and Glob to read the repo, and Bash for these commands \
         only, one to a call where you can, written as plain words: no `$`, backticks, \
         pipes, redirects or variables. Kelpie's guard refuses anything else, git \
         included.\n\n\
         - File an issue: `gh issue create --title '<title>' --label '{status}' --label \
         '{AGENT_LABEL}<name>' --body-file - <<'EOF'`, then the body's lines, then `EOF`\n\
         - Change the agent of an issue you filed: `gh issue edit <number> --remove-label \
         '{AGENT_LABEL}<old>' --add-label '{AGENT_LABEL}<new>'`\n\
         - Read this repo's issues: `gh issue view <number>`, `gh issue list --search \
         '<words>'`\n\
         - An issue's id, which a link takes: `gh api \
         'repos/{{owner}}/{{repo}}/issues/<number>' --jq .id`, alone in its call\n\
         - Make an issue you filed a sub-issue of another you filed: `gh api -X POST \
         'repos/{{owner}}/{{repo}}/issues/<parent>/sub_issues' -F sub_issue_id=<its id>`\n\
         - Mark an issue you filed blocked by another: `gh api -X POST \
         'repos/{{owner}}/{{repo}}/issues/<number>/dependencies/blocked_by' -F \
         issue_id=<the other's id>`\n\n\
         You edit and link only the issues you filed in this session, by the ids you \
         read as above. Write `{{owner}}/{{repo}}` as it stands: gh fills it in from \
         this checkout."
    )
}
