#!/bin/bash
# Launch script for Aura Desktop Client with QUIC support on macOS

# Set library path for msquic
export DYLD_LIBRARY_PATH="/opt/homebrew/lib:$DYLD_LIBRARY_PATH"

# Enable QUIC in .NET
export DOTNET_SYSTEM_NET_HTTP_SOCKETSHTTPHANDLER_HTTP3SUPPORT=1

echo "Starting Aura Desktop Client..."
echo "DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH"

dotnet run
