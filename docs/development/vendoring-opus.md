---
title: Vendoring xiph/opus
description: How and why Aura vendors a stripped-down copy of libopus, and how to bump it.
---

# Vendoring xiph/opus

Aura vendors `xiph/opus` (currently pinned at v1.6.1) into the repo at
`crates/opus16-sys/vendor/opus/` and builds it from source via `cc-rs` in
`crates/opus16-sys/build.rs`. This page documents what we do and don't ship,
and how to bump the pinned version.

## What's in the vendored copy

We **ship** everything the FFI build actually compiles or links:

- `src/`, `silk/`, `celt/`, `include/` — the core opus encoder/decoder
- `dnn/*.c` and `dnn/*.h` — neural-net glue code (DRED, LACE, NoLACE, FARGAN,
  PLC, BWE, OSCE)

We **don't ship** the upstream tree's PyTorch training tooling at
`dnn/torch/` — about 31K lines of Python across `lpcnet`, `fargan`, `fwgan`,
`rdovae`, `lossgen`, `neural-pitch`, `osce`, `plc`, `dnntools`, and
`testsuite`. The FFI build doesn't compile any of it, and Aura has no need
to retrain models. The directory is removed after every subtree pull (see
`scripts/pull-opus.sh`).

## What's not in git

The trained neural-net weight blobs (`dnn/*_data.{c,h}` and
`dnn/plc_data.{c,h}`) are downloaded at build time by
`dnn/download_model.sh`, which fetches `opus_data-<sha>.tar.gz` from
`media.xiph.org`. The checksum is pinned in `vendor/opus/autogen.sh` and
kept in sync with the `OPUS_MODEL_CHECKSUM` env var across all three GitHub
Actions workflows.

These files are gitignored:

```gitignore
crates/opus16-sys/vendor/opus/dnn/*_data.c
crates/opus16-sys/vendor/opus/dnn/*_data.h
crates/opus16-sys/vendor/opus/dnn/plc_data.c
crates/opus16-sys/vendor/opus/dnn/plc_data.h
crates/opus16-sys/vendor/opus/opus_data-*.tar.gz
```

`build.rs` will fail with `fatal error: plc_data.h: No such file or directory`
if you build before running `vendor/opus/dnn/download_model.sh`. CI handles
this automatically with a cache + conditional download step in each
workflow.

## Bumping the pinned version

Use `scripts/pull-opus.sh`:

```bash
./scripts/pull-opus.sh v1.6.2
```

The script does three things:

1. **Pulls** `<ref>` from `https://gitlab.xiph.org/xiph/opus.git` via
   `git subtree pull --squash`, producing a merge commit.
2. **Prunes** `crates/opus16-sys/vendor/opus/dnn/torch/` again, since the
   subtree pull restores it from upstream. This is its own commit.
3. **Prints** the current model-data checksum from
   `vendor/opus/autogen.sh`. If it changed, update `OPUS_MODEL_CHECKSUM`
   in all three workflow files:
   - `.github/workflows/ci.yml`
   - `.github/workflows/macos.yml`
   - `.github/workflows/desktop.yml`

The script aborts if the working tree isn't clean.

## Why vendor instead of system-link

Three reasons:

- **Opus 1.6 features.** DRED, deep PLC, OSCE, and LACE/NoLACE shipped in
  1.6 are core to Aura's audio quality story. Most distros are still on
  1.4–1.5; vendoring guarantees a known build.
- **Reproducible builds.** We pin the source revision *and* the model-data
  checksum. A user with no opus on their system gets a deterministic
  binary.
- **Cross-platform consistency.** macOS, Windows, and Linux clients all
  link the same opus build with the same compile flags. System opus would
  vary per OS.

The cost is repo size — vendored opus + DNN data is ~50 MB once weights are
downloaded. We accept that for the consistency wins.

## Why prune `dnn/torch`

The upstream xiph repo includes complete PyTorch training pipelines for
every neural-net component opus ships. Maintainers use these to retrain
when they release new model versions; downstream consumers (us) just use
the trained outputs.

If we kept `dnn/torch/`:

- ~31K lines of Python in our git tree
- ~14 MB of additional clone weight
- GitHub language detection would label the repo "mostly Python" — an
  inaccurate first-impression signal for a Rust/Swift VoIP project
- `cargo doc`, `tokei`, and similar tools would show inflated stats

Removing it costs nothing because the build script never invokes any of it.
If you ever need it back, `git checkout v1.6.1 -- dnn/torch` from a fresh
upstream clone gets you the directory.
