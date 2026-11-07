# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/nh-99/penroam/security/advisories/new),
not in a public issue. Include what you found, how to reproduce it and which version you used.

You can expect an acknowledgement within 7 days. Penroam is maintained by one person, so a fix can
take longer; you'll hear how it is going. We will credit you in the advisory unless you'd rather
not be named.

Never include real note content, a recovery code or account credentials in a report. Nobody working
on Penroam needs them, and nobody will ask you for them.

## What is in scope

* The app and the crates in this repository, including file importers, sync, encryption and the
  native pen plugins.
* The sync protocol and its documentation.
* Release artifacts and the update feed.

Report problems with the hosted Penroam Cloud service the same way. Its server code is not public.

## Supported versions

Security fixes go into the newest release only. Each release's notes state which file and sync
versions it uses, and older releases stay available for download.

## Current status of the encryption

Sync encryption has **not** been independently reviewed. The design, what the server can see and the
known limits are in the [threat model](docs/security/threat-model.md).

## Checking a download

Each release includes `SHA256SUMS`, a CycloneDX software bill of materials and a build provenance
attestation:

```bash
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <file> --repo nh-99/penroam
```
