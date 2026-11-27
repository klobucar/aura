import SwiftUI
import ImageIO

// MARK: - Message row

/// One chat message (handoff §2). Chat-first: 34px avatar beside the message,
/// name and mono time on one row, bubble up to 470. Voice-focus: no avatars,
/// sender above the bubble, bubble up to 230.
struct ChatMessageRow: View {
    let message: ChatMessage
    let mode: AuraLayoutMode
    var fileTransferState: FileTransferState? = nil
    var onReply: (ChatMessage) -> Void
    var onRetry: (ChatMessage) -> Void
    var onJumpToReply: (String) -> Void
    var onOpenAttachment: ((URL) -> Void)? = nil
    var onSaveAttachment: ((URL, String) -> Void)? = nil

    @State private var isHovering = false

    private var isWide: Bool { mode == .chatFirst }
    private var bubbleMaxWidth: CGFloat { isWide ? 470 : 230 }
    private var outgoing: Bool { message.isOutgoing }

    var body: some View {
        Group {
            if isWide { wideRow } else { narrowRow }
        }
        .onHover { isHovering = $0 }
        .accessibilityElement(children: .combine)
    }

    // MARK: Layouts

    private var wideRow: some View {
        HStack(alignment: .top, spacing: AuraTheme.Spacing.s11) {
            if outgoing { Spacer(minLength: 40) } else { avatar }

            VStack(alignment: outgoing ? .trailing : .leading, spacing: AuraTheme.Spacing.s5) {
                nameRow
                bubble
                    .frame(maxWidth: bubbleMaxWidth, alignment: outgoing ? .trailing : .leading)
                failureRow
            }

            if outgoing { avatar } else { Spacer(minLength: 40) }
        }
    }

    private var narrowRow: some View {
        VStack(alignment: outgoing ? .trailing : .leading, spacing: AuraTheme.Spacing.s3) {
            if !outgoing {
                HStack(alignment: .firstTextBaseline, spacing: AuraTheme.Spacing.s8) {
                    Text(message.senderName)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                    mentionLabel
                }
                .padding(.leading, AuraTheme.Spacing.s12)
            }
            bubble
                .frame(maxWidth: bubbleMaxWidth, alignment: outgoing ? .trailing : .leading)
            if outgoing {
                Text(ChatFormatting.timeLabel(message.timestamp))
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
            }
            failureRow
        }
        .frame(maxWidth: .infinity, alignment: outgoing ? .trailing : .leading)
        .help(outgoing ? "" : ChatFormatting.timeLabel(message.timestamp))
    }

    // MARK: Pieces

    private var avatar: some View {
        AuraAvatar(name: message.senderName, data: message.avatarData, size: 34)
    }

