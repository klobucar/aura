#!/bin/bash
# Launch script for Aura Desktop Client with QUIC support on macOS

# Set library path fallback for msquic and aura-core
export DYLD_FALLBACK_LIBRARY_PATH="/opt/homebrew/lib:$(pwd)/Generated:$DYLD_FALLBACK_LIBRARY_PATH"

# Enable QUIC in .NET
export DOTNET_SYSTEM_NET_HTTP_SOCKETSHTTPHANDLER_HTTP3SUPPORT=1

echo "Starting Aura Desktop Client (.NET 10)..."
echo "DYLD_FALLBACK_LIBRARY_PATH=$DYLD_FALLBACK_LIBRARY_PATH"

dotnet run
