import Foundation
import Combine

// MARK: - Models

/// One contiguous run of speech by a single participant.
/// `end == nil` means the run is still live.
struct TalkSegment: Identifiable, Equatable {
    let id = UUID()
    let sessionId: UInt32
    var start: Date
    var end: Date?

    var isLive: Bool { end == nil }

    func duration(now: Date) -> TimeInterval {
        (end ?? now).timeIntervalSince(start)
    }
}

/// An interval where two people talked at once, attributed to whoever started
/// second — this is the "talked over" signal the lanes draw in `caution`.
struct TalkOverlap: Identifiable, Equatable {
    /// The later speaker: the one who talked over someone else.
    let sessionId: UInt32
    let start: Date
    let end: Date

    var id: String { "\(sessionId)-\(start.timeIntervalSince1970)-\(end.timeIntervalSince1970)" }
}

// MARK: - Waveform

/// Rolling level history for the current speaker's waveform, kept in its own
/// observable so the 10Hz sampling invalidates the speaker card and nothing
/// else. Folded into `TalkSegmentStore` it would redraw the whole window.
@MainActor
final class WaveformBuffer: ObservableObject {
    /// The voice stage draws 26 bars, the rail 16 — keep enough for the wider
    /// one and take the tail for the narrower.
    static let sampleCount = 26

    @Published private(set) var samples: [Float] = Array(repeating: 0, count: WaveformBuffer.sampleCount)

    /// Appends a normalised (0…1) level, dropping the oldest sample.
    func push(_ level: Float) {
        var next = samples
        next.removeFirst()
        next.append(min(1, max(0, level)))
        // Only publish on change: an idle channel should not redraw ten times
        // a second.
        if next != samples { samples = next }
    }

    func clear() {
        samples = Array(repeating: 0, count: Self.sampleCount)
    }
}

// MARK: - Store

/// Rolling talk history for the voice rail / stage.
///
/// In-memory only, nothing on the wire. Fed from two sources because the
/// network client cannot supply both: `QuicNetworkClient.activeSpeakers` for
/// remote senders (it comes out of the jitter buffer's mixer, so it never
/// contains us) and `QuicNetworkClient.isLocalSpeaking` for our own lane.
@MainActor
final class TalkSegmentStore: ObservableObject {

    // MARK: Tuning

    /// The lanes cover the last three minutes.
    static let window: TimeInterval = 180

    /// How long the current-speaker card keeps someone after they go quiet,
    /// so cross-talk does not flicker the hero.
    private static let currentSpeakerHold: TimeInterval = 1.2

    /// Runs shorter than this are candidates for folding into a neighbour…
    private static let minSegment: TimeInterval = 0.4

    /// …when the gap to that neighbour is no wider than this.
    private static let mergeGap: TimeInterval = 0.25

    /// Lane re-layout / trim cadence.
    private static let trimInterval: TimeInterval = 1.0

    /// Waveform sampling cadence. Faster than the trim tick, which is why the
    /// timer runs at this rate and trims every tenth pass.
    private static let levelInterval: TimeInterval = 0.1

    // MARK: Published state

    /// Every run of speech still inside the window, oldest first.
    @Published private(set) var talkSegments: [TalkSegment] = []

    /// Who has the floor. Held for 1200ms past the end of their speech.
    ///
    /// Never us — see `localSessionId`.
    @Published private(set) var currentSpeakerId: UInt32?

    /// Our own session, excluded from the current-speaker card.
    ///
    /// The hero answers "who has the floor", which is only ever a question
    /// about other people — you already know when you are talking, and the
    /// HUD's input meter says so continuously. Promoting yourself there put a
    /// mirror next to that meter and attached a talk-share figure to it, which
    /// read as a scold rather than information. Lanes still include you: there
    /// the number is about balance, which is the one place it earns its keep.
    var localSessionId: UInt32?

    /// True when we are the only one talking. Distinguishes "nobody has said
    /// anything" from "you are talking to an empty room", which want different
    /// copy in the rail's empty state.
    var isOnlyLocalSpeaking: Bool {
        guard let localSessionId else { return false }
        let open = talkSegments.filter { $0.end == nil }
        return open.contains { $0.sessionId == localSessionId }
            && !open.contains { $0.sessionId != localSessionId }
    }

    /// Fraction of the observed window each participant spent talking.
    /// Derived; recomputed on every trim. Can exceed 1.0 in aggregate because
    /// people talk over each other.
    @Published private(set) var talkShare: [UInt32: Double] = [:]

    /// Intersections between two people's speech, attributed to the later one.
    @Published private(set) var overlaps: [TalkOverlap] = []

    /// Level history behind the current speaker's waveform. Separately
    /// observable — see `WaveformBuffer`.
    let waveform = WaveformBuffer()

    // MARK: Private state

