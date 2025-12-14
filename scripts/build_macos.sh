#!/bin/bash
# =============================================================================
# Aura macOS Build Script
# Automatically builds Rust core and generates UniFFI bindings for Xcode
# =============================================================================
#
# This script is designed to be called from an Xcode "Run Script" build phase.
# It will:
#   1. Build the Rust aura-core library for the target architecture
#   2. Generate Swift bindings and C header via uniffi-bindgen
#   3. Copy all artifacts to the Xcode project's Generated/ folder
#
# Usage:
#   ./scripts/build_macos.sh [release|debug]
#
# Environment Variables (set by Xcode):
#   CONFIGURATION - Debug or Release
#   ARCHS - Target architectures (arm64, x86_64)
#   SRCROOT - Path to Xcode project root

set -e  # Exit on any error

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths
RUST_CRATE="$REPO_ROOT/crates/aura-core"
UDL_FILE="$RUST_CRATE/src/aura.udl"
XCODE_PROJECT="$REPO_ROOT/clients/macos"
OUTPUT_DIR="$XCODE_PROJECT/Aura/Generated"

# Build configuration (from Xcode or argument)
if [ -n "$CONFIGURATION" ]; then
    BUILD_CONFIG=$(echo "$CONFIGURATION" | tr '[:upper:]' '[:lower:]')
else
    BUILD_CONFIG="${1:-release}"
fi

# Determine Rust target based on architecture
if [ "$ARCHS" = "x86_64" ]; then
    RUST_TARGET="x86_64-apple-darwin"
elif [ "$ARCHS" = "arm64" ] || [ -z "$ARCHS" ]; then
    RUST_TARGET="aarch64-apple-darwin"
else
    # Default to current machine's architecture
    RUST_TARGET="aarch64-apple-darwin"
fi

# Rust build profile
if [ "$BUILD_CONFIG" = "debug" ]; then
    RUST_PROFILE="debug"
    CARGO_FLAGS=""
else
    RUST_PROFILE="release"
    CARGO_FLAGS="--release"
fi

TARGET_DIR="$REPO_ROOT/target/$RUST_TARGET/$RUST_PROFILE"

echo "==========================================="
echo "Aura macOS Build"
echo "==========================================="
echo "Config:      $BUILD_CONFIG"
echo "Target:      $RUST_TARGET"
echo "Output:      $OUTPUT_DIR"
echo "==========================================="

# =============================================================================
# Step 1: Build Rust Library
# =============================================================================

echo ""
echo "📦 Building Rust library..."

cd "$REPO_ROOT"

# Ensure target is installed
rustup target add "$RUST_TARGET" 2>/dev/null || true

# Build
cargo build -p aura-core --target "$RUST_TARGET" $CARGO_FLAGS

echo "✅ Rust build complete"

# =============================================================================
# Step 2: Generate UniFFI Bindings
# =============================================================================

echo ""
echo "🔧 Generating UniFFI bindings..."

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Generate Swift bindings
cargo run -p aura-core --bin uniffi-bindgen generate \
    "$UDL_FILE" \
    --language swift \
    --out-dir "$OUTPUT_DIR"

echo "✅ Swift bindings generated"

# =============================================================================
# Step 3: Copy Library
# =============================================================================

echo ""
echo "📋 Copying library..."

# Copy static library (for Xcode linking)
STATIC_LIB="$TARGET_DIR/libaura_core.a"
if [ -f "$STATIC_LIB" ]; then
    cp "$STATIC_LIB" "$OUTPUT_DIR/"
    echo "✅ Copied libaura_core.a"
else
    echo "⚠️  Static library not found at $STATIC_LIB"
fi

# Copy dynamic library (for runtime if needed)
DYLIB="$TARGET_DIR/libaura_core.dylib"
if [ -f "$DYLIB" ]; then
    cp "$DYLIB" "$OUTPUT_DIR/"
    echo "✅ Copied libaura_core.dylib"
fi

# =============================================================================
# Step 4: Create Module Map (for Xcode)
# =============================================================================

echo ""
echo "📝 Creating module map..."

# The UniFFI-generated header needs a module map for Swift import
cat > "$OUTPUT_DIR/module.modulemap" << 'EOF'
module aura_coreFFI {
    header "aura_coreFFI.h"
    export *
}
EOF

echo "✅ Module map created"

# =============================================================================
# Done!
# =============================================================================

echo ""
echo "==========================================="
echo "✅ Build complete!"
echo "==========================================="
echo ""
echo "Generated files in $OUTPUT_DIR:"
ls -la "$OUTPUT_DIR"
echo ""
