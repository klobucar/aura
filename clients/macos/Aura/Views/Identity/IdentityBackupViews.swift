import AuraCore
import SwiftUI
import AppKit

// MARK: - Identity card

/// Settings → Identity: who you are on every server, and whether that key
/// survives losing this Mac.
struct IdentitySummaryCard: View {
    let name: String
    let identityKey: Data?
    /// When the login identity was last backed up; nil if never.
    let lastBackup: Date?

    private var fingerprint: String {
        guard let identityKey, let grouped = try? identityFingerprint(publicKey: identityKey) else {
            return "no identity"
        }
        return grouped
    }

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s12) {
            HStack(alignment: .top, spacing: AuraTheme.Spacing.s12) {
                AuraAvatar(name: name, data: nil, size: 40)

                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
                    Text(name)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.text)
                        .lineLimit(1)
                    Text(fingerprint)
                        .font(AuraTheme.Typography.mono(10.5))
                        .foregroundStyle(AuraTheme.Colors.accent)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                        .accessibilityLabel("Identity key fingerprint")
                    Text("Hybrid Ed25519 + ML-DSA-65 key, kept on this device")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                }
            }

            if let lastBackup {
                Label(ChatFormatting.lastBackupLabel(lastBackup), systemImage: "checkmark.seal")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textMid)
            } else {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 12))
                        .foregroundStyle(AuraTheme.Colors.caution)
                    Text("Never backed up. If this device is lost, your name on each server stays taken by a key you no longer have, and everyone who verified you will see \"key changed\".")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                        .foregroundStyle(AuraTheme.Colors.cautionText)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.horizontal, AuraTheme.Spacing.s12)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background {
                    RoundedRectangle(cornerRadius: AuraTheme.Radii.r14, style: .continuous)
                        .fill(AuraTheme.Colors.cautionFill)
                }
                .overlay {
                    RoundedRectangle(cornerRadius: AuraTheme.Radii.r14, style: .continuous)
                        .strokeBorder(AuraTheme.Colors.cautionBorder, lineWidth: 0.5)
                }
            }
        }
    }
}

// MARK: - Backup form

/// Passphrase, repeat, and "Save encrypted backup…". Encryption runs off the
/// main thread; the file is written only after it succeeds.
struct IdentityBackupForm: View {
    let displayName: String
    /// Supplies the identity secret at save time; nil if it can't be read.
    let secret: () -> Data?
    var onSaved: (Date) -> Void = { _ in }

    @State private var passphrase = ""
    @State private var repeated = ""
    @State private var isEncrypting = false
    @State private var errorMessage: String?
    @State private var savedMessage: String?

    private var problem: String? {
        IdentityBackup.passphraseProblem(passphrase, repeated: repeated)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            SecureField("Backup passphrase", text: $passphrase)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Backup passphrase")
            SecureField("Repeat passphrase", text: $repeated)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Repeat passphrase")

            // Only nag once the user has started typing.
            if let problem, !passphrase.isEmpty {
                Text(problem)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                    .foregroundStyle(AuraTheme.Colors.textDim)
            }

            HStack(spacing: 10) {
                Button("Save encrypted backup…", action: save)
                    .buttonStyle(.borderedProminent)
                    .disabled(problem != nil || isEncrypting)

                if isEncrypting {
                    ProgressView()
                        .controlSize(.small)
                    Text("Encrypting…")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                }
            }

            if let errorMessage {
                Text(errorMessage)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.danger)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let savedMessage {
                Text(savedMessage)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textMid)
            }
        }
        .onChange(of: passphrase) { _, _ in errorMessage = nil }
    }

    private func save() {
        errorMessage = nil
        savedMessage = nil
        guard let secret = secret() else {
            errorMessage = "This identity's key couldn't be read."
            return
        }

        let panel = NSSavePanel()
        panel.title = "Save encrypted backup"
        panel.nameFieldStringValue = IdentityBackup.suggestedFileName(displayName: displayName)
        panel.allowedContentTypes = [IdentityBackup.contentType]
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }

        let passphrase = passphrase
        isEncrypting = true
        Task {
            defer { isEncrypting = false }
            do {
                let backup = try await IdentityBackup.encrypt(
                    secret: secret, displayName: displayName, passphrase: passphrase)
                try backup.write(to: url)
                self.passphrase = ""
                repeated = ""
                savedMessage = "Saved \(url.lastPathComponent)"
                onSaved(Date())
            } catch {
                errorMessage = IdentityBackup.message(for: error)
            }
        }
    }
}

// MARK: - Restore

/// A backup file picked for restoring, ready to be unlocked.
struct PendingBackup: Identifiable {
    let id = UUID()
    let fileName: String
    let data: Data
}

