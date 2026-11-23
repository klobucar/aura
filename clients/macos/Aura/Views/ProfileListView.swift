import SwiftUI
import UniformTypeIdentifiers

struct ProfileListView: View {
    @Environment(\.dismiss) var dismiss
    @StateObject private var profileManager = ProfileManager()
    @State private var showingAddProfile = false
    @State private var editingProfile: UserProfileModel?
    @State private var showingImport = false
    /// Profile whose encrypted backup is being made.
    @State private var backingUp: UserProfileModel?
    /// `.aura-identity` file picked for import, waiting for its passphrase.
    @State private var pendingBackup: PendingBackup?
    @State private var importError: String?

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
        .sheet(item: $backingUp) { profile in
            ProfileBackupSheet(profile: profile)
        }
        .sheet(item: $pendingBackup) { backup in
            RestoreBackupSheet(profileManager: profileManager, backup: backup)
        }
        .alert(
            "Couldn't import",
            isPresented: Binding(get: { importError != nil }, set: { if !$0 { importError = nil } }),
            presenting: importError
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { message in
            Text(message)
        }
        .fileImporter(
            isPresented: $showingImport,
            // Encrypted backups, and the old plaintext JSON bundles (v1/v2).
            allowedContentTypes: [IdentityBackup.contentType, .json],
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
                Text("\(profile.publicKeyHex.prefix(16))...")
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
            Button(action: { backingUp = profile }) {
                Label("Export Encrypted Backup…", systemImage: "square.and.arrow.up")
            }
            Button(action: { profileManager.deleteProfile(id: profile.id) }) {
                Label("Delete", systemImage: "trash")
            }
        }
    }
    
    private func handleImport(_ result: Result<[URL], Error>) {
        switch result {
        case .success(let urls):
            guard let url = urls.first else { return }
            let accessing = url.startAccessingSecurityScopedResource()
            defer { if accessing { url.stopAccessingSecurityScopedResource() } }

            let data: Data
            do {
                data = try Data(contentsOf: url)
            } catch {
                print("[ProfileListView] Failed to import: \(error)")
                importError = "The file couldn't be read."
                return
            }

            // Encrypted backup: ask for the passphrase, then add a new profile.
            if url.pathExtension.lowercased() == IdentityBackup.fileExtension || IdentityBackup.looksLikeBackup(data) {
                pendingBackup = PendingBackup(fileName: url.lastPathComponent, data: data)
                return
            }

            importPlaintextBundle(data)
        case .failure(let error):
            print("[ProfileListView] Import error: \(error)")
        }
    }

    /// The old plaintext JSON bundle (version 1 or 2).
    private func importPlaintextBundle(_ data: Data) {
        guard let identity = UserIdentity.importProfile(from: data) else {
            print("[ProfileListView] Failed to import profile")
            importError = "This file isn't an Aura identity backup or profile bundle."
            return
        }

        if let existing = profileManager.profiles.first(where: { $0.publicKeyHex == identity.publicKeyHex }) {
            importError = "This identity is already on this device as \"\(existing.displayName)\"."
            return
        }
        // Never let an imported bundle take over another profile's keychain
        // item by reusing its id.
        if let id = identity.id, profileManager.profiles.contains(where: { $0.id == id }) {
            identity.id = UUID()
        }

        // Saves the identity to the keychain (a version 1 bundle was upgraded
        // on import, and an upgraded identity that is not stored is lost) and
        // persists the profile.
        profileManager.createProfile(displayName: identity.displayName, identity: identity)
        print("[ProfileListView] Imported profile: \(identity.displayName)")
    }
}

/// "Export Encrypted Backup…" for one profile: loads its identity from the
/// keychain, then the passphrase form. Replaces the old plaintext export.
private struct ProfileBackupSheet: View {
    @Environment(\.dismiss) private var dismiss
    let profile: UserProfileModel

    @State private var identity: UserIdentity?
    @State private var loadFailed = false

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s16) {
            Text("Back up \"\(profile.displayName)\"")
                .font(.title2.bold())

            Text("The backup is encrypted with your passphrase. Without the passphrase it can't be opened, so keep both somewhere safe.")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                .foregroundStyle(AuraTheme.Colors.textMid)
                .fixedSize(horizontal: false, vertical: true)

            if let identity {
                IdentityBackupForm(
                    displayName: profile.displayName,
                    secret: { identity.secretForBackup() }
                )
            } else if loadFailed {
                Text("This profile's key couldn't be loaded.")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.danger)
            } else {
                ProgressView().controlSize(.small)
            }

            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(24)
        .frame(width: 420)
        .onAppear {
            guard identity == nil else { return }
            identity = UserIdentity.loadFromKeychain(id: profile.id, requiresBiometric: profile.requiresBiometric)
            loadFailed = identity == nil
        }
    }
}
