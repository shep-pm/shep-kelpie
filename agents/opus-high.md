---
# Opus 5.5 at high effort, for work that is hard to undo. The agent board
# picks it for heavy work, and an `agent:opus-high!` label on an issue pins
# it there. Anything below the closing line is added to kelpie's own
# instructions for this agent; it adds nothing here.
role: implementer
harness: claude-code
model: claude-opus-5-5
effort: high
description: Builds work that is hard to undo, such as a data migration, a release step or a change to a public interface.
tier: heavy
---
