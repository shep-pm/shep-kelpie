//! The agent files kelpie ignores because their names start with `_`

use super::Line;
use crate::agents::Agents;

/// One line naming each such file, or none when there is none
pub(super) fn check(agents: &Agents) -> Option<Line> {
    let files: Vec<String> = (agents.ignored().iter())
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    (!files.is_empty()).then(|| {
        Line::ok(
            "agent files",
            format!(
                "ignored, since their names start with `_`: {}",
                files.join(", ")
            ),
        )
    })
}
