import SwiftUI
import UniformTypeIdentifiers

struct LoginView: View {
    @StateObject private var identity = UserIdentity()
    @StateObject private var client = QuicNetworkClient()
    
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
    
    var onConnected: ((QuicNetworkClient, UserIdentity) -> Void)?
    
    var body: some View {
        VStack(spacing: 24) {
            // Logo / Header
            VStack {
                Image(systemName: "wave.3.right.circle.fill")
                    .font(.system(size: 64))
                    .foregroundStyle(.linearGradient(colors: [.blue, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                Text("Aura")
                    .font(.largeTitle)
                    .fontWeight(.bold)
                Text("Zero-Trust Voice")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding(.top, 40)
            
            // Identity Info
            VStack(alignment: .leading, spacing: 8) {
                Text("Your Identity")
                    .font(.headline)
                    .foregroundColor(.secondary)
                
                HStack {
                    Image(systemName: "key.fill")
                        .foregroundColor(.blue)
                    Text(identity.publicKeyHex.isEmpty ? "Generating..." : "\(identity.publicKeyHex.prefix(16))...")
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.primary)
                }
                .padding(10)
                .background(VisualEffectBlur(material: .sidebar, blendingMode: .withinWindow).cornerRadius(8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.white.opacity(0.1), lineWidth: 1))
            }
            .padding(.horizontal)
            
            // Form Fields
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Server Address")
                        .font(.headline)
                        .foregroundColor(.secondary)
                    HStack(spacing: 8) {
                        TextField("127.0.0.1", text: $serverAddress)
                            .textFieldStyle(.plain)
                            .padding(10)
                            .background(VisualEffectBlur(material: .sidebar, blendingMode: .withinWindow).cornerRadius(8))
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.white.opacity(0.1), lineWidth: 1))
                        
                        TextField("8443", text: $serverPort)
                            .textFieldStyle(.plain)
                            .frame(width: 60)
                            .padding(10)
                            .background(VisualEffectBlur(material: .sidebar, blendingMode: .withinWindow).cornerRadius(8))
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.white.opacity(0.1), lineWidth: 1))
                    }
                }
                
                VStack(alignment: .leading, spacing: 8) {
                    Text("Display Name")
                        .font(.headline)
                        .foregroundColor(.secondary)
                    TextField("Your name", text: $displayName)
                        .textFieldStyle(.plain)
                        .padding(10)
                        .background(VisualEffectBlur(material: .sidebar, blendingMode: .withinWindow).cornerRadius(8))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.white.opacity(0.1), lineWidth: 1))
                }
                
                VStack(alignment: .leading, spacing: 8) {
                    Text("Server Password")
                        .font(.headline)
                        .foregroundColor(.secondary)
                    SecureField("Optional", text: $serverPassword)
                        .textFieldStyle(.plain)
                        .padding(10)
                        .background(VisualEffectBlur(material: .sidebar, blendingMode: .withinWindow).cornerRadius(8))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.white.opacity(0.1), lineWidth: 1))
                }
            }
            .padding(.horizontal)
            
            // Connect Button
            Button(action: connect) {
                HStack {
                    if isConnecting {
                        ProgressView()
                            .controlSize(.small)
                            .padding(.trailing, 4)
                    }
                    Text("Connect")
                }
                .font(.headline)
                .foregroundColor(.white)
                .frame(maxWidth: .infinity)
                .padding()
                .background(isConnecting ? Color.gray : Color.blue)
                .cornerRadius(12)
            }
            .padding(.horizontal)
            .disabled(displayName.isEmpty || serverAddress.isEmpty || isConnecting)
            
            // Status
            if let error = errorMessage {
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            } else if !client.connectionStatus.isEmpty && client.connectionStatus != "Disconnected" {
                Text(client.connectionStatus)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
        }
        .frame(width: 400, height: 550)
        .background(VisualEffectBlur(material: .hudWindow, blendingMode: .behindWindow))
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

// MARK: - Visual Effect Blur

struct VisualEffectBlur: NSViewRepresentable {
    var material: NSVisualEffectView.Material
    var blendingMode: NSVisualEffectView.BlendingMode
    
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        return view
    }
    
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
    }
}

#Preview {
    LoginView()
}
