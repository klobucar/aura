import Foundation
import SwiftUI

/// The one markdown renderer for chat, user comments and channel comments
/// (handoff §6, "Comment content rules"): bold, italic, inline code, inline
/// link, unordered list, blockquote. No headings, no tables, no raw HTML —
/// and never a remote image fetch, which would turn a profile into a beacon.
nonisolated enum AuraMarkdown {

    enum Block: Equatable {
        case paragraph(String)
        case bullet(String)
        case quote(String)
        case codeBlock(code: String, language: String?)
    }

    /// Link schemes a click may open. Anything else (`file:`, custom app
    /// schemes) renders as plain text — a chat line should not be a launcher.
    static let allowedLinkSchemes: Set<String> = ["http", "https", "mailto", "aura"]

    /// Split source into the block subset. Lines are classified on their own;
    /// consecutive quote lines merge into one quote. Fenced code blocks (` ``` `)
    /// preserve indentation and blank lines verbatim. Anything else, including
    /// `#` headings, stays a literal paragraph line.
    static func blocks(from source: String) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var inCodeFence = false
        var codeLanguage: String?
        var codeLines: [String] = []

        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            blocks.append(.paragraph(paragraph.joined(separator: "\n")))
            paragraph = []
        }

        func flushCodeBlock() {
            blocks.append(.codeBlock(code: codeLines.joined(separator: "\n"), language: codeLanguage))
            inCodeFence = false
            codeLanguage = nil
            codeLines = []
        }

        for rawLine in source.components(separatedBy: .newlines) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)

            if inCodeFence {
                if line.hasPrefix("```") && line.dropFirst(3).trimmingCharacters(in: .whitespaces).isEmpty {
                    flushCodeBlock()
                } else {
                    codeLines.append(rawLine)
                }
                continue
            }

            if line.hasPrefix("```") {
                flushParagraph()
                let lang = String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                codeLanguage = lang.isEmpty ? nil : lang
                inCodeFence = true
                codeLines = []
            } else if let item = bulletBody(line) {
                flushParagraph()
                blocks.append(.bullet(item))
            } else if line.hasPrefix(">") {
                flushParagraph()
                let body = String(line.dropFirst().drop(while: { $0 == " " }))
                if case .quote(let previous)? = blocks.last {
                    blocks[blocks.count - 1] = .quote(previous + "\n" + body)
                } else {
                    blocks.append(.quote(body))
                }
            } else if line.isEmpty {
                flushParagraph()
            } else {
                paragraph.append(rawLine)
            }
        }
        if inCodeFence {
            flushCodeBlock()
        }
        flushParagraph()
        return blocks
    }

    /// `- item`, `* item` or `+ item`. `*emphasis*` is not a bullet: the
    /// marker must be followed by a space.
    private static func bulletBody(_ line: String) -> String? {
        for marker in ["- ", "* ", "+ "] where line.hasPrefix(marker) {
            let body = line.dropFirst(marker.count).trimmingCharacters(in: .whitespaces)
            return body.isEmpty ? nil : body
        }
        return nil
    }

    /// Inline spans (bold, italic, code, link) with disallowed links and any
    /// image references stripped. Falls back to the literal text.
    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: false,
            interpretedSyntax: .inlineOnlyPreservingWhitespace,
            failurePolicy: .returnPartiallyParsedIfPossible
        )
        guard var result = try? AttributedString(markdown: text, options: options) else {
            return AttributedString(text)
        }
        for run in result.runs {
            if let url = run.link, !isAllowed(url) {
                result[run.range].link = nil
            }
            if run.imageURL != nil {
                result[run.range].imageURL = nil
            }
        }
        return result
    }

    static func isAllowed(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased() else { return false }
        return allowedLinkSchemes.contains(scheme)
    }
}

