//
//  ContentView.swift
//  Aura
//

import SwiftUI

struct ContentView: View {
    @State private var isConnected = false
    @State private var client: QuicNetworkClient?
    @State private var identity: UserIdentity?
    @StateObject private var audioCapture = AudioCapture()
    @State private var isMicEnabled = false
    
    var body: some View {
        if isConnected, let client = client, let identity = identity {
            connectedView(client: client, identity: identity)
        } else {
            LoginView { connectedClient, connectedIdentity in
                self.client = connectedClient
                self.identity = connectedIdentity
                self.isConnected = true
            }
        }
    }
    
    @ViewBuilder
    private func connectedView(client: QuicNetworkClient, identity: UserIdentity) -> some View {
        NavigationSplitView {
            // Sidebar
            VStack(alignment: .leading, spacing: 0) {
                // User info
                HStack(spacing: 12) {
                    Circle()
                        .fill(LinearGradient(colors: [.blue, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                        .frame(width: 40, height: 40)
                        .overlay(
                            Text(identity.displayName.prefix(1).uppercased())
                                .font(.headline)
                                .foregroundColor(.white)
                        )
                    
                    VStack(alignment: .leading, spacing: 2) {
                        Text(identity.displayName)
                            .font(.headline)
                        Text(client.connectionStatus)
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    
                    Spacer()
                    
                    Button(action: {
                        audioCapture.stop()
                        client.disconnect()
                        self.client = nil
                        self.identity = nil
                        isConnected = false
                    }) {
                        Image(systemName: "rectangle.portrait.and.arrow.right")
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help("Disconnect")
                }
                .padding()
                .background(Color(nsColor: .controlBackgroundColor))
                
                Divider()
                
                // Channels
                List {
                    Section("Voice Channels") {
                        Button(action: {
                            Task {
                                try? await client.joinChannel(1)
                            }
                        }) {
                            Label("General", systemImage: "speaker.wave.2")
                        }
                    }
                }
                .listStyle(.sidebar)
            }
            .frame(minWidth: 220)
            .toolbar {
                ToolbarItem(placement: .navigation) {
                    HStack {
                        Image(systemName: "wave.3.right.circle.fill")
                            .foregroundStyle(.linearGradient(colors: [.blue, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                        Text("Aura")
                            .font(.headline)
                    }
                }
            }
        } detail: {
            // Main content
            VStack(spacing: 0) {
                Spacer()
                
                // Mic status
                VStack(spacing: 24) {
                    ZStack {
                        Circle()
                            .fill(isMicEnabled ? Color.green.opacity(0.1) : Color.secondary.opacity(0.05))
                            .frame(width: 120, height: 120)
                        
                        Image(systemName: isMicEnabled ? "mic.fill" : "mic.slash.fill")
                            .font(.system(size: 48))
                            .foregroundColor(isMicEnabled ? .green : .secondary)
                    }
                    
                    VStack(spacing: 8) {
                        Text(isMicEnabled ? "Transmitting" : "Microphone Muted")
                            .font(.title2)
                            .fontWeight(.medium)
                        
                        if isMicEnabled {
                            Text("\(audioCapture.packetsSent) packets sent")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    }
                }
                
                Spacer()
                
                // Controls
                HStack(spacing: 16) {
                    Button(action: toggleMic) {
                        Label(isMicEnabled ? "Mute" : "Unmute", 
                              systemImage: isMicEnabled ? "mic.fill" : "mic.slash")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(isMicEnabled ? .green : .secondary)
                    .controlSize(.large)
                }
                .padding(.bottom, 32)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color(nsColor: .textBackgroundColor))
        }
        .navigationTitle("Aura")
    }
    
    private func toggleMic() {
        guard let client = client else { return }
        
        if isMicEnabled {
            audioCapture.stop()
            isMicEnabled = false
        } else {
            audioCapture.start { pcmData in
                Task {
                    try? await client.sendAudioDatagram(pcmData)
                }
            }
            isMicEnabled = true
        }
    }
}

#Preview {
    ContentView()
}
