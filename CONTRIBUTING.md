# Contributing to Aura

Thanks for your interest. Aura is pre-alpha and small enough that "open an issue or a PR" is the whole process — but a few notes will save us both time.

## Before you start

- Read the [README](README.md) for what the project is and what currently works.
- Skim [`docs/ROADMAP.md`](docs/ROADMAP.md) to see what's planned.
- Skim [`docs/protocol.md`](docs/protocol.md) and [`docs/MLS_SECURITY.md`](docs/MLS_SECURITY.md) before changing anything in `aura-protocol` or the crypto path.
- For **security** issues, do **not** file a public issue — see [SECURITY.md](SECURITY.md).

## Bug reports and feature requests

File a GitHub issue. For bugs, please include:

- What you ran (server commit, client version, OS).
- What you expected and what you saw instead.
- Logs (`RUST_LOG=aura=debug` is a good starting point) or a reproducer if you have one.

For feature requests, describe the use case before the implementation — "I want X so that Y" lands better than a pre-baked design.

## Pull requests

Small, focused PRs land fastest. A few guidelines:

- **One logical change per PR.** If you find unrelated cleanup along the way, a follow-up PR is easier to review.
- **Tests should pass** — `cargo test --workspace` locally before pushing. CI will run the same plus `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
- **Format and lint clean.** `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` should both be silent.
- **Wire-format changes** (`crates/aura-protocol/`) need a doc update in `docs/protocol.md` and a note in the PR description about migration impact for clients.
- **Crypto changes** (DAVE, MLS, auth, identity) need a reviewer who is comfortable in that area and a corresponding update to `docs/05_dave_protocol_deviations.md`, `docs/MLS_SECURITY.md`, or `docs/08_security_review.md` as appropriate.
- **Macros and `unsafe`** are high-scrutiny — please justify them in the PR description.

### Commit messages

Aura uses [**Conventional Commits**](https://www.conventionalcommits.org/en/v1.0.0/). CI runs `commitlint` on every PR (`.github/workflows/commitlint.yml`) and will fail if commits don't conform.

Format:

```
<type>(<optional scope>): <short summary>

<optional body — explain why, not what>

<optional footers — Fixes #123, Signed-off-by, etc.>
```

Allowed types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.

Examples from this repo:

```
docs: rewrite README around honest pre-alpha status
chore(deps): bump direct + transitive Rust crates
build(opus): add subtree-pull script + vendoring docs
```

Other rules:

- Imperative mood, lowercase summary, no trailing period, under 72 chars.
- Body lines wrapped at ~100 chars.
- Reference the issue if there is one (`Fixes #123`).
- Explain *why*, not just *what*. The diff already shows what.
- **Do not add `Co-Authored-By: Claude` (or other AI co-author trailers) to commits.** Aura is partly AI-assisted; that's noted once at the bottom of the README. Per-commit trailers are noise.

### Sign your work — Developer Certificate of Origin (DCO)

Aura uses the [Developer Certificate of Origin](https://developercertificate.org/) instead of a CLA. By adding a `Signed-off-by` line to your commit, you are certifying that you wrote the patch (or otherwise have the right to submit it under the project's Apache-2.0 license).

Add the trailer with `git commit -s` or `git commit --signoff`:

```
Signed-off-by: Your Name <your.email@example.com>
```

The name and email must match your git identity. PRs without DCO sign-off will be asked to amend.

## Building and testing locally

The full quickstart is in the [README](README.md#quickstart-run-a-server-and-connect-two-clients-locally). The short version:

```bash
# Workspace tests
cargo test --workspace

# Lints
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Run a local server
cargo run -p aura-server
```

The macOS client builds via Xcode (`open clients/macos/Aura.xcodeproj`); the desktop client builds via `dotnet run` from `clients/desktop/` after `aura-core` has been built for the host triple. See the README for details.

## Code style

- Rust: idiomatic, `cargo fmt`-clean, no `#[allow(...)]` without a comment explaining why.
- Swift / C#: match the surrounding file's conventions.
- Public Rust APIs in `aura-core` and `aura-protocol` should have rustdoc comments. Internal functions don't need them unless the *why* is non-obvious.

## License

By contributing, you agree your contributions are licensed under the [Apache License, Version 2.0](LICENSE), the same license as the project.
