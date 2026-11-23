import AuraCore
import Foundation
import Network

// MARK: - What the UI sees

/// The voice UI's view of the audio: meters and who is talking. The engine
/// publishes one of these at most every `VoiceEngine.publishInterval`, and only
/// when it changed, so the main actor never handles individual frames.
nonisolated struct VoiceMeters: Sendable, Equatable {
    /// Quietest level the meters represent; everything below reads as silence.
    static let floorDb: Float = -60

    /// Level of the latest captured frame, in whole dBFS.
    var inputLevelDb: Float = floorDb
    /// Level of the latest received mix while anyone is talking, else the floor.
    var outputLevelDb: Float = floorDb
    /// Remote sessions heard within `VoiceActivityTracker.speakerTimeout`.
    var activeSpeakers: Set<UInt32> = []
    /// True while our transmitted frames are loud enough to count as speech.
    var isLocalSpeaking = false
    /// The latest `clearLocalSpeech` the engine has applied. The main actor
    /// clears the local fields itself when capture stops; a snapshot stamped
    /// with an older generation was taken before that and must not undo it.
    var localSpeechGeneration: UInt64 = 0
}

/// Talk and level bookkeeping behind `VoiceMeters`. A value type driven with
/// explicit timestamps, so the timing rules can be tested without waiting.
nonisolated struct VoiceActivityTracker {
    /// A remote speaker, or our own lane, stays active this long after the
    /// last frame that showed them talking.
    static let speakerTimeout: Duration = .milliseconds(500)

    /// Local speech is judged by the sender's VAD, but VAD only runs in
    /// voice-activation mode: in always-on and push-to-talk every frame becomes
    /// a packet. This floor keeps the local lane from filling with room noise.
    static let localSpeechFloorDb: Float = -45

    private var inputLevelDb = VoiceMeters.floorDb
    private var mixLevelDb = VoiceMeters.floorDb
    private var lastHeard: [UInt32: ContinuousClock.Instant] = [:]
    private var lastLocalSpeech: ContinuousClock.Instant?
    private var localSpeechGeneration: UInt64 = 0

    /// True while a speaker or our own lane is waiting to time out.
    var hasPendingTimeouts: Bool { !lastHeard.isEmpty || lastLocalSpeech != nil }

    /// A captured frame. `transmitted` is false when it produced no packet
    /// (VAD silence, not logged in, no sender, or an encoder error).
    mutating func captured(levelDb: Float, transmitted: Bool, at now: ContinuousClock.Instant) {
        inputLevelDb = levelDb
        if transmitted && levelDb > Self.localSpeechFloorDb {
            lastLocalSpeech = now
        }
    }

    /// A mix popped from the receiver, with the sessions that contributed.
    mutating func mixed(speakers: [UInt32], levelDb: Float, at now: ContinuousClock.Instant) {
        mixLevelDb = levelDb
        for id in speakers {
            lastHeard[id] = now
        }
    }

    /// Capture stopped: drop the local lane and the input meter at once.
    mutating func clearLocalSpeech(generation: UInt64) {
        localSpeechGeneration = generation
        lastLocalSpeech = nil
        inputLevelDb = VoiceMeters.floorDb
    }

    /// A speaker was muted locally; the mixer stops reporting them, so stop
    /// showing them now rather than when their timeout runs out.
    mutating func forget(_ sessionId: UInt32) {
        lastHeard.removeValue(forKey: sessionId)
    }

    /// The meters at `now`, after dropping everyone whose timeout has run out.
    mutating func snapshot(at now: ContinuousClock.Instant) -> VoiceMeters {
        lastHeard = lastHeard.filter { now - $0.value < Self.speakerTimeout }
        if let last = lastLocalSpeech, now - last >= Self.speakerTimeout {
            lastLocalSpeech = nil
        }
        return VoiceMeters(
            inputLevelDb: inputLevelDb,
            outputLevelDb: lastHeard.isEmpty ? VoiceMeters.floorDb : mixLevelDb,
            activeSpeakers: Set(lastHeard.keys),
            isLocalSpeaking: lastLocalSpeech != nil,
            localSpeechGeneration: localSpeechGeneration
        )
    }
}

