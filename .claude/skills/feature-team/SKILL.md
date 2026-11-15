---
name: feature-team
description: Run a scoped Penroam feature iteration with a small Claude Code agent team.
disable-model-invocation: true
argument-hint: [feature or bug and expected behavior]
---

Run a **Claude Code Agent Team**, not merely parallel subagents, for this Penroam iteration:

$ARGUMENTS

1. Inspect current source and git status. This application is ALREADY IMPLEMENTED; find real code and tests before proposing implementation.
2. Define the observed issue, success criteria, regression risks, and exact files likely to change.
3. Spawn only 2-4 relevant teammates **using the named custom agent types** available in this repository. Do not spawn every role. Prefer distinct implementation ownership, independent QA, and a reviewer for riskier work.
4. Make the shared task list with explicit dependencies, assigned file ownership, and no overlapping edits. Ask reviewers not to modify implementation files. Shared worktree means no automatic isolation for teammates.
5. Teammates must communicate API/behavior contracts directly before changing boundaries; the lead integrates any cross-cutting edits.
6. Implement the smallest meaningful fix or feature. Keep public/private repository code separate. Do not write private cloud code into public repo.
7. Require focused unit/integration tests, type checks/lints, and targeted manual/device verification when available. Label simulated tests as such.
8. Have the independent QA reviewer review the final diff. Resolve findings or explicitly report blockers.
9. Summarize changed paths, observed results, open risks, and next actionable issue. Do not deploy/push/publish unless explicitly requested.

For a small one-file change, prefer doing it in a single session rather than spawning a team; tell the user why.
