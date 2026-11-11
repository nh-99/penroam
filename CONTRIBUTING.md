# Contributing to Penroam

Bug reports, fixes and device test results are all welcome. Penroam is
maintained by one person, so please open an issue before starting on anything large.

## Licence

Penroam is under the [Mozilla Public License 2.0](LICENSE). By contributing you agree that your
contribution is under the same licence, and you confirm you have the right to make it. There is no
contributor licence agreement.

Don't add code or dependencies under the GPL or other strong copyleft licences, and don't commit
sample files you don't have the right to redistribute. `deny.toml` lists the licences allowed for
Rust dependencies.

## Building and testing

The [developer README](docs/README.md) covers setting up, and links to every developer document;
[docs/development.md](docs/development.md) covers building, tests and tooling. Nothing in this
repository needs a private repository or a credential. Before opening a pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
pnpm typecheck && pnpm lint && pnpm test
```

## What we look for

* **Notes are never lost.** Changes to storage, journal, merge or sync need tests that cover
  crashes and bad input. File and protocol formats are versioned: see `docs/formats/` and
  `docs/protocol/`. Never change a golden fixture to make a test pass.
* **The app works offline.** Nothing may need an account, a server or a network connection, and
  nothing may send data without the user asking for it.
* **Documented code.** Every module has a `//!` header, and every public item a `///` comment.
  Explain why, not what.
* **Honest claims.** The README and the user manual describe only what exists, and a device is
  listed as tested only after a person has tested it. If you change something users see, update
  the [user manual](docs/manual/README.md) in the same pull request.

## Device testing

Results from real hardware are especially useful. The checks are in
[docs/benchmarks/protocol.md](docs/benchmarks/protocol.md#device-scorecard), and the status of each
device is in [docs/platforms/capabilities.md](docs/platforms/capabilities.md).

## Security

Please don't report vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).
