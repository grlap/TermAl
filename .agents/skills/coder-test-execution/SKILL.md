---
name: coder-test-execution
description: Implement code and run meaningful tests without babysitting long-running commands. Use for test selection, completion notification and READY handoff.
---

# Coder test execution

1. Discover the project's own instructions, build commands, launcher and any changed-path/test mapping it has. Use those existing contracts and real commands; do not create another launcher, gate or selection guarantee.
2. Own the complete working package: code, necessary test changes, build and meaningful changed-behavior checks. Select changed and new tests using what step 1 found. When uncertain, choose a wider targeted suite justified by the touched seams, not an unconditional whole-suite run.
3. Run focused checks in the owned worktree with the correct cwd and environment. Do not edit the tested input while it runs and never cancel a running test to end a turn. Where a tool offers it, prefer the option that reports every failure in one run, so one run shows them all.
4. Choose the completion shape before launch. When the project supports a background/detached run with an own-run completion notification, use it: after the start receipt, end the turn and wait for that exact completion. When no such notification exists, disclose it and use the supported foreground completion or existing recovery route. Recover an existing run instead of rerunning it. Do not poll, sleep, tail logs or issue progress loops. A message may not be able to interrupt a foreground wait, so this choice can't be corrected after launch.
5. Keep full logs as runner artifacts; bring only bounded failure diagnostics into context. Classify product, test/fixture or environment failure. Fix what the package owns in the owning worktree, rerun the affected checks, and route the rest as the project's instructions say. Never retry until green or weaken coverage.
6. READY includes exact commit/tree, one line on why the selection covers the change, commands, results, artifact locators, remaining gaps, and any review results and finding dispositions the project requires before READY. Integration owns the final full validation of the combined change; no duplicate whole-suite run beforehand, unless the project's instructions require one.

No project names or commands are hardcoded here: each repository's own instructions and launcher documentation define them.