/// Renders `AuraMarkdown` blocks. Selection is enabled so messages, bios and
/// comments can be copied in part.
struct AuraMarkdownText: View {
    let source: String
    var size: CGFloat = AuraTheme.Typography.t13
    var foreground: Color = AuraTheme.Colors.text
    /// Link colour. On the outgoing gradient bubble links are drawn in the text
    /// colour and underlined instead, since `link` blue disappears on it.
    var link: Color = AuraTheme.Colors.link
    var underlineLinks = false
    var lineSpacing: CGFloat = 2
    /// Your display name: `@<name>` tokens that mention you get the caution
    /// highlight. Nil on your own messages, which never mention you.
    var mentionName: String? = nil

    var body: some View {
        let blocks = AuraMarkdown.blocks(from: source)
        VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .paragraph(let text):
                    Text(styled(text))
                case .bullet(let text):
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        Circle()
                            .fill(AuraTheme.Colors.accent)
                            .frame(width: 4, height: 4)
                            .alignmentGuide(.firstTextBaseline) { $0[.bottom] + size * 0.3 }
                        Text(styled(text))
                    }
                case .quote(let text):
                    HStack(spacing: AuraTheme.Spacing.s9) {
                        RoundedRectangle(cornerRadius: 1.5)
                            .fill(AuraTheme.Colors.accent.opacity(0.8))
                            .frame(width: 3)
                        Text(styled(text))
                            .opacity(0.85)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                case .codeBlock(let code, let language):
                    VStack(alignment: .leading, spacing: AuraTheme.Spacing.s5) {
                        if let language, !language.isEmpty {
                            Text(language)
                                .font(AuraTheme.Typography.mono(AuraTheme.Typography.t10, weight: .semibold))
                                .foregroundStyle(foreground.opacity(0.6))
                        }
                        ScrollView(.horizontal, showsIndicators: false) {
                            Text(code)
                                .font(AuraTheme.Typography.mono(max(11, size - 1)))
                                .foregroundStyle(foreground)
                                .fixedSize(horizontal: true, vertical: true)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, AuraTheme.Spacing.s9)
                    .padding(.vertical, AuraTheme.Spacing.s8)
                    .background(
                        RoundedRectangle(cornerRadius: AuraTheme.Radii.r8, style: .continuous)
                            .fill(Color.black.opacity(0.22))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: AuraTheme.Radii.r8, style: .continuous)
                            .strokeBorder(Color.white.opacity(0.10), lineWidth: 0.5)
                    )
                }
            }
        }
        .font(AuraTheme.Typography.ui(size))
        .lineSpacing(lineSpacing)
        .fixedSize(horizontal: false, vertical: true)
        .textSelection(.enabled)
    }

    private func styled(_ text: String) -> AttributedString {
        var result = AuraMarkdown.inline(text)
        for run in result.runs {
            if run.link != nil {
                result[run.range].foregroundColor = underlineLinks ? foreground : link
                if underlineLinks {
                    result[run.range].underlineStyle = .single
                }
            } else {
                result[run.range].foregroundColor = foreground
            }
        }
        if let mentionName {
            highlightMentions(of: mentionName, in: &result)
        }
        return result
    }

    /// Caution tint behind each `@you` token. A `Text` run can carry a
    /// background colour but not a rounded one, so this is the inline
    /// stand-in for the handoff's pill.
    private func highlightMentions(of name: String, in result: inout AttributedString) {
        let plain = String(result.characters)
        for range in Mentions.mentionRanges(in: plain, displayName: name) {
            let offset = plain.distance(from: plain.startIndex, to: range.lowerBound)
            let length = plain.distance(from: range.lowerBound, to: range.upperBound)
            let start = result.characters.index(result.startIndex, offsetBy: offset)
            let end = result.characters.index(start, offsetBy: length)
            result[start..<end].backgroundColor = AuraTheme.Colors.caution.opacity(0.24)
            result[start..<end].foregroundColor = AuraTheme.Colors.cautionText
            result[start..<end].font = AuraTheme.Typography.ui(size, weight: .semibold)
        }
    }
}
