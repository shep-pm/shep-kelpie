<!-- coderabbit-ai-generated:v1 -->

**<picture><source media="(prefers-color-scheme: dark)" srcset="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/minor-16-dark.svg"><img src="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/minor-16.svg" width="16" height="16" align="absmiddle" alt=""></picture> Minor** · **Observability** · **Quick win**

**Log why the process listing failed before skipping the kill.**

When `running_from` fails during the grace period the sweep returns without a word and leaves the
process alone. Log `why` first, so a failed or empty listing leaves a diagnostic.

<!-- ai-agent-prompt:start -->
<details>
<summary><picture><img src="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/bot.svg" width="16" height="16" align="absmiddle" alt=""></picture> Prompt for AI Agents</summary>

```
In `@crates/process/src/worktree/stray.rs` around lines 53 - 55, Update the
`running_from` failure branch to log `why` before returning.
```

</details>
<!-- ai-agent-prompt:end -->
