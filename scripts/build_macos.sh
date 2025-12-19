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

# Ensure Rust tools are in PATH (especially for Xcode)
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

echo "Environment Diagnostics:"
echo "  PATH: $PATH"
echo "  SHELL: $SHELL"
echo "  USER: $USER"
echo ""

# Verify tools
if ! command -v cargo &> /dev/null; then
    echo "❌ Error: 'cargo' not found in PATH."
    echo "   Search result: $(which -a cargo || echo 'none')"
    exit 1
fi

if ! command -v rustup &> /dev/null; then
    echo "⚠️  Warning: 'rustup' not found in PATH. Automatic target installation will be skipped."
else
    echo "✅ Found rustup: $(command -v rustup)"
fi

echo "✅ Found cargo: $(command -v cargo)"
cargo --version
echo ""

# Build configuration (from Xcode or argument)
if [ -n "$CONFIGURATION" ]; then
    BUILD_CONFIG=$(echo "$CONFIGURATION" | tr '[:upper:]' '[:lower:]')
else
    BUILD_CONFIG="${1:-release}"
fi

# Rust build profile & flags
if [ "$BUILD_CONFIG" = "debug" ]; then
    RUST_PROFILE="debug"
    CARGO_FLAGS=""
    # Disable bitcode and set deployment target to avoid LLVM mismatches
    export MACOSX_DEPLOYMENT_TARGET=15.0
    export RUSTFLAGS="-C embed-bitcode=no -C lto=off"
else
    RUST_PROFILE="release"
    CARGO_FLAGS="--release"
    export MACOSX_DEPLOYMENT_TARGET=15.0
    export RUSTFLAGS="-C embed-bitcode=no"
fi

echo "==========================================="
echo "Aura macOS Build"
echo "==========================================="
echo "Config:      $BUILD_CONFIG"
echo "Architectures: $ARCHS"
echo "Output:      $OUTPUT_DIR"
echo "==========================================="

# Determine Rust targets based on architectures
ARCH_ARRAY=($ARCHS)
if [ ${#ARCH_ARRAY[@]} -eq 0 ]; then
    # Default to host architecture if not specified
    ARCH_ARRAY=("$(uname -m)")
    # Normalize uname names to xcode names
    if [ "${ARCH_ARRAY[0]}" = "arm64" ]; then ARCH_ARRAY=("arm64"); fi
    if [ "${ARCH_ARRAY[0]}" = "x86_64" ]; then ARCH_ARRAY=("x86_64"); fi
fi

# Build for each architecture
BUILT_LIBS_A=()
BUILT_LIBS_DYLIB=()

cd "$REPO_ROOT"

for ARCH in "${ARCH_ARRAY[@]}"; do
    case $ARCH in
        x86_64)
            TARGET="x86_64-apple-darwin"
            ;;
        arm64)
            TARGET="aarch64-apple-darwin"
            ;;
        *)
            echo "⚠️  Unsupported architecture: $ARCH, skipping"
            continue
            ;;
    esac
    
    echo ""
    echo "📦 Checking Rust target for $ARCH ($TARGET)..."
    
    echo ""
    echo "📦 Checking Rust target for $ARCH ($TARGET)..."
    
    # Check if target is already installed (only if rustup is available)
    if command -v rustup &> /dev/null; then
        if ! rustup target list --installed | grep -q "$TARGET"; then
            echo "🔧 Target $TARGET missing, attempting to install..."
            rustup target add "$TARGET" || echo "⚠️  Warning: rustup target add failed. Trying cargo build anyway..."
        fi
    else
        echo "⚠️  rustup not found, skipping target check and trying cargo build..."
    fi
    
    # Build
    echo "🚀 Building for $ARCH ($TARGET)..."
    if ! cargo build -p aura-core --target "$TARGET" $CARGO_FLAGS; then
        echo "❌ Error: Failed to build for $ARCH ($TARGET)."
        echo "   Try running 'cargo build --target $TARGET' manually in your terminal."
        continue
    fi
    
    BUILT_LIBS_A+=("$REPO_ROOT/target/$TARGET/$RUST_PROFILE/libaura_core.a")
    BUILT_LIBS_DYLIB+=("$REPO_ROOT/target/$TARGET/$RUST_PROFILE/libaura_core.dylib")
done

echo ""
echo "🔗 Creating Universal (Fat) Libraries..."

# Create output directory
mkdir -p "$OUTPUT_DIR"

if [ ${#BUILT_LIBS_A[@]} -eq 0 ]; then
    echo "❌ Error: No libraries were built."
    echo "   Ensure you have the required Rust targets installed by running:"
    for ARCH in "${ARCH_ARRAY[@]}"; do
        case $ARCH in
            x86_64) echo "   rustup target add x86_64-apple-darwin" ;;
            arm64) echo "   rustup target add aarch64-apple-darwin" ;;
        esac
    done
    exit 1
fi

if [ ${#BUILT_LIBS_A[@]} -gt 1 ]; then
    lipo -create "${BUILT_LIBS_A[@]}" -output "$OUTPUT_DIR/libaura_core.a"
    lipo -create "${BUILT_LIBS_DYLIB[@]}" -output "$OUTPUT_DIR/libaura_core.dylib"
    echo "✅ Created Universal binaries"
else
    # Verify the file exists before copying
    if [ ! -f "${BUILT_LIBS_A[0]}" ]; then
        echo "❌ Error: Static library not found at ${BUILT_LIBS_A[0]}"
        exit 1
    fi
    cp "${BUILT_LIBS_A[0]}" "$OUTPUT_DIR/libaura_core.a"
    cp "${BUILT_LIBS_DYLIB[0]}" "$OUTPUT_DIR/libaura_core.dylib"
    echo "✅ Copied single-arch binaries"
fi

# Fix install name for dylib
install_name_tool -id "@rpath/libaura_core.dylib" "$OUTPUT_DIR/libaura_core.dylib"


# =============================================================================
# Step 2: Generate UniFFI Bindings
# =============================================================================

echo ""
echo "🔧 Generating UniFFI bindings..."

# Use the first built dylib (or the fat one) for binding generation
cargo run -p aura-core --bin uniffi-bindgen generate \
    --library "$OUTPUT_DIR/libaura_core.dylib" \
    --language swift \
    --out-dir "$OUTPUT_DIR"

echo "✅ Swift bindings generated"

# =============================================================================
# Step 3: Create Module Map (for Xcode)
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
# Step 4: Sync to Shared Location (Fixed for Hardcoded Paths)
# =============================================================================

# Always sync to target/release so Xcode can find the library via its legacy search paths if they exist
echo ""
echo "🔄 Syncing libraries to target/release/..."
mkdir -p "$REPO_ROOT/target/release"
cp "$OUTPUT_DIR/libaura_core.a" "$REPO_ROOT/target/release/libaura_core.a"
cp "$OUTPUT_DIR/libaura_core.dylib" "$REPO_ROOT/target/release/libaura_core.dylib"
echo "✅ Library synced to target/release/"

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
