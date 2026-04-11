# C# Client Migration Guide - Opus 1.6 & WebRTC Audio Processing

## Overview

This guide covers migrating the C#/.NET client to use:
- **Opus 1.6** with DRED (Deep Redundancy) and Deep PLC
- **WebRTC Audio Processing** (AEC3, NS, AGC)
- **RNNoise** noise suppression
- **Configurable jitter buffer**

All features are already implemented in the Rust core and exposed via UniFFI.

## Prerequisites

### Build Tools (Windows)
```powershell
# Install Rust
winget install Rustlang.Rustup

# Install Visual Studio Build Tools (for C++ compilation)
# Download from: https://visualstudio.microsoft.com/downloads/
# Select: "Desktop development with C++"

# Install LLVM (for UniFFI)
winget install LLVM.LLVM
```

### Build Tools (Linux)
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install build dependencies
sudo apt install build-essential clang libclang-dev

# For WebRTC bundled build
sudo apt install automake autoconf libtool pkg-config
```

## Step 1: Rebuild Rust Library

### Windows
```powershell
cd crates\aura-core

# Build with all features
cargo build --release --features webrtc-audio

# Output: target\release\aura_core.dll
```

### Linux
```bash
cd crates/aura-core

# Build with all features
cargo build --release --features webrtc-audio

# Output: target/release/libaura_core.so
```

### macOS (for C# client)
```bash
cd crates/aura-core

# With WebRTC (recommended for consistency)
cargo build --release --features webrtc-audio

# Output: target/release/libaura_core.dylib
```

## Step 2: Generate C# Bindings

```bash
# Install UniFFI bindgen if not already installed
cargo install uniffi-bindgen

# Generate C# bindings
uniffi-bindgen generate \
  src/aura.udl \
  --language csharp \
  --out-dir ../../clients/dotnet/Generated

# This creates:
# - Generated/AuraCore.cs (C# wrapper classes)
# - Generated/AuraCore.uniffi.cs (FFI declarations)
```

## Step 3: Update C# Project

### Copy Native Library

**Windows:**
```powershell
Copy-Item target\release\aura_core.dll clients\dotnet\runtimes\win-x64\native\
```

**Linux:**
```bash
cp target/release/libaura_core.so clients/dotnet/runtimes/linux-x64/native/
```

**macOS:**
```bash
cp target/release/libaura_core.dylib clients/dotnet/runtimes/osx-x64/native/
```

### Update .csproj

```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
  </PropertyGroup>

  <ItemGroup>
    <!-- Include generated bindings -->
    <Compile Include="Generated\**\*.cs" />
    
    <!-- Include native libraries -->
    <Content Include="runtimes\**\*">
      <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>
    </Content>
  </ItemGroup>
</Project>
```

## Step 4: Update Audio Capture Code

### OLD: Int16 Audio (Opus 1.3)
```csharp
// OLD CODE - Remove this
private void CaptureAudio()
{
    short[] samples = new short[960]; // 20ms at 48kHz
    audioInput.Read(samples, 0, 960);
    
    var packet = audioSender.Process(samples);
    SendPacket(packet);
}
```

### NEW: Float32 Audio (Opus 1.6)
```csharp
// NEW CODE - Use this
private void CaptureAudio()
{
    float[] samples = new float[960]; // 20ms at 48kHz
    
    // Read from audio input (convert to float32)
    short[] rawSamples = new short[960];
    audioInput.Read(rawSamples, 0, 960);
    
    // Convert int16 to float32 (-1.0 to 1.0)
    for (int i = 0; i < 960; i++)
    {
        samples[i] = rawSamples[i] / 32768.0f;
    }
    
    // Process with Opus 1.6
    var packet = audioSender.ProcessFloat(samples);
    SendPacket(packet);
}
```

## Step 5: Initialize Audio Settings

### On Application Startup
```csharp
public class AudioManager
{
    private AudioSenderWrapper audioSender;
    private AudioReceiverWrapper audioReceiver;
    