    /// Name 13/650 and mono time on one baseline; reversed for outgoing.
    private var nameRow: some View {
        HStack(alignment: .firstTextBaseline, spacing: AuraTheme.Spacing.s8) {
            if outgoing { time }
            Text(outgoing ? "you" : message.senderName)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.textMid)
                .lineLimit(1)
            if !outgoing {
                time
                mentionLabel
            }
        }
    }

    /// Mono `mentions you` on the name row of a message that does.
    @ViewBuilder
    private var mentionLabel: some View {
        if message.mentionsYou {
            Text("mentions you")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.cautionText)
                .lineLimit(1)
        }
    }

    private var time: some View {
        Text(ChatFormatting.timeLabel(message.timestamp))
            .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
            .foregroundStyle(AuraTheme.Colors.textFaint)
    }

    private var bubble: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s8) {
            if let preview = message.replyToPreview {
                replyContext(sender: message.replyToSender ?? "", preview: preview)
            }

            if let attachment = message.attachment {
                ChatAttachmentCard(
                    attachment: attachment,
                    state: fileTransferState ?? .pending,
                    outgoing: outgoing,
                    onOpen: onOpenAttachment,
                    onSave: onSaveAttachment
                )
            }

            if !message.content.isEmpty {
                AuraMarkdownText(
                    source: message.content,
                    size: AuraTheme.Typography.t13,
                    foreground: outgoing ? .white : AuraTheme.Colors.text,
                    underlineLinks: outgoing,
                    mentionName: outgoing ? nil : message.mentionName
                )
            }
        }
        .padding(.horizontal, isWide ? AuraTheme.Spacing.s16 : AuraTheme.Spacing.s14)
        .padding(.vertical, isWide ? AuraTheme.Spacing.s12 : 10)
        .auraMessageBubble(isOutgoing: outgoing, isMention: message.mentionsYou)
        .opacity(message.deliveryFailed ? 0.6 : 1)
        // Hover affordance for reply. An overlay so it never shifts layout.
        .overlay(alignment: outgoing ? .leading : .trailing) {
            if isHovering {
                Button { onReply(message) } label: {
                    Image(systemName: "arrowshape.turn.up.left.fill")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                        .frame(width: 24, height: 24)
                        .background(Circle().fill(AuraTheme.Gradients.panel))
                        .overlay(Circle().strokeBorder(AuraTheme.Colors.panelBorder, lineWidth: 0.5))
                }
                .buttonStyle(.plain)
                .offset(x: outgoing ? -30 : 30)
                .help("Reply")
                .transition(.opacity)
            }
        }
        .contextMenu {
            Button("Reply", systemImage: "arrowshape.turn.up.left") { onReply(message) }
            Button("Copy", systemImage: "doc.on.doc") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(message.content, forType: .string)
            }
            if message.deliveryFailed {
                Divider()
                Button("Retry Sending", systemImage: "arrow.clockwise") { onRetry(message) }
            }
        }
    }

    /// 3px bar, sender mono in `link`, quoted line truncated.
    private func replyContext(sender: String, preview: String) -> some View {
        Button {
            if let id = message.replyToId { onJumpToReply(id) }
        } label: {
            HStack(alignment: .top, spacing: AuraTheme.Spacing.s9) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(outgoing ? Color.white.opacity(0.7) : AuraTheme.Colors.primary.opacity(0.7))
                    .frame(width: 3)
                VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                    Text(sender)
                        .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                        .foregroundStyle(outgoing ? Color.white.opacity(0.85) : AuraTheme.Colors.link)
                    Text(preview)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                        .foregroundStyle(outgoing ? Color.white.opacity(0.75) : AuraTheme.Colors.textDim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Show the original message")
    }

    @ViewBuilder
    private var failureRow: some View {
        if message.deliveryFailed {
            HStack(spacing: AuraTheme.Spacing.s5) {
                Image(systemName: "exclamationmark.circle.fill")
                    .font(.system(size: 10))
                Text("not sent")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                Button("Retry") { onRetry(message) }
                    .buttonStyle(.plain)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .semibold))
                    .foregroundStyle(AuraTheme.Colors.link)
            }
            .foregroundStyle(AuraTheme.Colors.danger)
        }
    }
}

// MARK: - System line

/// Centred info chip — joins, leaves, channel switches.
struct ChatInfoRow: View {
    let text: String

    var body: some View {
        HStack {
            Spacer()
            Text(text)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .medium))
                .foregroundStyle(AuraTheme.Colors.textDim)
                .padding(.horizontal, AuraTheme.Spacing.s14)
                .padding(.vertical, 5)
                .background(Capsule().fill(Color.white.opacity(0.05)))
                .overlay(Capsule().strokeBorder(AuraTheme.Colors.panelBorder, lineWidth: 0.5))
            Spacer()
        }
        .padding(.vertical, AuraTheme.Spacing.s2)
    }
}

// MARK: - Attachment card

struct ChatAttachmentCard: View {
    let attachment: FileAttachmentInfo
    let state: FileTransferState
    let outgoing: Bool
    var onOpen: ((URL) -> Void)?
    var onSave: ((URL, String) -> Void)?

    @State private var thumbnail: NSImage?

