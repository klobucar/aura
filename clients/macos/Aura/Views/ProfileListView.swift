import SwiftUI
import UniformTypeIdentifiers

struct ProfileListView: View {
    @Environment(\.dismiss) var dismiss
    @StateObject private var profileManager = ProfileManager()
    @State private var showingAddProfile = false
    @State private var editingProfile: UserProfileModel?
    @State private var showingImport = false
    
    var onSelect: ((UserProfileModel) -> Void)?
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Profiles")
                    .font(.title2.bold())
                Spacer()
                Button(action: { showingImport = true }) {
                    Label("Import", systemImage: "square.and.arrow.down")
                        .font(.caption)
                }
                .buttonStyle(.bordered)
                
                Button(action: { showingAddProfile = true }) {
                    Image(systemName: "plus.circle.fill")
                        .accessibilityLabel("Add")
                        .font(.title2)
                        .foregroundStyle(AuraTheme.Gradients.lushIndigo)
                }
                .buttonStyle(.plain)
            }
            .padding()
            
            Divider()
            
            // Profile List
            ScrollView {
                LazyVStack(spacing: 12) {
                    // Recent
                    if !profileManager.recentProfiles.isEmpty {
                        sectionHeader("Recent")
                        ForEach(profileManager.recentProfiles) { profile in
                            profileRow(profile)
                        }
                    }
                    
                    // All Profiles
                    sectionHeader("All Profiles")
                    ForEach(profileManager.profiles) { profile in
                        profileRow(profile)
                    }
                }
                .padding()
            }
        }
        .frame(width: 450, height: 500)
        .sheet(isPresented: $showingAddProfile) {
            UserProfileEditView(profileManager: profileManager)
        }
        .sheet(item: $editingProfile) { profile in
            UserProfileEditView(profileManager: profileManager, profile: profile)
        }
        .fileImporter(
            isPresented: $showingImport,
            allowedContentTypes: [.json],
            allowsMultipleSelection: false
        ) { result in
            handleImport(result)
        }
    }
    
    private func sectionHeader(_ title: String) -> some View {
        HStack {
            Text(title)
                .font(.caption.bold())
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            Spacer()
        }
        .padding(.top, 8)
    }
    
    private func profileRow(_ profile: UserProfileModel) -> some View {
        HStack(spacing: 12) {
            // Icon
            Circle()
                .fill(AuraTheme.Gradients.primary)
                .frame(width: 40, height: 40)
                .overlay {
                    Text(String(profile.displayName.prefix(1)))
                        .font(.title3.bold())
                        .foregroundStyle(.white)
                }
            
            // Info
            VStack(alignment: .leading, spacing: 4) {
                Text(profile.displayName)
                    .font(.system(size: 14, weight: .semibold))
                Text("\\(profile.publicKeyHex.prefix(16))...")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            
            Spacer()
            
            // Select button
            if onSelect != nil {
                Button("Select") {
                    onSelect?(profile)
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(12)
        .auraGlass(cornerRadius: 10)
        .contextMenu {
            Button(action: { editingProfile = profile }) {
                Label("Edit", systemImage: "pencil")
            }
            Button(action: { exportProfile(profile) }) {
                Label("Export", systemImage: "square.and.arrow.up")
            }
            Button(action: { profileManager.deleteProfile(id: profile.id) }) {
                Label("Delete", systemImage: "trash")
            }
        }
    }
    
    private func exportProfile(_ profile: UserProfileModel) {
        // Load identity from keychain
        guard let identity = UserIdentity.loadFromKeychain(id: profile.id),
              let data = identity.exportProfile() else {
            print("[ProfileListView] Failed to export profile")
            return
        }
        
        // Show save panel
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "\\(profile.displayName).aura"
        panel.allowedContentTypes = [.json]
        
        if panel.runModal() == .OK, let url = panel.url {
            do {
                try data.write(to: url)
                print("[ProfileListView] Exported profile to \\(url.path)")
            } catch {
                print("[ProfileListView] Failed to write export: \\(error)")
            }
        }
    }
    
    private func handleImport(_ result: Result<[URL], Error>) {
        switch result {
        case .success(let urls):
            guard let url = urls.first else { return }
            do {
                let data = try Data(contentsOf: url)
                guard let identity = UserIdentity.importProfile(from: data) else {
                    print("[ProfileListView] Failed to import profile")
                    return
                }
                
                // Save to keychain
                identity.saveToKeychain()
                
                // Create profile model
                let profile = UserProfileModel(
                    id: identity.id ?? UUID(),
                    displayName: identity.displayName,
                    publicKeyHex: identity.publicKeyHex
                )
                
                profileManager.profiles.append(profile)
                print("[ProfileListView] Imported profile: \\(profile.displayName)")
            } catch {
                print("[ProfileListView] Failed to import: \\(error)")
            }
        case .failure(let error):
            print("[ProfileListView] Import error: \\(error)")
        }
    }
}
