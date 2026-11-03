# Penroam: project instructions

## No milestone labels in the repository

`agent-docs/` holds the planning specs, organised as milestones (M00–M10). Treat them as
input only. **Never** put milestone names or numbers (`M01`, `M02`, `M03.1`, "milestone", …)
into anything in the repository outside `agent-docs/`. That covers code, comments, test
names, file and directory names, fixtures, docs, the README, commit messages and PR text.

Describe what the code actually does instead. For example, write "the operation journal",
"archive format 1 / notebook schema 1", or "not implemented yet: encrypted sync".

## What goes in `docs/` and what stays local

`docs/` only describes the code as it is now:

* `docs/architecture/overview.md`: design
* `docs/formats/` and `docs/protocol/`: contracts
* `docs/platforms/`: per-platform build notes and known risks
* `docs/development.md`: building, testing and tooling
* `docs/benchmarks/protocol.md`: benchmark method

Progress reports, plans, test-run evidence, benchmark results, device-run logs and historical
research do not belong in the repository. The agent-docs playbook asks for
`docs/roadmap/MXX-plan.md` and `MXX-results.md`; write those under
`agent-docs/results/` instead, which is git-ignored and local only. Earlier reports are in
`agent-docs/archive/`. Raw benchmark output goes in the git-ignored `benchmarks/`.

## README is for users

`README.md` covers only what Penroam is, screenshots, how to install it and how to use it.
Developer material goes in `docs/development.md`.

When a user-visible feature lands or changes, update the README's usage section in the same
change. If the UI changed, also regenerate the screenshots with `tools/screenshots/capture.sh`.
Never describe features that do not exist yet as available.

## Commits

Use the repository's configured git identity. Add no `Co-Authored-By` or other attribution
trailers.
