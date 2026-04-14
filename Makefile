# Aura Build System
# Builds the Rust core and generates UniFFI bindings for Swift and C#

.PHONY: all build-core generate-bindings generate-swift generate-csharp clean

# Paths
RUST_TARGET_DIR := target/release
SWIFT_OUT_DIR := clients/macos/Aura/Generated
CSHARP_OUT_DIR := clients/desktop/Generated
UDL_FILE := crates/aura-core/src/aura.udl

# Default target
all: build-core generate-bindings

# Build the Rust core library (cdylib + staticlib)
build-core:
	@echo "🔨 Building aura-core..."
	cargo build --release -p aura-core
	@echo "✅ Built: $(RUST_TARGET_DIR)/libaura_core.dylib"
	@echo "✅ Built: $(RUST_TARGET_DIR)/libaura_core.a"

# Generate all bindings
generate-bindings: generate-swift generate-csharp
	@echo "✅ All bindings generated"

# Generate Swift bindings for macOS
generate-swift: build-core
	@echo "🍎 Generating Swift bindings..."
	@mkdir -p $(SWIFT_OUT_DIR)
	cargo run -p aura-core --bin uniffi-bindgen generate \
		--library $(RUST_TARGET_DIR)/libaura_core.dylib \
		--language swift \
		--out-dir $(SWIFT_OUT_DIR)
	@echo "✅ Swift bindings: $(SWIFT_OUT_DIR)/aura_core.swift"
	@echo "✅ Swift header: $(SWIFT_OUT_DIR)/aura_coreFFI.h"

# Generate C# bindings for Windows/Desktop
generate-csharp: build-core
	@echo "🪟 Generating C# bindings..."
	@mkdir -p $(CSHARP_OUT_DIR)
	uniffi-bindgen-cs --library $(RUST_TARGET_DIR)/libaura_core.dylib \
		--out-dir $(CSHARP_OUT_DIR)
	@echo "✅ C# bindings: $(CSHARP_OUT_DIR)/aura_core.cs"
	
	# Copy the dynamic library
	@cp $(RUST_TARGET_DIR)/libaura_core.dylib $(CSHARP_OUT_DIR)/ 2>/dev/null || \
	 cp $(RUST_TARGET_DIR)/aura_core.dll $(CSHARP_OUT_DIR)/ 2>/dev/null || true
	@echo "✅ Library copied to $(CSHARP_OUT_DIR)/"

# Copy libraries to client directories
install-libs:
	@echo "📦 Installing libraries..."
	# macOS: Copy static library and header for Xcode
	@cp $(RUST_TARGET_DIR)/libaura_core.a clients/macos/Aura/ 2>/dev/null || true
	@cp $(SWIFT_OUT_DIR)/aura_coreFFI.h clients/macos/Aura/ 2>/dev/null || true
	
	# Desktop: Copy dynamic library for .NET
	@cp $(RUST_TARGET_DIR)/libaura_core.dylib $(CSHARP_OUT_DIR)/ 2>/dev/null || \
	 cp $(RUST_TARGET_DIR)/aura_core.dll $(CSHARP_OUT_DIR)/ 2>/dev/null || true

# Run tests
test:
	@echo "🧪 Running tests..."
	PROTOC=/opt/homebrew/bin/protoc cargo test --workspace

# Run ACME integration tests (requires docker-compose/Pebble)
test-acme:
	@echo "🧪 Starting Pebble ACME server..."
	docker-compose -f docker-compose.test.yml up -d
	@echo "🧪 Running ACME integration tests..."
	PROTOC=/opt/homebrew/bin/protoc cargo test -p aura-server --test acme_tests -- --nocapture
	@echo "🧹 Cleaning up Pebble..."
	docker-compose -f docker-compose.test.yml down

# Clean build artifacts
clean:
	@echo "🧹 Cleaning..."
	cargo clean
	rm -rf $(SWIFT_OUT_DIR)
	rm -rf $(CSHARP_OUT_DIR)

# Development: Quick rebuild and generate
dev: build-core generate-bindings
	@echo "🚀 Ready for development"

# Documentation
docs-serve:
	pip install mkdocs-material
	mkdocs serve

docs-build:
	mkdocs build --strict

# Help
help:
	@echo "Aura Build System"
	@echo ""
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@echo "  all              Build core and generate all bindings (default)"
	@echo "  build-core       Build the Rust aura-core library"
	@echo "  generate-swift   Generate Swift bindings for macOS"
	@echo "  generate-csharp  Generate C# bindings for Windows/Desktop"
	@echo "  install-libs     Copy libraries to client directories"
	@echo "  test             Run all tests"
	@echo "  clean            Clean build artifacts"
	@echo "  dev              Quick rebuild for development"
	@echo "  docs-serve       Serve documentation locally"
	@echo "  docs-build       Build static documentation site"