    private var isImage: Bool {
        attachment.mimeType.hasPrefix("image/")
    }

    private var readyURL: URL? {
        if case .ready(let url) = state { return url }
        return nil
    }

    private var formattedSize: String {
        ByteCountFormatter.string(fromByteCount: Int64(attachment.size), countStyle: .file)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
            if let url = readyURL, isImage, let thumbnail {
                Button { onOpen?(url) } label: {
                    Image(nsImage: thumbnail)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: 260, maxHeight: 180)
                        .clipShape(RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous))
                }
                .buttonStyle(.plain)
                .help("Open \(attachment.fileName)")
            }

            HStack(spacing: AuraTheme.Spacing.s8) {
                Image(systemName: isImage ? "photo.fill" : "doc.fill")
                    .font(.system(size: 16))
                    .foregroundStyle(outgoing ? Color.white.opacity(0.9) : AuraTheme.Colors.primary)

                VStack(alignment: .leading, spacing: 1) {
                    Text(attachment.fileName)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
                        .foregroundStyle(outgoing ? .white : AuraTheme.Colors.text)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(formattedSize)
                        .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                        .foregroundStyle(outgoing ? Color.white.opacity(0.75) : AuraTheme.Colors.textDim)
                }

                Spacer(minLength: AuraTheme.Spacing.s8)

                switch state {
                case .pending:
                    ProgressView()
                        .controlSize(.small)
                case .transferring(let progress):
                    ProgressView(value: progress)
                        .progressViewStyle(.circular)
                        .controlSize(.small)
                case .ready(let url):
                    HStack(spacing: AuraTheme.Spacing.s5) {
                        Button("Open") { onOpen?(url) }
                            .buttonStyle(.plain)
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .semibold))
                            .foregroundStyle(outgoing ? .white : AuraTheme.Colors.link)
                        Button("Save") { onSave?(url, attachment.fileName) }
                            .buttonStyle(.plain)
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .semibold))
                            .foregroundStyle(outgoing ? Color.white.opacity(0.85) : AuraTheme.Colors.textDim)
                    }
                case .failed(let reason):
                    Text(reason)
                        .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                        .foregroundStyle(AuraTheme.Colors.danger)
                        .lineLimit(1)
                }
            }
            .padding(AuraTheme.Spacing.s8)
            .background(
                RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                    .fill(outgoing ? Color.white.opacity(0.12) : Color.white.opacity(0.05))
            )
        }
        .task(id: readyURL) {
            guard isImage, let url = readyURL else {
                thumbnail = nil
                return
            }
            let decoded: NSImage? = await Task.detached(priority: .userInitiated) {
                guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
                let options: [CFString: Any] = [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceThumbnailMaxPixelSize: 520
                ]
                guard let cgImage = CGImageSourceCreateThumbnailAtIndex(src, 0, options as CFDictionary) else {
                    return nil
                }
                return NSImage(cgImage: cgImage, size: NSSize(width: cgImage.width, height: cgImage.height))
            }.value
            if !Task.isCancelled {
                thumbnail = decoded
            }
        }
    }
}

// MARK: - Composer

/// Composer (handoff §2), with encrypted file/image attachment picker.
struct ChatComposer: View {
    @Binding var text: String
    var focus: FocusState<Bool>.Binding
    let placeholder: String
    /// `⌘↵` keycap hint — chat-first only.
    let showsShortcutHint: Bool
    var pendingAttachment: URL? = nil
    var onAttachFile: (() -> Void)? = nil
    var onClearAttachment: (() -> Void)? = nil
    let onSend: () -> Void
    /// Set while replying; Escape cancels the reply.
    let onCancelReply: (() -> Void)?
    /// Display names of the others in this channel, offered after `@`.
    var mentionCandidates: [String] = []