/// Rate limit for meter publishing: a snapshot goes out only when it differs
/// from the last one sent, and never sooner than `interval` after it.
nonisolated struct MeterThrottle {
    let interval: Duration
    private(set) var lastPublished = VoiceMeters()
    private var lastPublishedAt: ContinuousClock.Instant?

    init(interval: Duration) {
        self.interval = interval
    }

    /// Whether `meters` should be published now; records it if so.
    mutating func shouldPublish(_ meters: VoiceMeters, at now: ContinuousClock.Instant) -> Bool {
        guard meters != lastPublished else { return false }
        if let last = lastPublishedAt, now - last < interval { return false }
        lastPublished = meters
        lastPublishedAt = now
        return true
    }
}

/// RMS levels in whole dBFS, floored at `VoiceMeters.floorDb`. Quantized: the
/// meters read in integers anyway, and it keeps float jitter from counting as
/// a change on every frame.
nonisolated enum VoiceLevelMeter {
    static func levelDb(_ pcm: [Float]) -> Float {
        guard !pcm.isEmpty else { return VoiceMeters.floorDb }
        var sumSquares: Float = 0
        for sample in pcm { sumSquares += sample * sample }
        let rms = (sumSquares / Float(pcm.count)).squareRoot()
        guard rms > 0 else { return VoiceMeters.floorDb }
        return max(VoiceMeters.floorDb, (20 * log10(rms)).rounded())
    }

    static func levelDb(_ pcm: [Int16]) -> Float {
        guard !pcm.isEmpty else { return VoiceMeters.floorDb }
        var sumSquares: Double = 0
        for sample in pcm {
            let normalized = Double(sample) / 32768.0
            sumSquares += normalized * normalized
        }
        let rms = (sumSquares / Double(pcm.count)).squareRoot()
        guard rms > 0 else { return VoiceMeters.floorDb }
        return max(VoiceMeters.floorDb, Float((20 * log10(rms)).rounded()))
    }
}

// MARK: - Inputs

/// Where audio datagrams go: the QUIC connection group, or a test double.
nonisolated protocol VoiceDatagramLink: Sendable {
    func send(_ datagram: Data, completion: @escaping @Sendable ((any Error)?) -> Void)
}

nonisolated struct ConnectionGroupLink: VoiceDatagramLink {
    let group: NWConnectionGroup

    func send(_ datagram: Data, completion: @escaping @Sendable ((any Error)?) -> Void) {
        group.send(content: datagram) { error in completion(error) }
    }
}

/// Where mixed PCM goes: `AudioPlayback`, or a test double. Owned by the
/// engine and only used on its executor, so it needn't be Sendable.
nonisolated protocol VoiceOutput: AnyObject {
    func start()
    func stop()
    func setVolume(_ volume: Float)
    func enqueue(pcm: [Int16])
}

/// Local-only mixer choices for one remote session, resolved from its user id
/// on the main actor.
nonisolated struct VoiceMixerPrefs: Sendable, Equatable {
    /// Playback gain, when the user set one.
    var gain: Float?
    /// Locally muted. Only `true` is applied, as before: unmuting goes through
    /// `setSenderMuted`.
    var muted = false
}

/// One remote session's key for the current epoch.
nonisolated struct VoiceReceiverKey: Sendable {
    let sessionId: UInt32
    let key: Data
    let prefs: VoiceMixerPrefs
}

/// Keys for one voice-group epoch, applied in a single step between frames.
nonisolated struct VoiceKeyRotation: Sendable {
    /// The voice channel whose MLS group these keys come from.
    let channelId: String
    let sendKey: Data
    let epoch: UInt64
    let receivers: [VoiceReceiverKey]
}

