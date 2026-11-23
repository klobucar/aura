import XCTest
import AuraCore
import os
@testable import Aura

// MARK: - Talk / level rules

/// The timing rules the voice rail depends on, driven with explicit instants.
final class VoiceActivityTrackerTests: XCTestCase {
    private let t0 = ContinuousClock.now

    private func at(_ ms: Int) -> ContinuousClock.Instant {
        t0.advanced(by: .milliseconds(ms))
    }

    func testRemoteSpeakerStaysActiveFor500msAfterTheirLastFrame() {
        var tracker = VoiceActivityTracker()
        tracker.mixed(speakers: [7], levelDb: -20, at: at(0))

        XCTAssertEqual(tracker.snapshot(at: at(499)).activeSpeakers, [7])
        XCTAssertEqual(tracker.snapshot(at: at(499)).outputLevelDb, -20)

        let after = tracker.snapshot(at: at(500))
        XCTAssertEqual(after.activeSpeakers, [])
        XCTAssertEqual(after.outputLevelDb, VoiceMeters.floorDb, "the output meter falls once nobody talks")
        XCTAssertFalse(tracker.hasPendingTimeouts)
    }

    func testSpeakersTimeOutIndependently() {
        var tracker = VoiceActivityTracker()
        tracker.mixed(speakers: [1, 2], levelDb: -12, at: at(0))
        tracker.mixed(speakers: [2], levelDb: -14, at: at(300))

        XCTAssertEqual(tracker.snapshot(at: at(450)).activeSpeakers, [1, 2])
        XCTAssertEqual(tracker.snapshot(at: at(600)).activeSpeakers, [2])
        XCTAssertEqual(tracker.snapshot(at: at(800)).activeSpeakers, [])
    }

    func testAMixWithoutASpeakerDoesNotDropThemBeforeTheirTimeout() {
        // Cross-talk: the mixer reports whoever had a frame ready.
        var tracker = VoiceActivityTracker()
        tracker.mixed(speakers: [1], levelDb: -12, at: at(0))
        tracker.mixed(speakers: [2], levelDb: -12, at: at(10))
        XCTAssertEqual(tracker.snapshot(at: at(20)).activeSpeakers, [1, 2])
    }

    func testForgetDropsAMutedSpeakerAtOnce() {
        var tracker = VoiceActivityTracker()
        tracker.mixed(speakers: [1, 2], levelDb: -12, at: at(0))
        tracker.forget(1)
        XCTAssertEqual(tracker.snapshot(at: at(1)).activeSpeakers, [2])
    }

    func testLocalSpeechNeedsATransmittedFrameAboveTheFloor() {
        var tracker = VoiceActivityTracker()

        tracker.captured(levelDb: -20, transmitted: false, at: at(0))
        XCTAssertFalse(tracker.snapshot(at: at(0)).isLocalSpeaking, "a VAD-gated frame is not speech")
        XCTAssertEqual(tracker.snapshot(at: at(0)).inputLevelDb, -20, "but the input meter still moves")

        tracker.captured(levelDb: VoiceActivityTracker.localSpeechFloorDb, transmitted: true, at: at(20))
        XCTAssertFalse(tracker.snapshot(at: at(20)).isLocalSpeaking, "room noise at the floor is not speech")

        tracker.captured(levelDb: -30, transmitted: true, at: at(40))
        XCTAssertTrue(tracker.snapshot(at: at(40)).isLocalSpeaking)
        XCTAssertTrue(tracker.snapshot(at: at(539)).isLocalSpeaking)
        XCTAssertFalse(tracker.snapshot(at: at(540)).isLocalSpeaking)
    }

    func testClearLocalSpeechResetsTheLaneAndMeterAndStampsTheGeneration() {
        var tracker = VoiceActivityTracker()
        tracker.captured(levelDb: -10, transmitted: true, at: at(0))
        tracker.clearLocalSpeech(generation: 3)

        let meters = tracker.snapshot(at: at(1))
        XCTAssertFalse(meters.isLocalSpeaking)
        XCTAssertEqual(meters.inputLevelDb, VoiceMeters.floorDb)
        XCTAssertEqual(meters.localSpeechGeneration, 3)
    }
}

