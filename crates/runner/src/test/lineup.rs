use super::Rig;

// The agents the rig's project lists, which kelpie no longer ships: a project
// writes the lineup it wants, and the tests write the one they were built on.
const SONNET_HIGH: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                           effort: high\n\
                           description: Builds everyday features and anything with state, such as a store, a queue or a migration.\n\
                           tier: standard\n---\n";

const OPUS_HIGH: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-opus-5-5\n\
                         effort: high\n\
                         description: Builds work that is hard to undo, such as a data migration, a release step or a change to a public interface.\n\
                         tier: heavy\n---\n";

const DEFECT_HUNTER_LITE: &str = "---\nextends: kelpie:defect-hunter\nharness: claude-code\n\
                                  model: claude-opus-5-5\neffort: medium\nsecond_look: false\n\
                                  description: Reads a pull request once for defects a careful maintainer would block the merge for.\n\
                                  tier: standard\n---\n";

const QWEN: &str = "---\nrole: reviewer\nharness: command\ncommand: ~/.claude/scripts/qwen-review.sh\n\
                    description: Reads a diff for defects with the maintainer's local qwen-review script, as a first pass.\n\
                    tier: light\n---\n";

impl Rig {
    /// Writes the agent files the rig's project lists
    pub(super) fn write_lineup(&self) {
        self.write_agent("sonnet-high", SONNET_HIGH);
        self.write_agent("opus-high", OPUS_HIGH);
        self.write_agent("defect-hunter-lite", DEFECT_HUNTER_LITE);
        self.write_agent("qwen", QWEN);
    }
}