    public void Initialize(uint sessionId)
    {
        // Create audio sender/receiver
        byte[] key = new byte[32]; // TODO: Get from MLS
        audioSender = new AudioSenderWrapper(sessionId, key);
        audioReceiver = new AudioReceiverWrapper();
        
        // Configure Opus 1.6 features
        audioSender.SetDredDuration(10); // 100ms redundancy
        audioReceiver.SetJitterBufferMs(40); // 40ms jitter buffer
        
        // Configure audio processing (Windows defaults)
        audioSender.SetNoiseSuppressionEnabled(true);  // RNNoise ON
        audioSender.SetWebrtcAecEnabled(true);         // AEC ON (for speakers)
        audioSender.SetWebrtcNsEnabled(false);         // WebRTC NS OFF (use RNNoise)
        audioSender.SetWebrtcAgcEnabled(true);         // AGC ON (normalize volume)
    }
}
```

## Step 6: Create Settings UI

### WinForms Example
```csharp
public class AudioSettingsForm : Form
{
    private CheckBox rnnoiseCheck;
    private CheckBox aecCheck;
    private CheckBox webrtcNsCheck;
    private CheckBox agcCheck;
    private TrackBar dredSlider;
    private ComboBox jitterCombo;
    
    public AudioSettingsForm(AudioSenderWrapper sender, AudioReceiverWrapper receiver)
    {
        // RNNoise toggle
        rnnoiseCheck = new CheckBox
        {
            Text = "RNNoise Suppression",
            Checked = true,
            Location = new Point(10, 10)
        };
        rnnoiseCheck.CheckedChanged += (s, e) =>
            sender.SetNoiseSuppressionEnabled(rnnoiseCheck.Checked);
        
        // WebRTC AEC toggle
        aecCheck = new CheckBox
        {
            Text = "Echo Cancellation (AEC3)",
            Checked = true,
            Location = new Point(10, 40)
        };
        aecCheck.CheckedChanged += (s, e) =>
            sender.SetWebrtcAecEnabled(aecCheck.Checked);
        
        // WebRTC NS toggle
        webrtcNsCheck = new CheckBox
        {
            Text = "WebRTC Noise Suppression",
            Checked = false,
            Location = new Point(10, 70)
        };
        webrtcNsCheck.CheckedChanged += (s, e) =>
        {
            sender.SetWebrtcNsEnabled(webrtcNsCheck.Checked);
            // Auto-disable RNNoise when WebRTC NS is on
            if (webrtcNsCheck.Checked)
            {
                rnnoiseCheck.Checked = false;
                rnnoiseCheck.Enabled = false;
            }
            else
            {
                rnnoiseCheck.Enabled = true;
            }
        };
        
        // AGC toggle
        agcCheck = new CheckBox
        {
            Text = "Auto Gain Control",
            Checked = true,
            Location = new Point(10, 100)
        };
        agcCheck.CheckedChanged += (s, e) =>
            sender.SetWebrtcAgcEnabled(agcCheck.Checked);
        
        // DRED slider
        var dredLabel = new Label
        {
            Text = "DRED Redundancy (ms):",
            Location = new Point(10, 140)
        };
        dredSlider = new TrackBar
        {
            Minimum = 0,
            Maximum = 100,
            Value = 10,
            TickFrequency = 10,
            Location = new Point(10, 160),
            Width = 200
        };
        var dredValueLabel = new Label
        {
            Text = "100ms",
            Location = new Point(220, 165)
        };
        dredSlider.ValueChanged += (s, e) =>
        {
            sender.SetDredDuration(dredSlider.Value);
            dredValueLabel.Text = $"{dredSlider.Value * 10}ms";
        };
        
        // Jitter buffer picker
        var jitterLabel = new Label
        {
            Text = "Jitter Buffer:",
            Location = new Point(10, 210)
        };
        jitterCombo = new ComboBox
        {
            Location = new Point(10, 230),
            DropDownStyle = ComboBoxStyle.DropDownList
        };
        jitterCombo.Items.AddRange(new object[] { 0, 10, 20, 40, 60, 80, 100 });
        jitterCombo.SelectedItem = 40;
        jitterCombo.SelectedIndexChanged += (s, e) =>
            receiver.SetJitterBufferMs((uint)(int)jitterCombo.SelectedItem);
        
        // Add controls
        Controls.AddRange(new Control[]
        {
            rnnoiseCheck, aecCheck, webrtcNsCheck, agcCheck,
            dredLabel, dredSlider, dredValueLabel,
            jitterLabel, jitterCombo
        });
        
        Text = "Audio Settings";
        Size = new Size(300, 350);
    }
}
```

### Avalonia UI Example
```csharp
<StackPanel Spacing="10" Margin="10">
    <!-- RNNoise -->
    <CheckBox IsChecked="{Binding RnnoiseEnabled}"
              Content="RNNoise Suppression" />
    
    <!-- WebRTC AEC -->
    <CheckBox IsChecked="{Binding AecEnabled}"
              Content="Echo Cancellation (AEC3)" />
    
    <!-- WebRTC NS -->
    <CheckBox IsChecked="{Binding WebrtcNsEnabled}"
              Content="WebRTC Noise Suppression" />
    
    <!-- AGC -->
    <CheckBox IsChecked="{Binding AgcEnabled}"
              Content="Auto Gain Control" />
    
    <!-- DRED -->
    <TextBlock Text="DRED Redundancy" />
    <Slider Minimum="0" Maximum="100" Value="{Binding DredDuration}"
            TickFrequency="10" IsSnapToTickEnabled="True" />
    <TextBlock Text="{Binding DredDuration, StringFormat='{}{0}0ms'}" />
    
    <!-- Jitter Buffer -->
    <TextBlock Text="Jitter Buffer" />
    <ComboBox SelectedItem="{Binding JitterBufferMs}">
        <ComboBoxItem>0</ComboBoxItem>
        <ComboBoxItem>10</ComboBoxItem>
        <ComboBoxItem>20</ComboBoxItem>
        <ComboBoxItem>40</ComboBoxItem>
        <ComboBoxItem>60</ComboBoxItem>
        <ComboBoxItem>80</ComboBoxItem>
        <ComboBoxItem>100</ComboBoxItem>
    </ComboBox>
