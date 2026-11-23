import Foundation
import Combine

/// Per-member trust for the roster's key glyph and the profile card.
///
/// Keys come only from the authenticated MLS ratchet tree of the channel we
/// are in, exactly like the trust sheet — never from the server's user list.
/// Members of other channels are not in our group, so for them the answer is
/// honestly "unknown" (`nil`) rather than a guess.
@MainActor
final class RosterTrustModel: ObservableObject {

    struct Entry: Equatable {
        let identityKey: Data
        let state: TrustState
    }

    /// By user UUID (the MLS credential identity).
    @Published private(set) var entries: [String: Entry] = [:]

    private let trust: TrustStore

    init(trust: TrustStore) {
        self.trust = trust
    }

    /// Re-read the voice group's members for `channelId`. Cheap enough to call
    /// on a short timer: the group is a handful of leaves, and the published
    /// value only changes when a state actually moves.
    func refresh(mls: MlsWrapper?, channelId: String?) {
        guard let mls, let channelId,
              let records = try? mls.groupMembers(channelId: channelId, isVoice: true)
        else {
            if !entries.isEmpty { entries = [:] }
            return
        }

        var next: [String: Entry] = [:]
        for record in records where !record.isSelf {
            // Observe before evaluating, as the trust sheet does: a later key
            // that disagrees with the first one seen is what "key changed" means.
            trust.observe(userUuid: record.identity, publicKey: record.identityKey)
            next[record.identity] = Entry(
                identityKey: record.identityKey,
                state: trust.evaluate(userUuid: record.identity, publicKey: record.identityKey)
            )
        }

        if next != entries { entries = next }
    }

    func entry(forUuid uuid: String?) -> Entry? {
        guard let uuid else { return nil }
        return entries[uuid]
    }
}
