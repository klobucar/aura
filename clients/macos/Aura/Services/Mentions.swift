import Foundation

/// Mention detection and `@` autocomplete, shared byte-for-byte with the
/// desktop client.
///
/// A message mentions you when it contains `@<your display name>`, compared
/// without regard to case, where the next character is the end of the text,
/// whitespace or punctuation (Unicode general category P*); or when it
/// contains one of your keywords as a whole word, again ignoring case. A
/// keyword is a whole word when the characters on either side of it (if any)
/// are not letters, digits or `_`. Your own messages never mention you.
nonisolated enum Mentions {

    /// Up to this many members are offered after `@`.
    static let suggestionLimit = 6

    static func mentionsYou(
        _ text: String,
        displayName: String,
        keywords: [String],
        isOwnMessage: Bool
    ) -> Bool {
        guard !isOwnMessage else { return false }
        if !mentionRanges(in: text, displayName: displayName).isEmpty { return true }
        return keywords.contains { containsWholeWord(text, $0) }
    }

    /// Every `@<displayName>` token in `text` that counts as a mention.
    static func mentionRanges(in text: String, displayName: String) -> [Range<String.Index>] {
        let name = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return [] }
        let needle = "@" + name

        var ranges: [Range<String.Index>] = []
        var searchStart = text.startIndex
        while searchStart < text.endIndex,
              let found = text.range(of: needle, options: .caseInsensitive, range: searchStart..<text.endIndex) {
            if found.upperBound == text.endIndex || endsMention(text[found.upperBound]) {
                ranges.append(found)
                searchStart = found.upperBound
            } else {
                searchStart = text.index(after: found.lowerBound)
            }
        }
        return ranges
    }

    /// The comma-separated keyword setting, trimmed, blanks dropped.
    static func keywords(from setting: String) -> [String] {
        setting
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    static func containsWholeWord(_ text: String, _ word: String) -> Bool {
        let word = word.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !word.isEmpty else { return false }

        var searchStart = text.startIndex
        while searchStart < text.endIndex,
              let found = text.range(of: word, options: .caseInsensitive, range: searchStart..<text.endIndex) {
            let startsWord = found.lowerBound == text.startIndex
                || !isWordCharacter(text[text.index(before: found.lowerBound)])
            let endsWord = found.upperBound == text.endIndex
                || !isWordCharacter(text[found.upperBound])
            if startsWord && endsWord { return true }
            searchStart = text.index(after: found.lowerBound)
        }
        return false
    }

    private static func endsMention(_ character: Character) -> Bool {
        character.isWhitespace || character.isPunctuation
    }

    private static func isWordCharacter(_ character: Character) -> Bool {
        character.isLetter || character.isNumber || character == "_"
    }

    // MARK: Autocomplete

    /// The partial name being typed after an `@` at the end of `text`, or nil
    /// when the text does not end in one. The `@` must start the text or
    /// follow whitespace, so an e-mail address does not open the list.
    static func autocompleteQuery(in text: String) -> String? {
        guard let at = text.lastIndex(of: "@") else { return nil }
        if at != text.startIndex, !text[text.index(before: at)].isWhitespace { return nil }
        let query = text[text.index(after: at)...]
        guard !query.contains(where: \.isNewline) else { return nil }
        return String(query)
    }

    /// Members whose name starts with `query`, ignoring case: unique, in the
    /// order given, at most `suggestionLimit`.
    static func suggestions(for query: String, among names: [String]) -> [String] {
        let prefix = query.lowercased()
        var seen: Set<String> = []
        var result: [String] = []
        for name in names where !name.isEmpty && name.lowercased().hasPrefix(prefix) {
            guard seen.insert(name).inserted else { continue }
            result.append(name)
            if result.count == suggestionLimit { break }
        }
        return result
    }

    /// `text` with its trailing `@partial` replaced by `@name `.
    static func complete(_ text: String, with name: String) -> String {
        guard let at = text.lastIndex(of: "@") else { return text }
        return String(text[..<at]) + "@" + name + " "
    }
}