final class MeterThrottleTests: XCTestCase {
    private let t0 = ContinuousClock.now

    private func at(_ ms: Int) -> ContinuousClock.Instant {
        t0.advanced(by: .milliseconds(ms))
    }

    private func meters(_ level: Float) -> VoiceMeters {
        VoiceMeters(inputLevelDb: level)
    }

    func testPublishesAChangeAtOnceThenAtMostOncePerInterval() {
        var throttle = MeterThrottle(interval: .milliseconds(33))

        XCTAssertTrue(throttle.shouldPublish(meters(-20), at: at(0)))
        XCTAssertFalse(throttle.shouldPublish(meters(-19), at: at(10)), "too soon")
        XCTAssertFalse(throttle.shouldPublish(meters(-18), at: at(32)), "too soon")
        XCTAssertTrue(throttle.shouldPublish(meters(-18), at: at(33)))
        XCTAssertEqual(throttle.lastPublished, meters(-18))
    }

    func testDoesNotRepublishAnUnchangedSnapshot() {
        var throttle = MeterThrottle(interval: .milliseconds(33))
        XCTAssertFalse(throttle.shouldPublish(VoiceMeters(), at: at(0)), "the UI starts at silence")
        XCTAssertTrue(throttle.shouldPublish(meters(-20), at: at(0)))
        XCTAssertFalse(throttle.shouldPublish(meters(-20), at: at(100)))
    }
}

// MARK: - Engine

private extension Duration {
    var seconds: Double { Double(components.seconds) + Double(components.attoseconds) / 1e18 }
    var milliseconds: Double { seconds * 1000 }
}

/// Datagrams the engine sent, with when it sent them.
private final class DatagramRecorder: Sendable {
    private let sent = OSAllocatedUnfairLock<[(at: ContinuousClock.Instant, datagram: Data)]>(initialState: [])

    var datagrams: [Data] { sent.withLock { $0.map(\.datagram) } }
    var times: [ContinuousClock.Instant] { sent.withLock { $0.map(\.at) } }

    func record(_ datagram: Data) {
        let now = ContinuousClock.now
        sent.withLock { $0.append((now, datagram)) }
    }
}

private struct RecordingLink: VoiceDatagramLink {
    let recorder: DatagramRecorder

    func send(_ datagram: Data, completion: @escaping @Sendable ((any Error)?) -> Void) {
        recorder.record(datagram)
        completion(nil)
    }
}

/// What reached the playback output, and on which thread.
private final class PlaybackRecorder: Sendable {
    private let state = OSAllocatedUnfairLock(initialState: (frames: 0, onMainThread: 0, started: false))

    var frames: Int { state.withLock { $0.frames } }
    var framesOnMainThread: Int { state.withLock { $0.onMainThread } }
    var started: Bool { state.withLock { $0.started } }

    func setStarted(_ value: Bool) { state.withLock { $0.started = value } }

    func frame(onMainThread: Bool) {
        state.withLock {
            $0.frames += 1
            if onMainThread { $0.onMainThread += 1 }
        }
    }
}

private final class RecordingOutput: VoiceOutput {
    let recorder: PlaybackRecorder

    init(recorder: PlaybackRecorder) {
        self.recorder = recorder
    }

    func start() { recorder.setStarted(true) }
    func stop() { recorder.setStarted(false) }
    func setVolume(_ volume: Float) {}
    func enqueue(pcm: [Int16]) { recorder.frame(onMainThread: Thread.isMainThread) }
}

/// Counts meter snapshots as the main actor would receive them.
private final class MeterCounter: Sendable {
    private let state = OSAllocatedUnfairLock(initialState: [VoiceMeters]())

    var snapshots: [VoiceMeters] { state.withLock { $0 } }

    func record(_ meters: VoiceMeters) {
        state.withLock { $0.append(meters) }
    }
}

