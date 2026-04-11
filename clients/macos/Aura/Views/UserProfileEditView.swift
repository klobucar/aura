import SwiftUI

struct UserProfileEditView: View {
    @Environment(\.dismiss) var dismiss
    @ObservedObject var profileManager: ProfileManager
    
    let profile: UserProfileModel?
    
    @State private var displayName: String
    @State private var identity: UserIdentity?
    @State private var requiresBiometric: Bool
    
    init(profileManager: ProfileManager, profile: UserProfileModel? = nil) {
        self.profileManager = profileManager
        self.profile = profile
        _displayName = State(initialValue: profile?.displayName ?? "")
        _requiresBiometric = State(initialValue: profile?.requiresBiometric ?? false)
        
        // Load existing identity from keychain if editing
        if let profile = profile {
            _identity = State(initialValue: UserIdentity.loadFromKeychain(id: profile.id, requiresBiometric: profile.requiresBiometric))
        }
    }
    
    var body: some View {
        VStack(spacing: 20) {
            Text(profile == nil ? "Create Profile" : "Edit Profile")
                .font(.title2.bold())
            
            VStack(spacing: 16) {
                // Display Name
                VStack(alignment: .leading, spacing: 6) {
                    Text("Display Name")
                        .font(.caption.bold())
                        .foregroundStyle(.secondary)
                    TextField("My Profile", text: $displayName)
                        .textFieldStyle(.roundedBorder)
                }
                
                // Public Key (read-only)
                if let identity = identity {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Public Key")
                            .font(.caption.bold())
                            .foregroundStyle(.secondary)
                        Text(identity.publicKeyHex)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.secondary.opacity(0.1))
                            .clipShape(.rect(cornerRadius: 6))
                    }
                } else {
                    Text("A new keypair will be generated")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .padding()
                        .frame(maxWidth: .infinity)
                        .background(Color.blue.opacity(0.1))
                        .clipShape(.rect(cornerRadius: 6))
                }
                
                // Biometric Protection Toggle
                VStack(alignment: .leading, spacing: 6) {
                    Toggle("Require biometric authentication", isOn: $requiresBiometric)
                        .font(.system(size: 13, weight: .medium))
                    
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                        Text("Adds extra security but requires Touch ID/Face ID each time")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.leading, 4)
                }
                .padding(.top, 8)
            }
            .padding()
            .auraGlass()
            
            // Buttons
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.bordered)
                
                Spacer()
                
                Button("Save") {
                    saveProfile()
                }
                .buttonStyle(.borderedProminent)
                .disabled(displayName.isEmpty)
            }
        }
        .padding(30)
        .frame(width: 400)
        .onAppear {
            if identity == nil {
                // Generate new identity for new profile
                let newIdentity = UserIdentity()
                newIdentity.id = UUID()
                newIdentity.displayName = displayName
                newIdentity.generateKeypair()
                identity = newIdentity
            }
        }
    }
    
    private func saveProfile() {
        guard let identity = identity else { return }
        
        identity.displayName = displayName
        
        if let existing = profile {
            // Update existing
            var updated = existing
            updated.displayName = displayName
            updated.requiresBiometric = requiresBiometric
            profileManager.updateProfile(updated)
            
            // Update keychain
            identity.saveToKeychain(requiresBiometric: requiresBiometric)
        } else {
            // Create new
            identity.saveToKeychain(requiresBiometric: requiresBiometric)
            
            let newProfile = UserProfileModel(
                id: identity.id ?? UUID(),
                displayName: displayName,
                publicKeyHex: identity.publicKeyHex,
                requiresBiometric: requiresBiometric
            )
            profileManager.profiles.append(newProfile)
        }
        
        dismiss()
    }
}
