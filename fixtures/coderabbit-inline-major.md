<!-- coderabbit-ai-generated:v1 -->

**<picture><source media="(prefers-color-scheme: dark)" srcset="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/major-16-dark.svg"><img src="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/major-16.svg" width="16" height="16" align="absmiddle" alt=""></picture> Major** · **Stability & Availability** · **Heavy lift**

**Use the OS-resolved executable identity for containment.**

The `inside` check compares the raw `args` text of each process against `prefixes`. A process can
be started through a relative path, a symlink or a rewritten argv[0], so its arguments need not
name the executable it runs. Read the executable from the OS's process metadata and compare that
resolved path instead.

<!-- ai-agent-prompt:start -->
<details>
<summary><picture><img src="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/bot.svg" width="16" height="16" align="absmiddle" alt=""></picture> Prompt for AI Agents</summary>

```
Verify each finding against current code. Fix only still-valid issues, skip the
rest with a brief reason, keep changes minimal, and validate.

In `@crates/process/src/worktree/stray.rs` at line 102, Update the `inside`
process-containment check to obtain each process's executable identity from OS
process metadata and compare that resolved path against `prefixes`.
```

</details>
<!-- ai-agent-prompt:end -->
