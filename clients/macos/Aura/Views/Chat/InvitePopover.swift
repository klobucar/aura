import SwiftUI

/// The channel header's Invite popover: an `aura://` link to hand to a
/// friend, built from this connection.
struct InvitePopover: View {
    let host: String
    let port: UInt16
    let channelId: String?
    let channelName: String?
    /// The pinned certificate fingerprint this client trusts the host by, if
    /// any. A publicly valid certificate has none.
    let pinnedFingerprint: String?
    /// The password this connection used, if any.
    let password: String?

    @State private var includeChannel = true
    @State private var includePassword = false
    @State private var copied = false

    private var link: AuraInviteLink {
        AuraInviteLink(
            host: host,
            port: port,
            channelId: includeChannel ? channelId : nil,
            fingerprint: pinnedFingerprint,
            password: includePassword ? password : nil
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s14) {
            Text("INVITE LINK")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .kerning(AuraTheme.Typography.monoLabelKerning)
                .foregroundStyle(AuraTheme.Colors.textDim)

            HStack(alignment: .center, spacing: 10) {
                Text(link.string)
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t11))
                    .foregroundStyle(AuraTheme.Colors.text)
                    .lineLimit(4)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .auraPanel(cornerRadius: AuraTheme.Radii.r10)
                    .accessibilityLabel("Invite link")
                    .accessibilityValue(link.string)

                AuraChipButton(title: copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc") {
                    copy()
                }
            }

            VStack(alignment: .leading, spacing: 10) {
                if let channelId, let channelName, !channelId.isEmpty {
                    Toggle(isOn: $includeChannel) {
                        Text(ChatFormatting.inviteChannelToggle(channelName: channelName))
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                            .foregroundStyle(AuraTheme.Colors.text)
                    }
                }

                Toggle(isOn: $includePassword) {
                    VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                        Text("Include the server password")
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                            .foregroundStyle(AuraTheme.Colors.text)
                        Text("Anyone holding the link can then get in")
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                            .foregroundStyle(AuraTheme.Colors.cautionText)
                    }
                }
                .disabled(password == nil)
                .help(password == nil ? "This connection didn't use a password" : "")
            }
            .toggleStyle(.switch)
            .controlSize(.small)

            if pinnedFingerprint != nil {
                HStack(alignment: .top, spacing: AuraTheme.Spacing.s8) {
                    Image(systemName: "lock.shield")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.accent)
                    Text("The link carries the server's certificate fingerprint, so a friend's first connection is checked against it instead of trusted on sight.")
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(AuraTheme.Spacing.s16)
        .frame(width: 360)
        .onChange(of: link) { _, _ in copied = false }
    }

    private func copy() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(link.string, forType: .string)
        copied = true
    }
}