    /// Highlighted row of the `@` list.
    @State private var selectedSuggestion = 0
    /// The `@` query the user dismissed with Escape; the list stays shut
    /// until they type something else.
    @State private var dismissedQuery: String?

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || pendingAttachment != nil
    }

    private var mentionQuery: String? {
        Mentions.autocompleteQuery(in: text)
    }

    private var suggestions: [String] {
        guard let query = mentionQuery, query != dismissedQuery else { return [] }
        return Mentions.suggestions(for: query, among: mentionCandidates)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s8) {
            let suggestions = suggestions
            if !suggestions.isEmpty {
                MentionSuggestionList(
                    names: suggestions,
                    selected: min(selectedSuggestion, suggestions.count - 1),
                    onPick: complete
                )
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
            if let pendingAttachment {
                HStack(spacing: AuraTheme.Spacing.s5) {
                    Image(systemName: "paperclip")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.primary)
                    Text(pendingAttachment.lastPathComponent)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11, weight: .medium))
                        .foregroundStyle(AuraTheme.Colors.text)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let onClearAttachment {
                        Button(action: onClearAttachment) {
                            Image(systemName: "xmark.circle.fill")
                                .font(.system(size: 12))
                                .foregroundStyle(AuraTheme.Colors.textDim)
                        }
                        .buttonStyle(.plain)
                        .help("Remove attachment")
                    }
                }
                .padding(.horizontal, AuraTheme.Spacing.s9)
                .padding(.vertical, 4)
                .background(Capsule().fill(AuraTheme.Colors.primary.opacity(0.14)))
            }
            field
        }
        .animation(AuraTheme.Motion.crossFade, value: suggestions)
        .onChange(of: text) { _, _ in selectedSuggestion = 0 }
    }

    private func complete(_ name: String) {
        text = Mentions.complete(text, with: name)
        dismissedQuery = nil
    }

    /// Completes the highlighted name if the `@` list is open.
    private func completeSelection() -> Bool {
        let suggestions = suggestions
        guard !suggestions.isEmpty else { return false }
        complete(suggestions[min(selectedSuggestion, suggestions.count - 1)])
        return true
    }

    private func moveSelection(by step: Int) -> KeyPress.Result {
        let count = suggestions.count
        guard count > 0 else { return .ignored }
        selectedSuggestion = (min(selectedSuggestion, count - 1) + step + count) % count
        return .handled
    }

    private var field: some View {
        HStack(spacing: 10) {
            if let onAttachFile {
                Button(action: onAttachFile) {
                    Image(systemName: "paperclip")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(pendingAttachment != nil ? AuraTheme.Colors.primary : AuraTheme.Colors.textDim)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Attach encrypted file or image")
                .accessibilityLabel("Attach file")
                .auraFluidHover()
            }

            TextField(placeholder, text: $text)
                .textFieldStyle(.plain)
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13))
                .focused(focus)
                .onSubmit {
                    // Return picks a name while the `@` list is open.
                    if !completeSelection() { onSend() }
                }
                .onKeyPress(.tab) { completeSelection() ? .handled : .ignored }
                .onKeyPress(.upArrow) { moveSelection(by: -1) }
                .onKeyPress(.downArrow) { moveSelection(by: 1) }
                .onKeyPress(.escape) {
                    if !suggestions.isEmpty {
                        dismissedQuery = mentionQuery
                        return .handled
                    }
                    guard let onCancelReply else { return .ignored }
                    onCancelReply()
                    return .handled
                }
                .accessibilityLabel(placeholder)

            if showsShortcutHint {
                Text("⌘↵")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textFaint)
                    .accessibilityHidden(true)
            }

            Button(action: onSend) {
                Image(systemName: "paperplane.fill")
                    .font(.system(size: 14, weight: .bold))
                    .foregroundStyle(.white)
                    .frame(width: 36, height: 36)
                    .background {
                        if canSend {
                            Circle().fill(AuraTheme.Gradients.primary)
                        } else {
                            Circle().fill(Color.secondary.opacity(0.3))
                        }
                    }
                    .shadow(color: AuraTheme.Colors.primary.opacity(canSend ? 0.35 : 0), radius: 8, y: 3)
            }
            .buttonStyle(.plain)
            .disabled(!canSend)
            .keyboardShortcut(.return, modifiers: .command)
            .help("Send (⌘↵)")
            .accessibilityLabel("Send")
            .auraFluidHover()
        }
        .padding(.leading, AuraTheme.Spacing.s16)
        .padding(.trailing, AuraTheme.Spacing.s8)
        .padding(.vertical, AuraTheme.Spacing.s8)
        .auraGlass(cornerRadius: AuraTheme.Radii.r24, material: .hudWindow)
    }
}

