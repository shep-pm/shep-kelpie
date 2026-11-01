//! The agent files the doctor's scene lists, which kelpie no longer ships: a
//! project writes the lineup it wants, and these tests write the one they
//! were built on

use std::path::Path;

use crate::test::write_in;

/// The reviewer on the gateway `paddock`, which a scene's kelpie settings hold only when a test says so
pub(super) const IQ3_XXS_REVIEW: &str = "iq3-xxs-review";

const FILES: [(&str, &str); 5] = [
    (
        "sonnet-high",
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\neffort: high\n\
         description: Builds everyday features and anything with state, such as a store, a queue or a migration.\n\
         tier: standard\n---\n",
    ),
    (
        "opus-high",
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-opus-5-5\neffort: high\n\
         description: Builds work that is hard to undo, such as a data migration, a release step or a change to a public interface.\n\
         tier: heavy\n---\n",
    ),
    (
        "defect-hunter-lite",
        "---\nextends: kelpie:defect-hunter\nharness: claude-code\nmodel: claude-opus-5-5\n\
         effort: medium\nsecond_look: false\n\
         description: Reads a pull request once for defects a careful maintainer would block the merge for.\n\
         tier: standard\n---\n",
    ),
    (
        IQ3_XXS_REVIEW,
        "---\nrole: reviewer\nharness: endpoint\ngateway: paddock\nmodel: iq3_xxs\ncontext: 262144\n\
         description: Reads a diff for defects on the local paddock model, as a first pass before a stronger reviewer.\n\
         tier: light\n---\n",
    ),
    (
        "qwen",
        "---\nrole: reviewer\nharness: command\ncommand: ~/.claude/scripts/qwen-review.sh\n\
         description: Reads a diff for defects with the maintainer's local qwen-review script, as a first pass.\n\
         tier: light\n---\n",
    ),
];

/// Writes the five agent files the scenes list into `folder`
pub(super) fn write(folder: &Path) {
    for (name, text) in FILES {
        write_in(folder, &format!("{name}.md"), text);
    }
}
