import SwiftUI
import UniformTypeIdentifiers

struct LoginView: View {
    @StateObject private var identity = UserIdentity()
    @StateObject private var appSettings = AppSettings.shared
    @State private var client = QuicNetworkClient()
    
    // Parse old format that might include port
    @State private var serverAddress: String = {
        let saved = UserDefaults.standard.string(forKey: "AuraServerAddress") ?? "127.0.0.1"
        // Strip port if present (old format was "127.0.0.1:9000")
        return saved.components(separatedBy: ":").first ?? saved
    }()
    @State private var serverPort: String = UserDefaults.standard.string(forKey: "AuraServerPort") ?? "8443"
    @State private var displayName: String = UserDefaults.standard.string(forKey: "AuraDisplayName") ?? ""
    @State private var serverPassword: String = ""
    @State private var isConnecting = false
    @State private var errorMessage: String?
    @State private var logoRingScale: CGFloat = 1.0
    
    // Management views
    @State private var showingServerManagement = false
    @State private var showingProfileManagement = false
    @StateObject private var serverManager = ServerManager()
    @StateObject private var profileManager = ProfileManager()
    
    var onConnected: ((QuicNetworkClient, UserIdentity) -> Void)?
    
    var body: some View {
        VStack(spacing: 32) {
            // Logo / Header
            VStack(spacing: 12) {
                ZStack {
                    // Animated ring
                    Circle()
                        .stroke(AuraTheme.Gradients.primary, lineWidth: 2)
                        .frame(width: 100, height: 100)
                        .opacity(0.3)
                        .scaleEffect(logoRingScale)
                        .animation(.easeInOut(duration: 2.5).repeatForever(autoreverses: true), value: logoRingScale)
                    
                    Image(systemName: "wave.3.right.circle.fill")
                        .font(.system(size: 70))
                        .foregroundStyle(AuraTheme.Gradients.lushIndigo)
                        .modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.primary))
                }
                
                VStack(spacing: 4) {
                    Text("Aura")
                        .font(.system(size: 32, weight: .bold))
                    Text("Zero-Trust Voice")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.top, 48)
            .onAppear { logoRingScale = 1.12 }
            
            VStack(alignment: .leading, spacing: 8) {
                Text("YOUR IDENTITY")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(.secondary)
                    .kerning(1)
                
                HStack(spacing: 10) {
                    Image(systemName: "key.fill")
                        .font(.system(size: 14))
                        .foregroundStyle(AuraTheme.Colors.primary)
                    
                    Text(identity.publicKeyHex.isEmpty ? "Generating..." : "\(identity.publicKeyHex.prefix(16))...")
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundStyle(.primary)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .auraGlass(cornerRadius: 10, material: .sidebar)
            }
            .padding(.horizontal, 32)
            
            // Form Fields
            VStack(alignment: .leading, spacing: 20) {
                VStack(alignment: .leading, spacing: 10) {
                    Text("SERVER ADDRESS")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(.secondary)
                        .kerning(1)
                    
                    HStack(spacing: 12) {
                        TextField("127.0.0.1", text: $serverAddress)
                            .textFieldStyle(.plain)
                            .font(.system(size: 14, weight: .medium))
                            .padding(.horizontal, 14)
                            .padding(.vertical, 12)
                            .auraGlass(cornerRadius: 10, material: .sidebar)
                        
                        TextField("8443", text: $serverPort)
                            .textFieldStyle(.plain)
                            .font(.system(size: 14, weight: .medium))
                            .frame(width: 70)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 12)
                            .auraGlass(cornerRadius: 10, material: .sidebar)
                    }
                }
                
                VStack(alignment: .leading, spacing: 10) {
                    Text("DISPLAY NAME")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(.secondary)
                        .kerning(1)
                    
                    TextField("How others see you", text: $displayName)
                        .textFieldStyle(.plain)
                        .font(.system(size: 14, weight: .medium))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                        .auraGlass(cornerRadius: 10, material: .sidebar)
                }
                
                VStack(alignment: .leading, spacing: 10) {
                    Text("SERVER PASSWORD")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(.secondary)
                        .kerning(1)
                    
                    SecureField("Optional", text: $serverPassword)
                        .textFieldStyle(.plain)
                        .font(.system(size: 14, weight: .medium))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                        .auraGlass(cornerRadius: 10, material: .sidebar)
                }
            }
            .padding(.horizontal, 32)
            
            // Connect Button
            Button(action: connect) {
                HStack(spacing: 10) {
                    if isConnecting {
                        ProgressView()
                            .controlSize(.small)
                            .colorInvert()
                            .brightness(1)
                    }
                    Text(isConnecting ? "Connecting..." : "Enter Aura")
                        .font(.system(size: 16, weight: .bold))
                }
                .foregroundStyle(.white)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
                .background(
                    isConnecting ? 
                    AnyShapeStyle(Color.secondary.opacity(0.3)) : 
                    AnyShapeStyle(AuraTheme.Gradients.lushIndigo)
                )
                .clipShape(RoundedRectangle(cornerRadius: 14))
                .modifier(AuraTheme.Shadows.soft())
                .auraFluidHover()
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 32)
            .disabled(displayName.isEmpty || serverAddress.isEmpty || isConnecting)
            
            // Status
            if let error = errorMessage {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            } else if !client.connectionStatus.isEmpty && client.connectionStatus != "Disconnected" {
                Text(client.connectionStatus)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            
            // Management buttons
            HStack(spacing: 12) {
                Button(action: { showingServerManagement = true }) {
                    Label("Servers", systemImage: "server.rack")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                
                Button(action: { showingProfileManagement = true }) {
                    Label("Profiles", systemImage: "person.2.circle")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            .controlSize(.regular)
            .padding(.horizontal, 32)
            .padding(.top, 8)
            
        }
        .padding(.bottom, 48)
        .liquidGlass()
        .sheet(isPresented: $showingServerManagement) {
            ServerListView()
        }
        .sheet(isPresented: $showingProfileManagement) {
            ProfileListView()
        }
        .onAppear {
            identity.loadOrGenerate()
        }
    }
    
    private func connect() {
        isConnecting = true
        errorMessage = nil
        
        // Save settings
        UserDefaults.standard.set(serverAddress, forKey: "AuraServerAddress")
        UserDefaults.standard.set(serverPort, forKey: "AuraServerPort")
        identity.saveDisplayName(displayName)
        
        Task {
            do {
                let port = UInt16(serverPort) ?? 8443
                
                // Connect
                try await client.connect(host: serverAddress, port: port)
                
                // Authenticate
                try await client.authenticate(identity: identity, serverPassword: serverPassword.isEmpty ? nil : serverPassword)
                
                // Success - notify parent
                onConnected?(client, identity)
                
            } catch {
                errorMessage = error.localizedDescription
                print("[LoginView] Connection error: \(error)")
            }
            
            isConnecting = false
        }
    }
}

// MARK: - AppKit Helpers (Moved to View+Modifiers.swift)

#Preview {
    LoginView()
}
