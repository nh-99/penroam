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
* `docs/policies/`: privacy and accessibility statements
* `docs/support.md`: help for users
* `docs/known-issues.md`: current known problems and workarounds

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

## Code documentation

The crates are free software, and outside readers should be able to understand them from the
source. When writing or changing code:

* Give every module a `//!` header. Say what the module is for, how it works, the invariants
  that matter (durability ordering, security properties, merge rules), and link the relevant
  doc in `docs/`.
* Give every public item a `///` comment: what it does, when to use it, what its errors mean.
  Library crates enable `#![warn(missing_docs)]`.
* Add inline comments where the reasoning isn't obvious from the code. Explain *why*, not
  *what*.
* Keep it concise and accurate; never document behaviour that doesn't exist.
* `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` must pass.