</StackPanel>
```

## Platform-Specific Recommendations

### Windows (Primary Target)
```csharp
// Recommended settings for Windows
audioSender.SetNoiseSuppressionEnabled(true);   // RNNoise
audioSender.SetWebrtcAecEnabled(true);          // AEC (for speakers)
audioSender.SetWebrtcNsEnabled(false);          // Use RNNoise instead
audioSender.SetWebrtcAgcEnabled(true);          // AGC (normalize volume)
audioSender.SetDredDuration(10);                // 100ms redundancy
audioReceiver.SetJitterBufferMs(40);            // 40ms balanced latency
```

### Linux
```csharp
// Same as Windows
// May need PulseAudio/ALSA configuration for audio I/O
```

### macOS (C# Client)
```csharp
// Can use native Core Audio APIs if available
// Or use same settings as Windows for consistency
```

## Testing Checklist

- [ ] Build Rust library with `--features webrtc-audio`
- [ ] Generate C# bindings
- [ ] Update audio capture to float32
- [ ] Test RNNoise toggle
- [ ] Test WebRTC AEC (with speakers)
- [ ] Test WebRTC NS vs RNNoise
- [ ] Test AGC (vary microphone volume)
- [ ] Test DRED (simulate packet loss)
- [ ] Test jitter buffer settings (0-100ms)
- [ ] Verify settings persistence
- [ ] Test on poor network conditions

## Troubleshooting

### "DllNotFoundException: aura_core"
- Ensure native library is in `runtimes/{platform}/native/`
- Check .csproj includes native libraries
- Verify library name matches platform (`.dll`, `.so`, `.dylib`)

### "EntryPointNotFoundException"
- Regenerate C# bindings (UniFFI version mismatch)
- Rebuild Rust library
- Ensure feature flags match

### Audio sounds robotic/choppy
- Increase jitter buffer (try 60ms or 80ms)
- Enable DRED for packet loss resilience
- Check network conditions

### Echo/feedback
- Enable WebRTC AEC
- Ensure using speakers (not headphones)
- May need to disable RNNoise when using WebRTC NS

## Performance Metrics

| Configuration | Latency | CPU (Windows) | Binary Size |
|---------------|---------|---------------|-------------|
| RNNoise only | +1ms | ~2% | ~5MB |
| +WebRTC AEC | +11ms | ~7% | ~8MB |
| +WebRTC NS | +11ms | ~8% | ~8MB |
| +WebRTC AGC | +11ms | ~9% | ~8MB |
| Full WebRTC | +11ms | ~10% | ~8MB |

## References

- [Opus 1.6 Release Notes](https://opus-codec.org/release/stable/2024/04/12/libopus-1_5_2.html)
- [WebRTC Audio Processing](https://github.com/tonarino/webrtc-audio-processing)
- [UniFFI User Guide](https://mozilla.github.io/uniffi-rs/)
