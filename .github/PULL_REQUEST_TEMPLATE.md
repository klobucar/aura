<!--
Thanks for the PR!

Before you open: make sure your commit messages follow Conventional Commits
(see CONTRIBUTING.md) — CI will fail otherwise. Sign off with `git commit -s`
to satisfy the DCO check.
-->

## What this changes

<!-- One or two sentences. The diff says *what*; tell us *why*. -->

## Why

<!-- The motivation. Bug? Feature? Cleanup? Link the issue if there is one (Fixes #N). -->

## How tested

<!-- "cargo test --workspace passes" is fine for small changes. For client / wire / crypto changes, say what you actually exercised end-to-end. -->

## Notes for the reviewer

<!-- Optional: anything tricky, anything you're unsure about, anything you deliberately punted. -->

## Checklist

- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/)
- [ ] Commits are signed off (`git commit -s`) per the DCO
- [ ] `cargo fmt --all` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` passes
- [ ] Wire-format / protocol changes have a corresponding update in `docs/protocol.md`
- [ ] Crypto changes have a corresponding update in `docs/MLS_SECURITY.md` or `docs/05_dave_protocol_deviations.md`
