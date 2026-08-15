using System;
using System.Collections.Generic;
using System.Linq;

namespace Aura.Desktop.Services;

/// <summary>
/// One stretch of speech by one participant. <see cref="End"/> is null while the
/// participant is still talking.
/// </summary>
public sealed class TalkSegment
{
    public uint SessionId { get; init; }
    public DateTime Start { get; set; }
    public DateTime? End { get; set; }

    public bool IsOpen => End == null;

    public DateTime EndOr(DateTime now) => End ?? now;

    public TimeSpan Duration(DateTime now) => EndOr(now) - Start;
}

/// <summary>
/// A window-relative slice of a lane, in 0..1 fractions of the rolling window.
/// </summary>
public readonly record struct LaneSlice(double Start, double End, bool IsLive)
{
    public double Width => End - Start;
}

/// <summary>
/// Rolling talk history behind the voice rail: who has the floor now, who has
/// been talking, and who got talked over.
///
/// In-memory only — nothing here is persisted or sent on the wire. Trim on a 1s
/// timer (the UI owns the timer so the store stays testable and thread-free).
///
/// Two signals feed this store and they come from different places:
///   * remote speakers — the jitter buffer's talking signal
///     (<see cref="AudioManager.OnActiveSpeakersChanged"/>), which is derived
///     from decoded incoming audio and therefore *never* includes us;
///   * the local user — the capture path
///     (<see cref="AudioManager.OnLocalCapture"/> plus the input level), wired
///     separately by the view model. Without that the local lane is always
///     empty.
/// </summary>
public sealed class TalkSegmentStore
{
    /// <summary>Rolling window the lanes and talk-share are computed over.</summary>
    public static readonly TimeSpan Window = TimeSpan.FromMinutes(3);

    /// <summary>Hold the current-speaker card this long after speech stops, so
    /// cross-talk doesn't flicker the hero card.</summary>
    public static readonly TimeSpan SpeakerHold = TimeSpan.FromMilliseconds(1200);

    /// <summary>Segments below this length are fringe; merge them into a neighbour.</summary>
    private static readonly TimeSpan MinSegment = TimeSpan.FromMilliseconds(400);

    /// <summary>...as long as the neighbour is no further away than this.</summary>
    private static readonly TimeSpan MergeGap = TimeSpan.FromMilliseconds(250);

    private readonly List<TalkSegment> _segments = new();
    private readonly Dictionary<uint, double> _talkShare = new();
    private readonly Func<DateTime> _clock;

    private uint? _lastSpeakerId;
    private DateTime _lastSpeechEnd = DateTime.MinValue;

    public TalkSegmentStore(Func<DateTime>? clock = null)
    {
        _clock = clock ?? (() => DateTime.UtcNow);
    }

    /// <summary>All retained segments, oldest first.</summary>
    public IReadOnlyList<TalkSegment> Segments => _segments;

    /// <summary>Talk-share per participant as a fraction of the whole window (0..1).</summary>
    public IReadOnlyDictionary<uint, double> TalkShare => _talkShare;

    /// <summary>
    /// Whoever has the floor: the live speaker, or the last one for 1200ms after
    /// they stop. Null once the hold lapses.
    /// </summary>
    public uint? CurrentSpeakerId
    {
        get
        {
            var open = _segments.LastOrDefault(s => s.IsOpen);
            if (open != null) return open.SessionId;
            if (_lastSpeakerId != null && _clock() - _lastSpeechEnd < SpeakerHold) return _lastSpeakerId;
            return null;
        }
    }

    /// <summary>
    /// How long the current speaker has held the floor, for `speaking 0:12`.
    ///
    /// Measured over the whole *run* — consecutive segments no more than 250ms
    /// apart — so a breath mid-sentence doesn't reset the counter to 0:00 before
    /// the trim tick folds the segments together.
    /// </summary>
    public TimeSpan CurrentSpeakerElapsed
    {
        get
        {
            var id = CurrentSpeakerId;
            if (id == null) return TimeSpan.Zero;

            var now = _clock();
            var own = _segments.Where(s => s.SessionId == id.Value).OrderBy(s => s.Start).ToList();
            if (own.Count == 0) return TimeSpan.Zero;

            var runStart = own[^1].Start;
            for (int i = own.Count - 1; i > 0; i--)
            {
                var previousEnd = own[i - 1].EndOr(now);
                if (own[i].Start - previousEnd > MergeGap) break;
                runStart = own[i - 1].Start;
            }

            return own[^1].EndOr(now) - runStart;
        }
    }

    public bool IsSpeaking(uint sessionId) => _segments.Any(s => s.SessionId == sessionId && s.IsOpen);

    /// <summary>Everyone with any retained speech, most talkative first.</summary>
    public IReadOnlyList<uint> SpeakersByShare() =>
        _talkShare.OrderByDescending(kv => kv.Value).Select(kv => kv.Key).ToList();

    /// <summary>
    /// Rising / falling edge for one participant. Repeat calls in the same state
    /// are no-ops, so this is safe to call every audio tick.
    /// </summary>
    public void SetSpeaking(uint sessionId, bool speaking)
    {
        var now = _clock();
        var open = _segments.LastOrDefault(s => s.SessionId == sessionId && s.IsOpen);

        if (speaking)
        {
            _lastSpeakerId = sessionId;
            if (open != null) return;

            // Always a new segment. Fringe merging happens on the 1s trim, which
            // is also when the lane is re-laid out, so the two stay in step.
            _segments.Add(new TalkSegment { SessionId = sessionId, Start = now });
        }
        else
        {
            if (open == null) return;
            open.End = now;
            _lastSpeakerId = sessionId;
            _lastSpeechEnd = now;
        }
    }