/// Changes to the audio domain, applied in order with the frames around them.
nonisolated enum VoiceCommand: Sendable {
    /// A freshly built sender and receiver. The main actor configures them and
    /// then hands them over; from here on only the engine uses them. The
    /// sender holds a placeholder key, so sending stays closed until a
    /// rotation for the current channel.
    case attach(sender: AudioSenderWrapper, receiver: AudioReceiverWrapper, masterVolume: Float)
    /// Mirrors `QuicNetworkClient.isAuthenticated`: nothing is sent while
    /// false, and logging out discards the sending key.
    case setAuthenticated(Bool)
    /// Mirrors `QuicNetworkClient.currentVoiceChannelId`. A different channel
    /// needs its own key: sending closes until one is installed.
    case setVoiceChannel(String?)
    /// The connection group audio goes out on, or nil when there is none.
    case setLink((any VoiceDatagramLink)?)
    /// Our sending key and every known receiver key for the current channel.
    /// Ignored when `channelId` isn't the current channel.
    case rotateKeys(VoiceKeyRotation)
    case addSender(sessionId: UInt32, key: Data, epochHint: UInt16, prefs: VoiceMixerPrefs)
    case removeSender(sessionId: UInt32)
    case applyMixerPrefs(sessionId: UInt32, prefs: VoiceMixerPrefs)
    case setSenderGain(sessionId: UInt32, gain: Float)
    case setSenderMuted(sessionId: UInt32, muted: Bool)
    case applySettings(AudioSettingsChange)
    case clearLocalSpeech(generation: UInt64)
    /// Runs `reached` once everything posted before it has been handled.
    case barrier(@Sendable () -> Void)
}

nonisolated enum VoiceEvent: Sendable {
    case captured([Float])
    case received(Data)
    case command(VoiceCommand)
}

// MARK: - Engine

