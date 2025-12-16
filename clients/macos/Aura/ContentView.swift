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
    
    // Channel definitions
    private let channels: [Channel] = [
        Channel(id: 1, name: "General", icon: "speaker.wave.2"),
        Channel(id: 2, name: "Gaming", icon: "gamecontroller"),
        Channel(id: 3, name: "Music", icon: "music.note"),
        Channel(id: 4, name: "AFK", icon: "moon.zzz")
    ]
    
    var body: some View {
        Group {
            if isConnected, let client = client, let identity = identity {
                connectedView(client: client, identity: identity)
            } else {
                loginView
            }
        }
    }
    
    // MARK: - Login View (Centered)
    
    @ViewBuilder
    private var loginView: some View {
        ZStack {
            // Background gradient
            LinearGradient(
                colors: [
                    Color(nsColor: .windowBackgroundColor),
                    Color(nsColor: .controlBackgroundColor)
                ],
                startPoint: .top,
                endPoint: .bottom
            )
            .ignoresSafeArea()
            
            // Centered login card
            LoginView { connectedClient, connectedIdentity in
                self.client = connectedClient
                self.identity = connectedIdentity
                self.isConnected = true
            }
            .frame(width: 400, height: 550)
            .clipShape(RoundedRectangle(cornerRadius: 16))
            .shadow(color: .black.opacity(0.3), radius: 20, x: 0, y: 10)
        }
        .frame(minWidth: 500, minHeight: 600)
    }
    
    // MARK: - Connected View
    
    @ViewBuilder
    private func connectedView(client: QuicNetworkClient, identity: UserIdentity) -> some View {
        NavigationSplitView {
            // Sidebar
            VStack(alignment: .leading, spacing: 0) {
                // User info header
                userHeader(identity: identity, client: client)
                
                Divider()
                
                // Channels list
                channelList(client: client)
            }
            .frame(minWidth: 220, maxWidth: 280)
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
            // Main content area
            channelDetailView(client: client)
        }
        .navigationTitle("")
    }
    
    // MARK: - User Header
    
    @ViewBuilder
    private func userHeader(identity: UserIdentity, client: QuicNetworkClient) -> some View {
        HStack(spacing: 12) {
            // Avatar
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
                    .lineLimit(1)
            }
            
            Spacer()
            
            // Disconnect button
            Button(action: disconnect) {
                Image(systemName: "rectangle.portrait.and.arrow.right")
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
            .help("Disconnect")
        }
        .padding()
        .background(Color(nsColor: .controlBackgroundColor))
    }
    
    // MARK: - Channel List
    
    @ViewBuilder
    private func channelList(client: QuicNetworkClient) -> some View {
        let currentId = client.currentChannelId ?? 1
        
        List {
            Section("Voice Channels") {
                ForEach(channels) { channel in
                    VStack(alignment: .leading, spacing: 4) {
                        // Channel header
                        Button(action: {
                            switchChannel(to: channel.id, client: client)
                        }) {
                            HStack {
                                Image(systemName: channel.icon)
                                    .foregroundColor(channel.id == currentId ? .blue : .secondary)
                                    .frame(width: 20)
                                
                                Text(channel.name)
                                    .foregroundColor(channel.id == currentId ? .primary : .secondary)
                                    .fontWeight(channel.id == currentId ? .medium : .regular)
                                
                                Spacer()
                                
                                // User count
                                if let users = client.usersByChannel[channel.id], !users.isEmpty {
                                    Text("\(users.count + (channel.id == currentId ? 1 : 0))")
                                        .font(.caption2)
                                        .foregroundColor(.secondary)
                                } else if channel.id == currentId {
                                    Text("1")
                                        .font(.caption2)
                                        .foregroundColor(.secondary)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                        
                        // Users in this channel
                        if let users = client.usersByChannel[channel.id], !users.isEmpty {
                            VStack(alignment: .leading, spacing: 2) {
                                // Show current user if in this channel
                                if channel.id == currentId {
                                    HStack(spacing: 6) {
                                        Circle()
                                            .fill(isMicEnabled ? Color.green : Color.secondary)
                                            .frame(width: 6, height: 6)
                                        Text("You")
                                            .font(.caption)
                                            .foregroundColor(.secondary)
                                    }
                                    .padding(.leading, 26)
                                }
                                
                                // Show other users
                                ForEach(users) { user in
                                    HStack(spacing: 6) {
                                        Circle()
                                            .fill(Color.green)
                                            .frame(width: 6, height: 6)
                                        Text(user.displayName)
                                            .font(.caption)
                                            .foregroundColor(.secondary)
                                    }
                                    .padding(.leading, 26)
                                }
                            }
                            .padding(.top, 2)
                        } else if channel.id == currentId {
                            // Just show "You" if alone in channel
                            HStack(spacing: 6) {
                                Circle()
                                    .fill(isMicEnabled ? Color.green : Color.secondary)
                                    .frame(width: 6, height: 6)
                                Text("You")
                                    .font(.caption)
                                    .foregroundColor(.secondary)
                            }
                            .padding(.leading, 26)
                            .padding(.top, 2)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
        .listStyle(.sidebar)
    }
    
    // MARK: - Channel Detail View
    
    @ViewBuilder
    private func channelDetailView(client: QuicNetworkClient) -> some View {
        let channel = currentChannel(for: client)
        let userCount = (client.usersByChannel[client.currentChannelId ?? 1]?.count ?? 0) + 1
        
        VStack(spacing: 0) {
            // Channel header
            HStack {
                Image(systemName: channel?.icon ?? "speaker.wave.2")
                    .font(.title2)
                    .foregroundColor(.blue)
                
                Text(channel?.name ?? "Channel")
                    .font(.title2)
                    .fontWeight(.semibold)
                
                Spacer()
                
                // User count badge
                Text("\(userCount)")
                    .font(.caption)
                    .fontWeight(.medium)
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(Color.secondary.opacity(0.1))
                    .clipShape(Capsule())
            }
            .padding()
            .background(Color(nsColor: .controlBackgroundColor).opacity(0.5))
            
            Divider()
            
            Spacer()
            
            // Mic status indicator
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
                Button(action: { toggleMic(client: client) }) {
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
    
    // MARK: - Helpers
    
    private func currentChannel(for client: QuicNetworkClient) -> Channel? {
        let channelId = client.currentChannelId ?? 1
        return channels.first { $0.id == channelId }
    }
    
    private func switchChannel(to channelId: UInt32, client: QuicNetworkClient) {
        guard channelId != client.currentChannelId else { return }
        
        Task {
            do {
                try await client.joinChannel(channelId)
                print("[ContentView] Switched to channel \(channelId)")
            } catch {
                print("[ContentView] Failed to switch channel: \(error)")
            }
        }
    }
    
    private func toggleMic(client: QuicNetworkClient) {
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
    
    private func disconnect() {
        audioCapture.stop()
        client?.disconnect()
        client = nil
        identity = nil
        isConnected = false
        isMicEnabled = false
    }
}

// MARK: - Channel Model

struct Channel: Identifiable, Hashable {
    let id: UInt32
    let name: String
    let icon: String
}

#Preview {
    ContentView()
}