    /// <summary>
    /// Apply the jitter buffer's active-speaker set: everyone in it is talking,
    /// everyone previously talking and absent from it has stopped. The local
    /// session id is excluded because the remote signal can never contain it —
    /// the caller drives the local lane through <see cref="SetSpeaking"/>.
    /// </summary>
    public void ApplyRemoteSpeakers(IReadOnlyCollection<uint> speakers, uint? localSessionId)
    {
        foreach (var id in speakers)
        {
            if (id == localSessionId) continue;
            SetSpeaking(id, true);
        }

        var stopped = _segments
            .Where(s => s.IsOpen && s.SessionId != localSessionId && !speakers.Contains(s.SessionId))
            .Select(s => s.SessionId)
            .Distinct()
            .ToList();

        foreach (var id in stopped) SetSpeaking(id, false);
    }

    /// <summary>Drop the participant entirely (left the channel / disconnected).</summary>
    public void Forget(uint sessionId)
    {
        _segments.RemoveAll(s => s.SessionId == sessionId);
        _talkShare.Remove(sessionId);
        if (_lastSpeakerId == sessionId) _lastSpeakerId = null;
    }

    public void Clear()
    {
        _segments.Clear();
        _talkShare.Clear();
        _lastSpeakerId = null;
        _lastSpeechEnd = DateTime.MinValue;
    }

    /// <summary>
    /// Age out everything before the window, clip what straddles its edge, merge
    /// fringe segments, and recompute talk-share. Called on a 1s timer.
    /// </summary>
    public void Trim()
    {
        var now = _clock();
        var cutoff = now - Window;

        _segments.RemoveAll(s => s.End != null && s.End.Value <= cutoff);
        foreach (var segment in _segments)
        {
            if (segment.Start < cutoff) segment.Start = cutoff;
        }

        MergeFringeSegments(now);
        RecomputeTalkShare(now);
    }

    /// <summary>
    /// Segments shorter than 400ms are merged into a neighbour within 250ms so
    /// the lane doesn't fringe. A short segment with no neighbour that close is
    /// left alone — it's a real, isolated blip.
    /// </summary>
    private void MergeFringeSegments(DateTime now)
    {
        foreach (var sessionId in _segments.Select(s => s.SessionId).Distinct().ToList())
        {
            var own = _segments.Where(s => s.SessionId == sessionId).OrderBy(s => s.Start).ToList();

            for (int i = 0; i < own.Count; i++)
            {
                var segment = own[i];
                if (segment.IsOpen) continue;
                if (segment.Duration(now) >= MinSegment) continue;

                var previous = i > 0 ? own[i - 1] : null;
                var next = i + 1 < own.Count ? own[i + 1] : null;

                if (previous?.End != null && segment.Start - previous.End.Value <= MergeGap)
                {
                    previous.End = segment.End;
                    _segments.Remove(segment);
                    own.RemoveAt(i);
                    i--;
                }
                else if (next != null && segment.End != null && next.Start - segment.End.Value <= MergeGap)
                {
                    next.Start = segment.Start;
                    _segments.Remove(segment);
                    own.RemoveAt(i);
                    i--;
                }
            }
        }
    }

    private void RecomputeTalkShare(DateTime now)
    {
        _talkShare.Clear();
        var windowSeconds = Window.TotalSeconds;

        foreach (var segment in _segments)
        {
            var seconds = segment.Duration(now).TotalSeconds;
            if (seconds <= 0) continue;
            _talkShare.TryGetValue(segment.SessionId, out var running);
            _talkShare[segment.SessionId] = running + seconds / windowSeconds;
        }
    }

    /// <summary>
    /// One participant's lane as window-relative fractions. The open segment is
    /// the live one and is drawn at full accent.
    /// </summary>
    public List<LaneSlice> LaneFor(uint sessionId)
    {
        var now = _clock();
        var origin = now - Window;
        var slices = new List<LaneSlice>();

        foreach (var segment in _segments.Where(s => s.SessionId == sessionId).OrderBy(s => s.Start))
        {
            var slice = ToSlice(segment, origin, now, segment.IsOpen);
            if (slice.Width > 0) slices.Add(slice);
        }

        return slices;
    }

    /// <summary>
    /// The "talked over" signal: intersections between this participant's speech
    /// and anyone else's, attributed to whoever started later — so the marker
    /// lands on the lane of the person who talked over someone else.
    /// </summary>
    public List<LaneSlice> OverlapsFor(uint sessionId)
    {
        var now = _clock();
        var origin = now - Window;
        var overlaps = new List<LaneSlice>();

        var own = _segments.Where(s => s.SessionId == sessionId).ToList();
        var others = _segments.Where(s => s.SessionId != sessionId).ToList();

        foreach (var mine in own)
        {
            foreach (var theirs in others)
            {
                // Attribute to the later starter only; the earlier speaker was
                // the one talked over.
                if (mine.Start < theirs.Start) continue;

                var start = mine.Start > theirs.Start ? mine.Start : theirs.Start;
                var myEnd = mine.EndOr(now);
                var theirEnd = theirs.EndOr(now);
                var end = myEnd < theirEnd ? myEnd : theirEnd;
                if (end <= start) continue;

                var slice = new LaneSlice(Fraction(start, origin), Fraction(end, origin), false);
                if (slice.Width > 0) overlaps.Add(slice);
            }
        }

        return overlaps;
    }

    private static LaneSlice ToSlice(TalkSegment segment, DateTime origin, DateTime now, bool isLive) =>
        new(Fraction(segment.Start, origin), Fraction(segment.EndOr(now), origin), isLive);

    private static double Fraction(DateTime instant, DateTime origin)
    {
        var fraction = (instant - origin).TotalSeconds / Window.TotalSeconds;
        return Math.Clamp(fraction, 0, 1);
    }
}