/// Unlock a backup, show whose identity it holds, and add it as a new
/// profile. Never replaces an existing profile; a key already on this Mac is
/// reported rather than added twice.
struct RestoreBackupSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var profileManager: ProfileManager
    let backup: PendingBackup
    /// Ed25519 key hex of the login identity, which is on this Mac without
    /// being a profile.
    var loginIdentityKeyHex: String?
    var loginIdentityName: String?

    private enum Stage {
        case locked
        case unlocked(UserIdentity)
        case restored(String)
    }

    @State private var stage = Stage.locked
    @State private var passphrase = ""
    @State private var isDecrypting = false
    @State private var errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s16) {
            Text("Restore from backup")
                .font(.title2.bold())

            switch stage {
            case .locked:
                lockedView
            case .unlocked(let identity):
                unlockedView(identity)
            case .restored(let name):
                restoredView(name)
            }
        }
        .padding(24)
        .frame(width: 420)
    }

    private var lockedView: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s12) {
            Text(backup.fileName)
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t11))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .lineLimit(1)
                .truncationMode(.middle)

            SecureField("Backup passphrase", text: $passphrase)
                .textFieldStyle(.roundedBorder)
                .onSubmit(unlock)
                .accessibilityLabel("Backup passphrase")

            if let errorMessage {
                Text(errorMessage)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.danger)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                if isDecrypting {
                    ProgressView().controlSize(.small)
                    Text("Decrypting…")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Unlock", action: unlock)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(passphrase.isEmpty || isDecrypting)
            }
        }
    }

    @ViewBuilder
    private func unlockedView(_ identity: UserIdentity) -> some View {
        let duplicate = existingName(for: identity)

        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s12) {
            HStack(spacing: AuraTheme.Spacing.s12) {
                AuraAvatar(name: identity.displayName, data: nil, size: 36)
                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                    Text(identity.displayName)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t15, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.text)
                    Text(shortFingerprint(identity))
                        .font(AuraTheme.Typography.mono(AuraTheme.Typography.t11))
                        .foregroundStyle(AuraTheme.Colors.accent)
                }
            }
            .padding(AuraTheme.Spacing.s12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .auraPanel(cornerRadius: AuraTheme.Radii.r14)

            if let duplicate {
                Text("This identity is already on this device as \"\(duplicate)\". Nothing was added.")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.cautionText)
                    .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Spacer()
                    Button("Done") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                }
            } else {
                Text("This adds the identity as a new profile. Your existing profiles stay as they are.")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                    .foregroundStyle(AuraTheme.Colors.textMid)
                    .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Spacer()
                    Button("Cancel") { dismiss() }
                        .keyboardShortcut(.cancelAction)
                    Button("Add Profile") { add(identity) }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                }
            }
        }
    }

    private func restoredView(_ name: String) -> some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s12) {
            Label("Added \"\(name)\" to your profiles.", systemImage: "checkmark.circle.fill")
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13))
                .foregroundStyle(AuraTheme.Colors.text)
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    // MARK: Actions

    private func unlock() {
        guard !passphrase.isEmpty, !isDecrypting else { return }
        errorMessage = nil
        isDecrypting = true
        let data = backup.data
        let passphrase = passphrase
        Task {
            defer { isDecrypting = false }
            do {
                let contents = try await IdentityBackup.decrypt(data, passphrase: passphrase)
                guard let identity = UserIdentity.restored(secret: contents.secret, displayName: contents.displayName) else {
                    errorMessage = "This backup doesn't hold a usable identity."
                    return
                }
                self.passphrase = ""
                stage = .unlocked(identity)
            } catch {
                errorMessage = IdentityBackup.message(for: error)
            }
        }
    }

    private func add(_ identity: UserIdentity) {
        guard existingName(for: identity) == nil else { return }
        profileManager.createProfile(displayName: identity.displayName, identity: identity)
        stage = .restored(identity.displayName)
    }

    /// The name this key already goes by on this Mac, if it is here.
    private func existingName(for identity: UserIdentity) -> String? {
        let key = identity.publicKeyHex
        if let profile = profileManager.profiles.first(where: { $0.publicKeyHex == key }) {
            return profile.displayName
        }
        if let loginIdentityKeyHex, loginIdentityKeyHex == key {
            return loginIdentityName ?? identity.displayName
        }
        return nil
    }

    private func shortFingerprint(_ identity: UserIdentity) -> String {
        guard let key = identity.identityKey, let short = try? identityFingerprintShort(publicKey: key) else {
            return String(identity.publicKeyHex.prefix(16))
        }
        return short
    }
}

/// NSOpenPanel for a `.aura-identity` file. Nil if cancelled; throws if the
/// file can't be read.
@MainActor
func pickIdentityBackup() throws -> PendingBackup? {
    let panel = NSOpenPanel()
    panel.title = "Restore from backup"
    panel.allowedContentTypes = [IdentityBackup.contentType]
    panel.allowsMultipleSelection = false
    panel.canChooseDirectories = false
    guard panel.runModal() == .OK, let url = panel.url else { return nil }
    return PendingBackup(fileName: url.lastPathComponent, data: try Data(contentsOf: url))
}

// MARK: - Leave server

extension View {
    /// The destructive "Leave server…" confirmation, shared by the roster's
    /// identity menu and Settings → Identity.
    func leaveServerAlert(
        isPresented: Binding<Bool>,
        host: String,
        name: String,
        onConfirm: @escaping () -> Void
    ) -> some View {
        alert(ChatFormatting.deleteAccountTitle(host: host), isPresented: isPresented) {
            Button("Cancel", role: .cancel) {}
            Button("Delete my account", role: .destructive, action: onConfirm)
        } message: {
            Text(ChatFormatting.deleteAccountMessage(name: name))
        }
    }
}