final class VoiceEngineTests: XCTestCase {
    private static let sessionId: UInt32 = 7
    private static let channel = "lobby"
    /// What the client builds the sender with at login.
    private static let placeholderKey = Data(repeating: 0x42, count: 32)
    /// The first MLS-derived key, at epoch 0.
    private static let firstKey = Data(repeating: 0x11, count: 32)

    /// A 20 ms, 440 Hz tone at about -9 dBFS: loud enough to count as speech.
    private static let toneFrame: [Float] = (0..<960).map { Float(sin(Double($0) * 2 * .pi * 440 / 48_000)) * 0.5 }

    /// A sender as the client builds it at login: placeholder key, epoch 0.
    private func makeSender() throws -> AudioSenderWrapper {
        let sender = try AudioSenderWrapper(sessionId: Self.sessionId, key: Self.placeholderKey)
        sender.setEpoch(epoch: 0)
        return sender
    }

    private func rotation(
        _ channel: String = VoiceEngineTests.channel,
        key: Data = VoiceEngineTests.firstKey,
        epoch: UInt64 = 0
    ) -> VoiceCommand {
        .rotateKeys(VoiceKeyRotation(channelId: channel, sendKey: key, epoch: epoch, receivers: []))
    }

    /// Logged in, in `channel`, with a sender that still has only the
    /// placeholder key, sending into `link`.
    private func makeGatedEngine(
        link: DatagramRecorder,
        sender: AudioSenderWrapper,
        playback: PlaybackRecorder = PlaybackRecorder(),
        receiver: AudioReceiverWrapper = AudioReceiverWrapper()
    ) -> VoiceEngine {
        let engine = VoiceEngine(output: RecordingOutput(recorder: playback))
        engine.post(.attach(sender: sender, receiver: receiver, masterVolume: 1))
        engine.post(.setAuthenticated(true))
        engine.post(.setLink(RecordingLink(recorder: link)))
        engine.post(.setVoiceChannel(Self.channel))
        return engine
    }

    /// As `makeGatedEngine`, after the channel's first MLS key (`firstKey`,
    /// epoch 0) has arrived.
    private func makeSendingEngine(
        link: DatagramRecorder,
        playback: PlaybackRecorder = PlaybackRecorder(),
        receiver: AudioReceiverWrapper = AudioReceiverWrapper()
    ) throws -> VoiceEngine {
        let engine = makeGatedEngine(link: link, sender: try makeSender(), playback: playback, receiver: receiver)
        engine.post(rotation())
        return engine
    }

    /// A measurement, kept in the result bundle as an activity.
    private func report(_ message: String) async {
        print("[VoiceEngineTests] \(message)")
        await MainActor.run { XCTContext.runActivity(named: message) { _ in } }
    }

    /// `[0x01][session u32][epoch u16][sequence u16]…`, big-endian.
    private func header(_ datagram: Data) -> (epoch: UInt16, sequence: UInt16) {
        let bytes = [UInt8](datagram)
        return (UInt16(bytes[5]) << 8 | UInt16(bytes[6]), UInt16(bytes[7]) << 8 | UInt16(bytes[8]))
    }

    func testNothingIsSentBeforeLoginOrWithoutASender() async throws {
        let link = DatagramRecorder()
        let engine = VoiceEngine(output: RecordingOutput(recorder: PlaybackRecorder()))
        defer { engine.shutdown() }
        engine.post(.setLink(RecordingLink(recorder: link)))

        engine.submitCaptured(Self.toneFrame)  // no sender yet
        engine.post(.attach(sender: try makeSender(), receiver: AudioReceiverWrapper(), masterVolume: 1))
        engine.post(.setVoiceChannel(Self.channel))
        engine.post(rotation())
        engine.submitCaptured(Self.toneFrame)  // not logged in
        engine.post(.setAuthenticated(true))
        engine.submitCaptured(Self.toneFrame)
        engine.post(.setAuthenticated(false))
        engine.submitCaptured(Self.toneFrame)
        await engine.flush()

        XCTAssertEqual(link.datagrams.count, 1)
    }