    /// Supplies the level (0…1) that feeds the waveform. Set by the view layer
    /// so the store keeps no reference to the network client.
    var levelProvider: (@MainActor () -> Float)?

    /// When the store started observing. Talk-share divides by the observed
    /// span until it reaches a full window, otherwise a five-second-old store
    /// would report everyone at ~2%.
    private var epoch = Date()

    /// Deadline for the current-speaker hold; `nil` while the holder is talking.
    private var holdUntil: Date?

    /// Wall-clock time each live speaker started, used to pick the newest riser
    /// when the floor opens up.
    private var lastRise: [UInt32: Date] = [:]

    private var timer: Timer?
    private var tickCount = 0

    // MARK: Lifecycle

    init() {
        start()
    }

    deinit {
        timer?.invalidate()
    }

    private func start() {
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: Self.levelInterval, repeats: true) { [weak self] _ in
            guard let self = self else { return }
            Task { @MainActor in self.tick() }
        }
    }

    /// Drops all history — channel switch or disconnect.
    func reset() {
        talkSegments.removeAll()
        overlaps.removeAll()
        talkShare.removeAll()
        currentSpeakerId = nil
        holdUntil = nil
        lastRise.removeAll()
        waveform.clear()
        epoch = Date()
    }

    // MARK: Input

    /// Push the full set of people talking right now — remote speakers plus,
    /// when applicable, the local session id.
    func setSpeaking(_ ids: Set<UInt32>, now: Date = Date()) {
        let live = Set(talkSegments.lazy.filter(\.isLive).map(\.sessionId))

        for id in ids.subtracting(live) {
            openSegment(for: id, at: now)
        }
        for id in live.subtracting(ids) {
            closeSegment(for: id, at: now)
        }

        resolveCurrentSpeaker(active: ids, now: now)
        recompute(now: now)
    }

    // MARK: Timer

    private func tick() {
        let now = Date()

        sampleLevel()

        tickCount += 1
        guard Double(tickCount) * Self.levelInterval >= Self.trimInterval else { return }
        tickCount = 0

        // The hold deadline has to be able to expire with no change in the
        // speaking set, so re-resolve on every trim.
        let active = Set(talkSegments.lazy.filter(\.isLive).map(\.sessionId))
        resolveCurrentSpeaker(active: active, now: now)
        trim(now: now)
        recompute(now: now)
    }

    private func sampleLevel() {
        // Nobody has the floor: let the bars fall away rather than freezing on
        // the last speaker's envelope.
        waveform.push(currentSpeakerId == nil ? 0 : (levelProvider?() ?? 0))
    }

    // MARK: Segment bookkeeping

    private func openSegment(for id: UInt32, at now: Date) {
        lastRise[id] = now

        // Reopen the previous run instead of starting a new one when it was a
        // short fringe within the merge gap — gate flutter should read as one
        // continuous run, not a row of pips.
        if let index = lastIndex(of: id),
           let end = talkSegments[index].end,
           now.timeIntervalSince(end) <= Self.mergeGap,
           talkSegments[index].duration(now: now) < Self.minSegment {
            talkSegments[index].end = nil
            return
        }

        talkSegments.append(TalkSegment(sessionId: id, start: now, end: nil))
    }

    private func closeSegment(for id: UInt32, at now: Date) {
        guard let index = talkSegments.lastIndex(where: { $0.sessionId == id && $0.isLive }) else { return }
        talkSegments[index].end = now

        // Fold a too-short run backwards into the run before it when the gap
        // between them is small enough.
        guard talkSegments[index].duration(now: now) < Self.minSegment else { return }
        let start = talkSegments[index].start
        guard let previous = talkSegments[..<index].lastIndex(where: { $0.sessionId == id }),
              let previousEnd = talkSegments[previous].end,
              start.timeIntervalSince(previousEnd) <= Self.mergeGap
        else { return }

        talkSegments[previous].end = now
        talkSegments.remove(at: index)
    }

    private func lastIndex(of id: UInt32) -> Int? {
        talkSegments.lastIndex { $0.sessionId == id }
    }

    // MARK: Current speaker

    /// The hero card switches only when the incumbent has been quiet for
    /// `currentSpeakerHold`. Someone starting *while* the incumbent still has
    /// the floor does not steal it — that is the whole point of the hold.
    private func resolveCurrentSpeaker(active: Set<UInt32>, now: Date) {
        // Our own voice never takes the floor here; someone else talking over
        // us still does.
        let active = active.subtracting(localSessionId.map { [$0] } ?? [])

        if let current = currentSpeakerId, active.contains(current) {
            holdUntil = nil
            return
        }

        if currentSpeakerId != nil {
            if holdUntil == nil {
                holdUntil = now.addingTimeInterval(Self.currentSpeakerHold)
                return
            }
            if let deadline = holdUntil, now < deadline {
                return
            }
        }

        // Floor is open: hand it to whoever started talking most recently.
        let next = active.max { (lastRise[$0] ?? .distantPast) < (lastRise[$1] ?? .distantPast) }
        if currentSpeakerId != next { currentSpeakerId = next }
        holdUntil = nil
    }

    // MARK: Derived state

    private func trim(now: Date) {
        let cutoff = now.addingTimeInterval(-Self.window)
        // Guarded because a bare `removeAll(where:)` publishes on every tick
        // even when it removes nothing, redrawing an idle channel once a second.
        if talkSegments.contains(where: { ($0.end ?? now) < cutoff }) {
            talkSegments.removeAll { segment in
                guard let end = segment.end else { return false }
                return end < cutoff
            }
        }
        let pruned = lastRise.filter { $0.value >= cutoff || currentSpeakerId == $0.key }
        if pruned != lastRise { lastRise = pruned }
    }

    private func recompute(now: Date) {
        let cutoff = now.addingTimeInterval(-Self.window)
        let observed = max(1, min(Self.window, now.timeIntervalSince(epoch)))

        var totals: [UInt32: TimeInterval] = [:]
        for segment in talkSegments {
            let start = max(segment.start, cutoff)
            let end = min(segment.end ?? now, now)
            guard end > start else { continue }
            totals[segment.sessionId, default: 0] += end.timeIntervalSince(start)
        }
        // Both assignments are guarded so an idle channel publishes nothing —
        // every observer of this store would otherwise redraw once a second.
        let shares = totals.mapValues { min(1.0, $0 / observed) }
        if shares != talkShare { talkShare = shares }

        let computed = computeOverlaps(cutoff: cutoff, now: now)
        if computed != overlaps { overlaps = computed }
    }

    /// Pairwise intersections between segments belonging to different people.
    /// The interval is credited to whoever started later: they are the one who
    /// talked over somebody.
    private func computeOverlaps(cutoff: Date, now: Date) -> [TalkOverlap] {
        let visible = talkSegments.filter { (($0.end ?? now) > cutoff) }
        guard visible.count > 1 else { return [] }

        var result: [TalkOverlap] = []
        for (index, a) in visible.enumerated() {
            let aStart = max(a.start, cutoff)
            let aEnd = min(a.end ?? now, now)
            guard aEnd > aStart else { continue }

            for b in visible[(index + 1)...] where b.sessionId != a.sessionId {
                let bStart = max(b.start, cutoff)
                let bEnd = min(b.end ?? now, now)
                guard bEnd > bStart else { continue }

                let start = max(aStart, bStart)
                let end = min(aEnd, bEnd)
                guard end > start else { continue }

                // Later starter owns the overlap; a dead-heat is credited to
                // whichever segment was appended second.
                let later = b.start >= a.start ? b : a
                result.append(TalkOverlap(sessionId: later.sessionId, start: start, end: end))
            }
        }
        return result
    }

    // MARK: Queries

    /// Segments for one participant, in window order.
    func segments(for id: UInt32) -> [TalkSegment] {
        talkSegments.filter { $0.sessionId == id }
    }

    /// Overlaps drawn on one participant's lane.
    func overlapSegments(for id: UInt32) -> [TalkOverlap] {
        overlaps.filter { $0.sessionId == id }
    }

    /// Anyone with talk time in the window, most talkative first.
    func rankedSpeakers() -> [UInt32] {
        talkShare
            .filter { $0.value > 0 }
            .sorted { lhs, rhs in
                lhs.value == rhs.value ? lhs.key < rhs.key : lhs.value > rhs.value
            }
            .map(\.key)
    }

    /// Lanes never render unbounded: the top `limit` talkers get a lane, the
    /// rest collapse behind an "N others" row.
    func lanes(limit: Int = 5) -> (top: [UInt32], overflow: [UInt32]) {
        let ranked = rankedSpeakers()
        guard ranked.count > limit else { return (ranked, []) }
        return (Array(ranked.prefix(limit)), Array(ranked.dropFirst(limit)))
    }

    /// Position of a date inside the window, 0 = window start, 1 = now.
    func fraction(of date: Date, now: Date = Date()) -> Double {
        let cutoff = now.addingTimeInterval(-Self.window)
        return min(1, max(0, date.timeIntervalSince(cutoff) / Self.window))
    }

    /// How long the current speaker has held the floor, in seconds, or `nil`
    /// when nobody is speaking.
    func currentSpeakerElapsed(now: Date = Date()) -> TimeInterval? {
        guard let id = currentSpeakerId,
              let segment = talkSegments.last(where: { $0.sessionId == id })
        else { return nil }
        return max(0, (segment.end ?? now).timeIntervalSince(segment.start))
    }
}
