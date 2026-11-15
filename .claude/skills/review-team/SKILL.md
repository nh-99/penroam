---
name: review-team
description: Independent multi-angle read-only review of an existing Penroam change.
disable-model-invocation: true
argument-hint: [PR, branch, or feature to review]
---

Review $ARGUMENTS in a Claude Code Agent Team.

Spawn 3 named teammates selected from penroam-qa, penroam-security, penroam-ink, penroam-core, or penroam-ux as appropriate. Review from independent angles (data integrity, device performance, security/privacy or UX). All teammates must remain read-only; do not create or modify product files. Have teammates message each other to challenge findings and reach a prioritized consensus. For each finding give severity, file, reproducible evidence, user impact, and a narrow fix. The lead synthesizes one actionable report. No deploys, pushes or production commands.