    func testAKeyRotationLandsBetweenFramesInTheOrderPosted() async throws {
        let link = DatagramRecorder()
        let engine = try makeSendingEngine(link: link)
        defer { engine.shutdown() }

        for _ in 0..<5 { engine.submitCaptured(Self.toneFrame) }
        engine.post(rotation(key: Data(repeating: 0x22, count: 32), epoch: 7))
        for _ in 0..<5 { engine.submitCaptured(Self.toneFrame) }
        await engine.flush()

        let headers = link.datagrams.map(header)
        XCTAssertEqual(headers.map(\.epoch), [0, 0, 0, 0, 0, 7, 7, 7, 7, 7])
        XCTAssertEqual(headers.map(\.sequence), (0..<10).map { UInt16($0) }, "the rotation keeps the sequence running")
    }

    func testReceivedAudioIsDecodedAndPlayedOffTheMainThreadAndItsSpeakerTimesOut() async throws {
        // Produce real packets with one engine and feed them to another.
        let link = DatagramRecorder()
        let sending = try makeSendingEngine(link: link)
        defer { sending.shutdown() }
        for _ in 0..<25 { sending.submitCaptured(Self.toneFrame) }
        await sending.flush()
        XCTAssertEqual(link.datagrams.count, 25)

        let playback = PlaybackRecorder()
        let receiving = VoiceEngine(output: RecordingOutput(recorder: playback))
        defer { receiving.shutdown() }
        let counter = MeterCounter()
        let meters = receiving.meters
        let watcher = Task {
            for await snapshot in meters { counter.record(snapshot) }
        }
        defer { watcher.cancel() }

        receiving.post(.attach(sender: try makeSender(), receiver: AudioReceiverWrapper(), masterVolume: 1))
        receiving.post(.addSender(sessionId: Self.sessionId, key: Self.firstKey, epochHint: 0, prefs: VoiceMixerPrefs()))
        for datagram in link.datagrams {
            receiving.submitReceived(Data(datagram.dropFirst()))
        }
        await receiving.flush()

        XCTAssertTrue(playback.started)
        XCTAssertGreaterThan(playback.frames, 0)
        XCTAssertEqual(playback.framesOnMainThread, 0)

        // The speaker shows up, then drops out ~500ms after the last frame.
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(counter.snapshots.contains { $0.activeSpeakers == [Self.sessionId] })
        try await Task.sleep(for: .milliseconds(700))
        XCTAssertEqual(counter.snapshots.last?.activeSpeakers, [])
        XCTAssertEqual(counter.snapshots.last?.outputLevelDb, VoiceMeters.floorDb)
    }

    func testLocalMuteHidesTheSpeakerAtOnce() async throws {
        let link = DatagramRecorder()
        let sending = try makeSendingEngine(link: link)
        defer { sending.shutdown() }
        for _ in 0..<10 { sending.submitCaptured(Self.toneFrame) }
        await sending.flush()

        let receiving = VoiceEngine(output: RecordingOutput(recorder: PlaybackRecorder()))
        defer { receiving.shutdown() }
        let counter = MeterCounter()
        let meters = receiving.meters
        let watcher = Task { for await snapshot in meters { counter.record(snapshot) } }
        defer { watcher.cancel() }

        receiving.post(.attach(sender: try makeSender(), receiver: AudioReceiverWrapper(), masterVolume: 1))
        receiving.post(.addSender(sessionId: Self.sessionId, key: Self.firstKey, epochHint: 0, prefs: VoiceMixerPrefs()))
        for datagram in link.datagrams { receiving.submitReceived(Data(datagram.dropFirst())) }
        await receiving.flush()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(counter.snapshots.last?.activeSpeakers, [Self.sessionId])

        receiving.post(.setSenderMuted(sessionId: Self.sessionId, muted: true))
        await receiving.flush()
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(counter.snapshots.last?.activeSpeakers, [], "well inside the 500ms timeout")
    }