/// The audio domain. Encodes, encrypts and sends captured frames; decrypts,
/// decodes and plays received ones; keeps the meters. None of it runs on, or
/// waits for, the main actor.
///
/// It owns the `AudioSenderWrapper`, `AudioReceiverWrapper`, the playback
/// output and the datagram link. The main actor keeps MLS, membership and the
/// persisted mixer preferences, derives keys there, and sends the results here
/// as commands. Frames and commands share one ordered stream, so a key change
/// lands between two frames — never in the middle of one — and in the order
/// the main actor issued it.
///
/// The actor runs on its own serial queue rather than the shared cooperative
/// pool, so long-running tasks elsewhere (a backup's scrypt, say) cannot
/// delay a frame. Callers on the tap and network threads only `yield`: no
/// Task per frame, and nothing for them to wait on.
actor VoiceEngine {
    /// Meter publishing period: about 30 Hz.
    static let publishInterval: Duration = .milliseconds(33)

    private nonisolated let queue: DispatchSerialQueue
    nonisolated var unownedExecutor: UnownedSerialExecutor {
        queue.asUnownedSerialExecutor()
    }

    private nonisolated let input: AsyncStream<VoiceEvent>.Continuation
    private nonisolated let meterInput: AsyncStream<VoiceMeters>.Continuation

    /// Meter snapshots for the UI. Holds only the newest one, so a busy main
    /// actor skips stale snapshots instead of working through a backlog.
    /// Single consumer.
    nonisolated let meters: AsyncStream<VoiceMeters>

    private var sender: AudioSenderWrapper?
    private var receiver: AudioReceiverWrapper?
    private var link: (any VoiceDatagramLink)?
    private let output: any VoiceOutput
    private var isAuthenticated = false

    /// The voice channel we are in, as the main actor last said.
    private var voiceChannelId: String?

    /// The channel whose MLS-derived key the sender holds, in this session.
    /// Nil while the sender still has its placeholder key — which must never
    /// encrypt audio, since anyone who knows the constant could decrypt it.
    private var keyedChannelId: String?

    /// Sending is open only with an MLS key for the channel we are in.
    private var hasChannelKey: Bool {
        keyedChannelId != nil && keyedChannelId == voiceChannelId
    }

    /// Datagrams sent, for the every-100th send log.
    private var sequenceNumber: UInt16 = 0

    private var tracker = VoiceActivityTracker()
    private var throttle: MeterThrottle
    private var ticker: Task<Void, Never>?
    private var lastEventAt = ContinuousClock.now

    /// How long the publishing loop outlives the last event. Keeps it running
    /// through steady capture whose meters don't change, instead of stopping
    /// and being restarted by the next frame.
    private static let publishLinger: Duration = .seconds(1)

    init(output: sending any VoiceOutput, publishInterval: Duration = VoiceEngine.publishInterval) {
        queue = DispatchSerialQueue(label: "com.klobucar.aura.voice", qos: .userInteractive)
        self.output = output
        throttle = MeterThrottle(interval: publishInterval)

        let (events, input) = AsyncStream.makeStream(of: VoiceEvent.self)
        self.input = input
        let (meters, meterInput) = AsyncStream.makeStream(
            of: VoiceMeters.self,
            bufferingPolicy: .bufferingNewest(1)
        )
        self.meters = meters
        self.meterInput = meterInput

        // Ends when `shutdown()` finishes the stream.
        Task { await self.run(events) }
    }

    // MARK: Entry points (any thread)

    /// A captured 20 ms frame. Called on the capture tap's thread.
    nonisolated func submitCaptured(_ pcm: [Float]) {
        input.yield(.captured(pcm))
    }

    /// An audio packet from the network, without the datagram type byte.
    nonisolated func submitReceived(_ packet: Data) {
        input.yield(.received(packet))
    }

    nonisolated func post(_ command: VoiceCommand) {
        input.yield(.command(command))
    }

    /// Returns once everything posted before the call has been handled.
    func flush() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            post(.barrier { continuation.resume() })
        }
    }

    /// Stops the engine: no more events are handled and playback stops.
    nonisolated func shutdown() {
        input.finish()
    }

    // MARK: Event loop

    private func run(_ events: AsyncStream<VoiceEvent>) async {
        for await event in events {
            switch event {
            case .captured(let pcm):
                transmit(pcm)
            case .received(let packet):
                receive(packet)
            case .command(let command):
                apply(command)
            }
            lastEventAt = .now
            ensureTicking()
        }
        ticker?.cancel()
        ticker = nil
        output.stop()
        meterInput.finish()
    }

    // MARK: Send path

    private func transmit(_ pcm: [Float]) {
        let now = ContinuousClock.now
        // Meter every captured frame, gated or not, so the HUD's input meter
        // still moves while VAD holds the transmit gate shut.
        let levelDb = VoiceLevelMeter.levelDb(pcm)

        // Without a key for this channel the frame isn't even encoded: the
        // sender would encrypt it under the placeholder key.
        guard isAuthenticated, let sender, hasChannelKey else {
            tracker.captured(levelDb: levelDb, transmitted: false, at: now)
            return
        }

        let packet: Data
        do {
            // nil when VAD is enabled and the frame is silence.
            guard let bytes = try sender.processFloat(pcm: pcm) else {
                tracker.captured(levelDb: levelDb, transmitted: false, at: now)
                return
            }
            packet = Data(bytes)
        } catch {
            print("[QuicClient] Audio encoding error: \(error)")
            tracker.captured(levelDb: levelDb, transmitted: false, at: now)
            return
        }

        // A packet came out of the gate: the local half of the talk lanes.
        tracker.captured(levelDb: levelDb, transmitted: true, at: now)

        guard let link else {
            print("[QuicClient] Cannot send audio: no connection group")
            return
        }

        var datagram = Data([0x01])  // Audio datagram type
        datagram.append(packet)

        let logSend = sequenceNumber % 100 == 0
        link.send(datagram) { error in
            if let error {
                print("[QuicClient] ✗ Failed to send audio datagram: \(error)")
            } else if logSend {
                print("[QuicClient] ✓ Sent audio packet #\(sender.sequence()) (67 bytes) via datagram")
            }
        }
        sequenceNumber &+= 1
    }

    // MARK: Receive path

    private func receive(_ packet: Data) {
        guard let receiver else { return }
        do {
            try receiver.onPacket(data: packet)
            // The Rust core mixes what is ready, with PLC/DRED and talk
            // detection inside.
            if let result = receiver.popMixed() {
                tracker.mixed(
                    speakers: result.activeSpeakers,
                    levelDb: VoiceLevelMeter.levelDb(result.pcm),
                    at: .now
                )
                output.enqueue(pcm: result.pcm)
            }
        } catch {
            print("[QuicClient] Audio processing error: \(error)")
        }
    }

    // MARK: Commands

    private func apply(_ command: VoiceCommand) {
        switch command {
        case .attach(let sender, let receiver, let masterVolume):
            self.sender = sender
            self.receiver = receiver
            closeSending(because: "a new session's sender has no channel key yet")
            output.setVolume(masterVolume)
            output.start()

        case .setAuthenticated(let authenticated):
            isAuthenticated = authenticated
            if !authenticated {
                closeSending(because: "logged out")
            }

        case .setVoiceChannel(let channelId):
            guard channelId != voiceChannelId else { return }
            voiceChannelId = channelId
            closeSending(because: "switched to \(channelId ?? "no channel")")

        case .setLink(let link):
            self.link = link

        case .rotateKeys(let rotation):
            rotateKeys(rotation)

        case .addSender(let sessionId, let key, let epochHint, let prefs):
            guard let receiver else { return }
            do {
                try receiver.addSender(sessionId: sessionId, key: key, epochHint: epochHint)
                applyMixerPrefs(prefs, to: sessionId)
                print("[QuicClient] Added audio sender \(sessionId) with MLS key")
            } catch {
                print("[QuicClient] Failed to add audio sender \(sessionId): \(error)")
            }

        case .removeSender(let sessionId):
            receiver?.removeSender(sessionId: sessionId)

        case .applyMixerPrefs(let sessionId, let prefs):
            applyMixerPrefs(prefs, to: sessionId)

        case .setSenderGain(let sessionId, let gain):
            receiver?.setSenderGain(sessionId: sessionId, gain: gain)

        case .setSenderMuted(let sessionId, let muted):
            receiver?.setSenderMuted(sessionId: sessionId, muted: muted)
            if muted { tracker.forget(sessionId) }

        case .applySettings(let change):
            applySettings(change)

        case .clearLocalSpeech(let generation):
            tracker.clearLocalSpeech(generation: generation)

        case .barrier(let reached):
            reached()
        }
    }

    private func closeSending(because reason: String) {
        guard keyedChannelId != nil else { return }
        keyedChannelId = nil
        print("[QuicClient] Voice sending paused until an MLS key arrives: \(reason)")
    }

    /// Our new sending key first; if that fails, nothing else changes. Then
    /// every receiver key, adding senders the join race left out.
    private func rotateKeys(_ rotation: VoiceKeyRotation) {
        // Only the channel we are in may set our keys. The main actor checks
        // this too; checking here keeps the gate honest whatever arrives.
        guard rotation.channelId == voiceChannelId else {
            print("[QuicClient] Ignoring voice keys for \(rotation.channelId); we are in \(voiceChannelId ?? "no channel")")
            return
        }

        if let sender {
            do {
                // updateKey keeps the sequence number and encoder state.
                try sender.updateKey(key: rotation.sendKey, epoch: rotation.epoch)
                print("[QuicClient] Rotated audio sender key from MLS, epoch=\(rotation.epoch)")
                if keyedChannelId != rotation.channelId {
                    keyedChannelId = rotation.channelId
                    print("[QuicClient] Voice sending open for \(rotation.channelId)")
                }
            } catch {
                print("[QuicClient] Failed to rotate audio sender key: \(error)")
                return
            }
        }

        guard let receiver else { return }
        let epochHint = UInt16(rotation.epoch & 0xFFFF)
        for user in rotation.receivers {
            do {
                if try receiver.updateSenderKey(sessionId: user.sessionId, key: user.key, epochHint: epochHint) {
                    print("[QuicClient] Updated receiver key for user \(user.sessionId)")
                } else {
                    // A sender we missed during the initial join race.
                    print("[QuicClient] New user \(user.sessionId) found during key rotation, adding sender...")
                    try receiver.addSender(sessionId: user.sessionId, key: user.key, epochHint: epochHint)
                    applyMixerPrefs(user.prefs, to: user.sessionId)
                }
            } catch {
                print("[QuicClient] Failed to update/add audio key for user \(user.sessionId): \(error)")
            }
        }
    }

    private func applyMixerPrefs(_ prefs: VoiceMixerPrefs, to sessionId: UInt32) {
        guard let receiver else { return }
        if let gain = prefs.gain {
            receiver.setSenderGain(sessionId: sessionId, gain: gain)
        }
        if prefs.muted {
            receiver.setSenderMuted(sessionId: sessionId, muted: true)
            tracker.forget(sessionId)
        }
    }

    private func applySettings(_ change: AudioSettingsChange) {
        if let enabled = change.noiseSuppression {
            sender?.setNoiseSuppressionEnabled(enabled: enabled)
            print("[QuicClient] Noise suppression: \(enabled ? "enabled" : "disabled")")
        }
        if let enabled = change.aecEnabled {
            sender?.setWebrtcAecEnabled(enabled: enabled)
            print("[QuicClient] AEC: \(enabled ? "enabled" : "disabled")")
        }
        if let enabled = change.webrtcNsEnabled {
            sender?.setWebrtcNsEnabled(enabled: enabled)
            print("[QuicClient] WebRTC NS: \(enabled ? "enabled" : "disabled")")
        }
        if let enabled = change.webrtcAgcEnabled {
            sender?.setWebrtcAgcEnabled(enabled: enabled)
            print("[QuicClient] AGC: \(enabled ? "enabled" : "disabled")")
        }
        if let ms = change.jitterBuffer {
            receiver?.setJitterBufferMs(latencyMs: UInt32(ms))
            print("[QuicClient] Jitter buffer set to \(ms)ms")
        }
        if let volume = change.masterVolume {
            output.setVolume(volume)
            print("[QuicClient] Master volume: \(Int((volume * 100).rounded()))%")
        }
        if let enabled = change.vadEnabled {
            sender?.setVadEnabled(enabled: enabled)
            print("[QuicClient] VAD: \(enabled ? "enabled" : "disabled")")
        }
        if let thresholdDb = change.vadThresholdDb {
            sender?.setVadThresholdDb(thresholdDb: thresholdDb)
            print("[QuicClient] VAD threshold: \(thresholdDb) dB")
        }
    }

    // MARK: Meter publishing

    /// Starts the publishing loop if it isn't running. It stops by itself once
    /// events have stopped, nothing is left to time out and the UI has the
    /// current meters, so an idle engine doesn't wake 30 times a second.
    private func ensureTicking() {
        guard ticker == nil else { return }
        ticker = Task { await self.publishLoop() }
    }

    private func publishLoop() async {
        while !Task.isCancelled {
            let now = ContinuousClock.now
            let current = tracker.snapshot(at: now)
            if throttle.shouldPublish(current, at: now) {
                meterInput.yield(current)
            }
            if !tracker.hasPendingTimeouts && current == throttle.lastPublished
                && now - lastEventAt >= Self.publishLinger {
                break
            }
            try? await Task.sleep(for: throttle.interval)
        }
        ticker = nil
    }
}
