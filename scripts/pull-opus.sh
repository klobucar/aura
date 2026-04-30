#!/usr/bin/env bash
# Pull a tagged version of xiph/opus into our vendored subtree, then re-apply
# our local pruning (drop dnn/torch/* training scripts that the FFI build
# doesn't compile).
#
# Usage:   ./scripts/pull-opus.sh <ref>
# Example: ./scripts/pull-opus.sh v1.6.2
#
# After this script runs, the model-data checksum in vendor/opus/autogen.sh
# may have moved. If it did, update OPUS_MODEL_CHECKSUM in:
#   - .github/workflows/ci.yml
#   - .github/workflows/macos.yml
#   - .github/workflows/desktop.yml
# The script prints the current checksum at the end so you can compare.

set -euo pipefail

ref="${1:?Usage: $0 <opus-ref>  (e.g. v1.6.2)}"

repo_root="$(git rev-parse --show-toplevel)"
prefix="crates/opus16-sys/vendor/opus"
upstream="https://gitlab.xiph.org/xiph/opus.git"

cd "$repo_root"

# 1. Working tree must be clean — git subtree won't proceed otherwise and we
#    don't want our pruning commit picking up unrelated drift.
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "ERROR: working tree has uncommitted changes; commit or stash first." >&2
    exit 1
fi

# 2. Pull from upstream xiph/opus, squashing into a single merge.
echo ">>> Pulling $ref from $upstream into $prefix..."
git subtree pull \
    --prefix="$prefix" \
    "$upstream" "$ref" \
    --squash \
    -m "merge: pull opus $ref into $prefix"

# 3. Re-prune dnn/torch — these are the PyTorch training scripts upstream
#    ships for re-training the neural-net weights. We don't train models;
#    the FFI build only compiles dnn/*.c (not subdirs). See:
#      docs/development/vendoring-opus.md
if [ -d "$prefix/dnn/torch" ]; then
    echo ">>> Removing $prefix/dnn/torch/ (PyTorch training scripts)..."
    git rm -rq "$prefix/dnn/torch"
    git commit -m "chore(opus): re-prune dnn/torch from $ref subtree pull

Upstream xiph/opus ships PyTorch training scripts under dnn/torch/.
The FFI build doesn't compile any of it, so we strip it after every
subtree pull. See docs/development/vendoring-opus.md."
fi

# 4. Surface the model-data checksum so the maintainer can sync workflows.
checksum="$(awk '/dnn\/download_model.sh/ {gsub(/"/,"",$2); print $2}' \
    "$prefix/autogen.sh")"
echo
echo "Done. Vendored opus is now at $ref."
echo
echo "Model-data checksum (from $prefix/autogen.sh):"
echo "  $checksum"
echo
echo "If this differs from the previous OPUS_MODEL_CHECKSUM, update it in:"
echo "  - .github/workflows/ci.yml"
echo "  - .github/workflows/macos.yml"
echo "  - .github/workflows/desktop.yml"