    /// Sends 1 s of audio at the capture cadence from a background thread
    /// while the main thread is blocked for half of it. Frames used to hop
    /// through the main actor, so a blocked main thread stalled them all; the
    /// old path is reproduced alongside for comparison.
    func testFramesKeepFlowingWhileTheMainThreadIsBlocked() async throws {
        let frameCount = 50
        let blockStart = Duration.milliseconds(200)
        let blockLength = Duration.milliseconds(500)

        let link = DatagramRecorder()
        let engine = try makeSendingEngine(link: link)
        defer { engine.shutdown() }

        // The old path, minus the encoder: every frame is a main-actor job.
        let oldPath = DatagramRecorder()

        let start = ContinuousClock.now
        let feeder = Thread {
            for i in 0..<frameCount {
                let due = start.advanced(by: .milliseconds(20 * i))
                let wait = due - ContinuousClock.now
                if wait > .zero { Thread.sleep(forTimeInterval: wait.seconds) }
                engine.submitCaptured(Self.toneFrame)
                Task { @MainActor in oldPath.record(Data()) }
            }
        }
        feeder.start()

        try await Task.sleep(until: start.advanced(by: blockStart))
        await MainActor.run {
            Thread.sleep(forTimeInterval: blockLength.seconds)
        }
        try await Task.sleep(until: start.advanced(by: .milliseconds(20 * frameCount + 200)))
        await engine.flush()

        let blockedFrom = start.advanced(by: blockStart)
        let blockedUntil = blockedFrom.advanced(by: blockLength)
        func stats(_ times: [ContinuousClock.Instant]) -> (duringBlock: Int, maxGapMs: Double) {
            let during = times.filter { $0 > blockedFrom.advanced(by: .milliseconds(20)) && $0 < blockedUntil }.count
            let gaps = zip(times.dropFirst(), times).map { $0 - $1 }
            return (during, (gaps.max() ?? .zero).milliseconds)
        }

        let new = stats(link.times)
        let old = stats(oldPath.times)
        await report("main blocked \(blockLength): engine sent \(link.datagrams.count)/\(frameCount), \(new.duringBlock) during the block, max gap \(String(format: "%.1f", new.maxGapMs)) ms; main-actor path delivered \(old.duringBlock) during the block, max gap \(String(format: "%.1f", old.maxGapMs)) ms")

        XCTAssertEqual(link.datagrams.count, frameCount)
        XCTAssertGreaterThanOrEqual(new.duringBlock, 15, "frames must keep going out while the main thread is blocked")
        XCTAssertLessThan(new.maxGapMs, 200)
        XCTAssertEqual(old.duringBlock, 0, "sanity: the main-actor path stalls")
    }

    /// One second of talking in both directions reaches the main actor as at
    /// most ~30 meter snapshots, however many frames there were.
    func testMeterSnapshotsAreRateLimited() async throws {
        let link = DatagramRecorder()
        let engine = try makeSendingEngine(link: link)
        defer { engine.shutdown() }
        let counter = MeterCounter()
        let meters = engine.meters
        let watcher = Task { for await snapshot in meters { counter.record(snapshot) } }
        defer { watcher.cancel() }

        let start = ContinuousClock.now
        for i in 0..<50 {
            // Vary the level every frame so every frame changes the meters.
            let gain = Float(i % 10 + 1) / 10
            engine.submitCaptured(Self.toneFrame.map { $0 * gain })
            try await Task.sleep(until: start.advanced(by: .milliseconds(20 * (i + 1))))
        }
        await engine.flush()
        let elapsedMs = (ContinuousClock.now - start).milliseconds

        let count = counter.snapshots.count
        await report("50 frames over \(Int(elapsedMs)) ms -> \(count) meter snapshots for the main actor")
        XCTAssertGreaterThan(count, 1)
        XCTAssertLessThanOrEqual(count, Int(elapsedMs / 33) + 2)
    }

    // MARK: Sending gate

