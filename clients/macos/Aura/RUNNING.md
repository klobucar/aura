# Xcode + Rust UniFFI Integration Guide

This document explains how to configure Xcode to automatically build the Rust core library when you press "Run".

## Prerequisites

- Xcode 15+
- Rust toolchain (`rustup`)
- The `aarch64-apple-darwin` target: `rustup target add aarch64-apple-darwin`

---

## Step 1: Add Run Script Build Phase

1. Open **Aura.xcodeproj** in Xcode
2. Select the **Aura** target
3. Go to **Build Phases** tab
4. Click **+** → **New Run Script Phase**
5. **Drag it ABOVE** "Compile Sources" phase (this is critical!)
6. Name it: `Build Rust Core`
7. Paste this script:

```bash
"${SRCROOT}/../../scripts/build_macos.sh"
```

8. Uncheck **"Based on dependency analysis"** (we always want to check Rust)

---

## Step 2: Configure Library Search Paths

1. Go to **Build Settings** tab
2. Search for `Library Search Paths`
3. Add (for Debug and Release):

```
$(SRCROOT)/Aura/Generated
```

---

## Step 3: Link the Static Library

1. Go to **Build Phases** tab
2. Expand **Link Binary With Libraries**
3. Click **+** → **Add Other...** → **Add Files...**
4. Navigate to `clients/macos/Aura/Generated/`
5. Select `libaura_core.a`
6. Click **Add**

If the file doesn't exist yet, run the build script manually first:
```bash
./scripts/build_macos.sh
```

---

## Step 4: Update Bridging Header

The bridging header at `Aura/Aura-Bridging-Header.h` should import the UniFFI C header:

```c
#import "Generated/aura_coreFFI.h"
```

---

## Step 5: Import in Swift

In any Swift file, import the generated module:

```swift
import aura_core  // If you added the module map
// OR just use the types directly - they're in Generated/aura_core.swift
```

The `AuraBridge.swift` wrapper is already set up in `Aura/Core/`.

---

## Usage

Now when you press **⌘R** (Run) in Xcode:

1. The Run Script phase executes `build_macos.sh`
2. Rust library is compiled for your architecture
3. UniFFI generates fresh Swift bindings
4. Xcode compiles Swift code with the new bindings
5. Xcode links against `libaura_core.a`
6. App launches!

---

## Troubleshooting

### "Library not found"
Run the build script manually first:
```bash
cd /Users/crabclaw/src/aura
./scripts/build_macos.sh
```

### "Symbol not found"
Ensure the static library is linked in Build Phases → Link Binary With Libraries.

### Swift compiler errors
The UniFFI bindings may have changed. Clean build: **⌘⇧K** then **⌘B**.

### Slow builds
The script only rebuilds if source changed. For faster iteration, use:
```bash
./scripts/build_macos.sh debug
```