// MARK: - Reply bar

/// "Replying to …" strip above the composer.
struct ChatReplyBar: View {
    let sender: String
    let preview: String
    let onCancel: () -> Void

    var body: some View {
        HStack(spacing: AuraTheme.Spacing.s9) {
            RoundedRectangle(cornerRadius: 1.5)
                .fill(AuraTheme.Colors.primary.opacity(0.7))
                .frame(width: 3, height: 28)

            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                Text("replying to \(sender)")
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.link)
                Text(preview)
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t11))
                    .foregroundStyle(AuraTheme.Colors.textDim)
                    .lineLimit(1)
            }

            Spacer()

            Button(action: onCancel) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(AuraTheme.Colors.textDim)
            }
            .buttonStyle(.plain)
            .help("Cancel reply (Esc)")
            .accessibilityLabel("Cancel reply")
        }
        .padding(.horizontal, AuraTheme.Spacing.s20)
        .padding(.top, AuraTheme.Spacing.s8)
    }
}

// MARK: - Channel comment card

/// Channel comment / MOTD card (handoff §6), pinned above the chat. Collapses
/// to one line so a long comment does not eat the column.
struct ChannelCommentCard: View {
    let channel: ChannelModel
    @Binding var isCollapsed: Bool

    private var firstLine: String {
        channel.comment
            .split(whereSeparator: \.isNewline)
            .first
            .map(String.init) ?? channel.comment
    }

    var body: some View {
        HStack(alignment: .top, spacing: AuraTheme.Spacing.s12) {
            RoundedRectangle(cornerRadius: 1.5)
                .fill(LinearGradient(
                    colors: [AuraTheme.Colors.accent, AuraTheme.Colors.primary],
                    startPoint: .top,
                    endPoint: .bottom
                ))
                .frame(width: 3)

            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
                HStack(spacing: 6) {
                    if let emoji = channel.iconEmoji, !emoji.isEmpty {
                        Text(emoji).font(.system(size: 13))
                    }
                    Text(channel.name)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t13, weight: .semibold))
                        .foregroundStyle(AuraTheme.Colors.text)
                    Spacer()
                    Button {
                        withAnimation(AuraTheme.Motion.crossFade) { isCollapsed.toggle() }
                    } label: {
                        Image(systemName: isCollapsed ? "chevron.down" : "chevron.up")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(AuraTheme.Colors.textDim)
                            .frame(width: 20, height: 20)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help(isCollapsed ? "Show channel comment" : "Collapse channel comment")
                    .accessibilityLabel(isCollapsed ? "Show channel comment" : "Collapse channel comment")
                }

                if isCollapsed {
                    Text(firstLine)
                        .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12))
                        .foregroundStyle(AuraTheme.Colors.textDim)
                        .lineLimit(1)
                } else {
                    AuraMarkdownText(
                        source: channel.comment,
                        size: AuraTheme.Typography.t12,
                        foreground: AuraTheme.Colors.textMid,
                        lineSpacing: 3
                    )
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(.horizontal, AuraTheme.Spacing.s14)
        .padding(.vertical, AuraTheme.Spacing.s12)
        .auraPanel(cornerRadius: AuraTheme.Radii.r18)
    }
}

// MARK: - Connection banner

/// Says plainly when the connection is gone, instead of leaving a quiet
/// window that silently drops what you type.
struct ConnectionBanner: View {
    let isRetrying: Bool
    let status: String
    var isBanned = false
    let onRetry: () -> Void

