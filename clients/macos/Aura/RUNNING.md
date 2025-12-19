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

## Step 3: Link and Embed the Dynamic Library

Aura uses dynamic linking (`.dylib`) to simplify multi-architecture support.

1. Go to **General** tab for the Aura target.
2. Scroll to **Frameworks, Libraries, and Embedded Content**.
3. Click **+** → **Add Other...** → **Add Files...**
4. Navigate to `clients/macos/Aura/Generated/`.
5. Select `libaura_core.dylib`.
6. Click **Add**.
7. **CRITICAL**: Ensure the "Embed" column is set to **Embed & Sign**.

If the file doesn't exist yet, run the build script manually first:
```bash
./scripts/build_macos.sh
```

---
### The Tester's Workaround (Unsigned)
If you send a ZIP of the app, the tester must run this in their terminal to bypass Gatekeeper:
```bash
xattr -cr /path/to/Aura.app
```

4.  (Optional) Change **Architectures** from "Standard Architectures" to `Apple Silicon`.

### Official Way (Signed)

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

### Linker error: x86_64 symbols not found
This happens because Xcode is trying to build for both Intel and Apple Silicon.

### To fix (Building arm64 only):
1.  Go to **Build Settings** in Xcode.
2.  Search for **Architectures**.
3.  Click on "Standard Architectures (Apple Silicon, Intel)".
4.  Select **Other...** at the bottom of the list.
5.  Double-click the existing line and type `arm64`.
6.  Ensure **Build Active Architecture Only** is set to **Yes** for Debug, and optionally **No** for Archive if you want it to work on all ARM Macs.

> [!NOTE]
> Our new build script now supports **Universal Binaries** automatically. If you leave the setting at "Standard Architectures", the script will build for both Intel and ARM.

### Build Error during x86_64 compilation (audiopus_sys)
If the build fails only for `x86_64` with a message about `audiopus_sys` or `Opus`, it's because the C-library compiler needs extra tools.
1.  Install the base build tools:
    ```bash
    brew install cmake pkg-config opus
    ```

### Sandbox: cp deny(1) file-write-data
This happens on Xcode 15+ because of a new security feature "User Script Sandboxing".
1. Go to **Build Settings** for the Aura target.
2. Search for **Enable User Script Sandboxing**.
3. Set it to **No**.

### Slow builds
The script only rebuilds if source changed. For faster iteration, use:
```bash
./scripts/build_macos.sh debug
```
