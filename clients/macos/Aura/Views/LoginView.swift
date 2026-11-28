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
    
    // TLS / TOFU
    @State private var showingCertAlert = false
    @State private var pendingFingerprint: String?
    
    // Management views
    @State private var showingServerManagement = false
    @State private var showingProfileManagement = false
    @StateObject private var serverManager = ServerManager()
    @StateObject private var profileManager = ProfileManager()
    /// Saved profile to log in with; empty means the default login identity.
    @AppStorage("AuraLoginProfileId") private var loginProfileId: String = ""

    private var selectedProfile: UserProfileModel? {
        UUID(uuidString: loginProfileId).flatMap { id in
            profileManager.profiles.first { $0.id == id }
        }
    }

    private var identityLabel: String {
        if let profile = selectedProfile {
            return "\(profile.displayName) · \(profile.publicKeyHex.prefix(12))…"
        }
        return identity.publicKeyHex.isEmpty ? "Generating..." : "\(identity.publicKeyHex.prefix(16))..."
    }

    /// An invite being followed: prefills the form, and when it carries a
    /// certificate fingerprint, the connection is pinned to it.
    var invite: AuraInviteLink? = nil
    /// Stop following the invite and show the plain login form.
    var onDismissInvite: (() -> Void)? = nil
    var onConnected: ((QuicNetworkClient, UserIdentity) -> Void)?

    /// True when XCTest launched the app as the host of a unit test bundle.
    private static var isHostingUnitTests: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    var body: some View {
        VStack(spacing: 32) {
            if let invite {
                inviteHeader(invite)
            } else {
                logoHeader
            }

            VStack(alignment: .leading, spacing: 8) {
                Text("YOUR IDENTITY")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(.secondary)
                    .kerning(1)
                
                // Which identity to log in with: the default one, or any saved
                // profile — including one restored from a backup, which is
                // only usable if it can be picked here.
                Menu {
                    Button {
                        loginProfileId = ""
                    } label: {
                        Label("Default identity", systemImage: selectedProfile == nil ? "checkmark" : "key")
                    }
                    if !profileManager.profiles.isEmpty {
                        Divider()
                        ForEach(profileManager.profiles) { profile in
                            Button {
                                loginProfileId = profile.id.uuidString
                                displayName = profile.displayName
                            } label: {
                                Label(
                                    "\(profile.displayName) · \(profile.publicKeyHex.prefix(8))…",
                                    systemImage: selectedProfile?.id == profile.id ? "checkmark" : "person.crop.circle"
                                )
                            }
                        }
                    }
                    Divider()
                    Button("Manage Identities…") { showingProfileManagement = true }
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "key.fill")
                            .font(.system(size: 14))
                            .foregroundStyle(AuraTheme.Colors.primary)

                        Text(identityLabel)
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(.primary)
                            .lineLimit(1)

                        Spacer(minLength: 0)

                        Image(systemName: "chevron.up.chevron.down")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .contentShape(Rectangle())
                    .auraGlass(cornerRadius: 10, material: .sidebar)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .help("Choose which identity to log in with")
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
            ServerListView { server in
                serverAddress = server.host
                serverPort = String(server.port)
                serverPassword = server.password ?? ""
                showingServerManagement = false
            }
        }
        .sheet(isPresented: $showingProfileManagement, onDismiss: { profileManager.reload() }) {
            ProfileListView()
        }
        .alert("Untrusted Certificate", isPresented: $showingCertAlert, actions: {
            Button("Trust and Connect") {
                if let fingerprint = pendingFingerprint {
                    AppSettings.shared.trustFingerprint(host: serverAddress, fingerprint: fingerprint)
                    connect()
                }
            }
            Button("Cancel", role: .cancel) {
                isConnecting = false
            }
        }, message: {
            if let fingerprint = pendingFingerprint {
                Text("The server at \(serverAddress) is using an unknown certificate.\n\nSHA256: \(fingerprint)\n\nDo you want to trust this server?")
            }
        })
        .onAppear {
            applyInvite(invite)
        }
        .onChange(of: invite) { _, newInvite in
            applyInvite(newInvite)
        }
        .onAppear {
            // Hosting unit tests, the app never logs in. Leaving the keychain
            // alone there keeps an unattended test run from stopping at a
            // keychain prompt (macOS asks before an app reads an item that a
            // different build of it saved).
            guard !Self.isHostingUnitTests else { return }
            identity.loadOrGenerate()
        }
    }

    // MARK: - Headers

    private var logoHeader: some View {
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
    }

    /// Replaces the logo while following an invite. The channel's name isn't
    /// known until connected (the link carries its id), so this names the host.
    private func inviteHeader(_ invite: AuraInviteLink) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "envelope.open.fill")
                .font(.system(size: 40))
                .foregroundStyle(AuraTheme.Gradients.lushIndigo)
                .modifier(AuraTheme.Shadows.glow(color: AuraTheme.Colors.primary))

            Text(ChatFormatting.inviteHeader(destination: invite.host))
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.text)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

            Button("Not now") { onDismissInvite?() }
                .buttonStyle(.plain)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .help("Use the login form without the invite")
        }
        .padding(.horizontal, 32)
        .padding(.top, 48)
    }

    // MARK: - Connecting

    /// Fill the form from an invite.
    private func applyInvite(_ invite: AuraInviteLink?) {
        guard let invite else { return }
        serverAddress = invite.host
        serverPort = String(invite.port)
        if let password = invite.password {
            serverPassword = password
        }
        errorMessage = nil
    }

    /// The invite's certificate fingerprint, if the form still points at the
    /// invite's server.
    private func invitePin(host: String, port: UInt16) -> String? {
        guard let invite, invite.matches(host: host, port: port) else { return nil }
        return invite.fingerprint
    }
    
    private func connect() {
        isConnecting = true
        errorMessage = nil
        
        // Save settings
        UserDefaults.standard.set(serverAddress, forKey: "AuraServerAddress")
        UserDefaults.standard.set(serverPort, forKey: "AuraServerPort")

        // A saved profile's key is read only now, at connect time, so a
        // biometric-protected one prompts when it is used rather than at launch.
        let loginIdentity: UserIdentity
        if let profile = selectedProfile {
            guard let loaded = UserIdentity.loadFromKeychain(
                id: profile.id,
                requiresBiometric: profile.requiresBiometric
            ) else {
                errorMessage = "Couldn't unlock the identity “\(profile.displayName)”."
                isConnecting = false
                return
            }
            loaded.displayName = displayName
            profileManager.markAsUsed(id: profile.id)
            loginIdentity = loaded
        } else {
            identity.saveDisplayName(displayName)
            loginIdentity = identity
        }

        let port = UInt16(serverPort) ?? 8443
        let pin = invitePin(host: serverAddress, port: port)

        // An invite's fingerprint has to agree with any pin already held for
        // the host. No "trust anyway" here: a mismatch is the attack a pinned
        // invite exists to catch.
        if let pin, let existing = AppSettings.shared.pinnedFingerprint(forHost: serverAddress),
           existing.lowercased() != pin {
            errorMessage = ChatFormatting.inviteCertificateMismatch
            isConnecting = false
            return
        }
        client.expectedCertificateFingerprint = pin
        
        Task {
            do {
                // Connect
                try await client.connect(host: serverAddress, port: port)
                
                // Authenticate
                try await client.authenticate(identity: loginIdentity, serverPassword: serverPassword.isEmpty ? nil : serverPassword)

                // The server proved it holds the invite's certificate; keep
                // the pin only now that the connection worked.
                if let pin {
                    AppSettings.shared.trustFingerprint(host: serverAddress, fingerprint: pin)
                }
                
                // Success - notify parent
                onConnected?(client, loginIdentity)
                
            } catch {
                if pin != nil {
                    errorMessage = client.certificateDidNotMatchPin
                        ? ChatFormatting.inviteCertificateMismatch
                        : error.localizedDescription
                } else if let fingerprint = client.lastUntrustedFingerprint {
                    pendingFingerprint = fingerprint
                    showingCertAlert = true
                } else {
                    errorMessage = error.localizedDescription
                }
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