    @State private var pulse = false

    private var tint: Color { isRetrying ? AuraTheme.Colors.caution : AuraTheme.Colors.danger }

    var body: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(tint)
                .frame(width: 7, height: 7)
                .opacity(isRetrying && pulse ? 0.35 : 1)
                .onAppear {
                    withAnimation(AuraTheme.Motion.speakingPulse) { pulse = true }
                }

            VStack(alignment: .leading, spacing: AuraTheme.Spacing.s2) {
                Text(isBanned ? "Banned" : isRetrying ? "Reconnecting…" : "Disconnected")
                    .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
                    .foregroundStyle(isRetrying ? AuraTheme.Colors.cautionText : AuraTheme.Colors.danger)
                Text(status)
                    .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                    .foregroundStyle(AuraTheme.Colors.textDim)
                    .lineLimit(1)
            }

            Spacer()

            if !isBanned {
                AuraChipButton(title: isRetrying ? "Retry now" : "Retry", systemImage: "arrow.clockwise", action: onRetry)
            }
        }
        .padding(.horizontal, AuraTheme.Spacing.s14)
        .padding(.vertical, 10)
        .background {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r14, style: .continuous)
                .fill(tint.opacity(isRetrying ? 0.12 : 0.16))
        }
        .overlay {
            RoundedRectangle(cornerRadius: AuraTheme.Radii.r14, style: .continuous)
                .strokeBorder(tint.opacity(isRetrying ? 0.30 : 0.38), lineWidth: 0.5)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Mentions

/// The `@` list above the composer: up to six members of this channel.
struct MentionSuggestionList: View {
    let names: [String]
    let selected: Int
    let onPick: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(Array(names.enumerated()), id: \.element) { index, name in
                Button { onPick(name) } label: {
                    HStack(spacing: AuraTheme.Spacing.s8) {
                        AuraAvatar(name: name, data: nil, size: 18)
                        Text(name)
                            .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: index == selected ? .semibold : .regular))
                            .foregroundStyle(index == selected ? AuraTheme.Colors.text : AuraTheme.Colors.textMid)
                            .lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background {
                        if index == selected {
                            RoundedRectangle(cornerRadius: AuraTheme.Radii.r10, style: .continuous)
                                .fill(AuraTheme.Colors.primary.opacity(0.20))
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Mention \(name)")
            }

            Text("tab or ↵ to complete · esc to close")
                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10))
                .foregroundStyle(AuraTheme.Colors.textFaint)
                .padding(.horizontal, 10)
                .padding(.top, AuraTheme.Spacing.s3)
                .accessibilityHidden(true)
        }
        .padding(6)
        .frame(maxWidth: 260, alignment: .leading)
        .auraPanel(cornerRadius: AuraTheme.Radii.r14)
    }
}

/// Centred pill over a chat scrolled up, when messages that mention you have
/// arrived below. Clicking scrolls to the first one not yet seen.
struct MentionsBelowPill: View {
    let count: Int
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(ChatFormatting.mentionsBelowLabel(count: count))
                .font(AuraTheme.Typography.ui(AuraTheme.Typography.t12, weight: .semibold))
                .foregroundStyle(AuraTheme.Colors.cautionText)
                .padding(.horizontal, AuraTheme.Spacing.s14)
                .padding(.vertical, 6)
                // Tint over an opaque base, so the pill reads over the messages.
                .background(Capsule().fill(AuraTheme.Colors.caution.opacity(0.16)))
                .background(Capsule().fill(AuraTheme.Colors.background.opacity(0.92)))
                .overlay(Capsule().strokeBorder(AuraTheme.Colors.caution.opacity(0.38), lineWidth: 1))
                .shadow(color: .black.opacity(0.25), radius: 8, y: 3)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .auraFluidHover()
        .help("Jump to the first mention you haven't seen")
        .accessibilityLabel(ChatFormatting.mentionsBelowLabel(count: count))
    }
}