    func testThePlaceholderKeyNeverEncryptsAudioButTheMicStillMeters() async throws {
        let link = DatagramRecorder()
        let sender = try makeSender()
        let engine = makeGatedEngine(link: link, sender: sender)
        defer { engine.shutdown() }
        let counter = MeterCounter()
        let meters = engine.meters
        let watcher = Task { for await snapshot in meters { counter.record(snapshot) } }
        defer { watcher.cancel() }

        for _ in 0..<5 { engine.submitCaptured(Self.toneFrame) }
        await engine.flush()
        try await Task.sleep(for: .milliseconds(100))

        XCTAssertEqual(link.datagrams.count, 0)
        XCTAssertEqual(sender.sequence(), 0, "no frame reached the encoder")
        let last = try XCTUnwrap(counter.snapshots.last)
        XCTAssertGreaterThan(last.inputLevelDb, VoiceMeters.floorDb, "the input meter still moves")
        XCTAssertFalse(last.isLocalSpeaking, "nothing is going out, so the local lane stays quiet")
    }

    func testSendingStartsAfterTheFirstRotationForTheCurrentChannel() async throws {
        let link = DatagramRecorder()
        let engine = makeGatedEngine(link: link, sender: try makeSender())
        defer { engine.shutdown() }

        for _ in 0..<3 { engine.submitCaptured(Self.toneFrame) }
        engine.post(rotation(epoch: 3))
        for _ in 0..<3 { engine.submitCaptured(Self.toneFrame) }
        await engine.flush()

        let headers = link.datagrams.map(header)
        XCTAssertEqual(headers.map(\.epoch), [3, 3, 3])
        XCTAssertEqual(headers.map(\.sequence), [0, 1, 2])
    }

    func testARotationForAnotherChannelDoesNotOpenTheGate() async throws {
        let link = DatagramRecorder()
        let sender = try makeSender()
        let engine = makeGatedEngine(link: link, sender: sender)
        defer { engine.shutdown() }

        engine.post(rotation("elsewhere", epoch: 3))
        for _ in 0..<3 { engine.submitCaptured(Self.toneFrame) }
        await engine.flush()

        XCTAssertEqual(link.datagrams.count, 0)
        XCTAssertEqual(sender.sequence(), 0)
    }

    func testTheGateClosesOnChannelSwitchUntilTheNewChannelsKey() async throws {
        let link = DatagramRecorder()
        let engine = makeGatedEngine(link: link, sender: try makeSender())
        defer { engine.shutdown() }

        engine.post(rotation(epoch: 1))
        for _ in 0..<2 { engine.submitCaptured(Self.toneFrame) }
        engine.post(.setVoiceChannel("elsewhere"))
        for _ in 0..<2 { engine.submitCaptured(Self.toneFrame) }  // gated
        engine.post(rotation(Self.channel, epoch: 2))              // the old channel's key
        for _ in 0..<2 { engine.submitCaptured(Self.toneFrame) }  // still gated
        engine.post(rotation("elsewhere", key: Data(repeating: 0x33, count: 32), epoch: 5))
        for _ in 0..<2 { engine.submitCaptured(Self.toneFrame) }
        await engine.flush()

        XCTAssertEqual(link.datagrams.map(header).map(\.epoch), [1, 1, 5, 5])
    }

    func testTheGateClosesOnLogoutAndANewSessionStartsWithoutAKey() async throws {
        let link = DatagramRecorder()
        let engine = makeGatedEngine(link: link, sender: try makeSender())
        defer { engine.shutdown() }

        engine.post(rotation(epoch: 1))
        engine.submitCaptured(Self.toneFrame)                     // sent
        engine.post(.setAuthenticated(false))                     // connection lost
        engine.submitCaptured(Self.toneFrame)
        engine.post(.setAuthenticated(true))                      // logged in again…
        engine.submitCaptured(Self.toneFrame)                     // …but the key went with the session
        engine.post(.attach(sender: try makeSender(), receiver: AudioReceiverWrapper(), masterVolume: 1))
        engine.submitCaptured(Self.toneFrame)                     // new session: placeholder only
        engine.post(rotation(epoch: 2))
        engine.submitCaptured(Self.toneFrame)                     // sent
        await engine.flush()

        let headers = link.datagrams.map(header)
        XCTAssertEqual(headers.map(\.epoch), [1, 2])
        XCTAssertEqual(headers.map(\.sequence), [0, 0], "the second packet comes from the new session's sender")
    }
}
