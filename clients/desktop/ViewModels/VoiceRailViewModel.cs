using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Aura.Desktop.Services;
using Avalonia;
using Avalonia.Media;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>One channel occupant as the voice rail needs them.</summary>
public sealed record VoiceParticipant(
    uint SessionId,
    string Name,
    bool IsMuted,
    bool IsDeafened,
    TrustState Trust,
    bool IsLocal);

/// <summary>
/// The voice rail (§3) / stage (§4): who has the floor now, who has been
/// talking, and who got talked over. Projected from <see cref="TalkSegmentStore"/>
/// once a second; the speaker card and meters update faster off the UI tick.
/// </summary>
public partial class VoiceRailViewModel : ObservableObject
{
    /// <summary>Lanes shown before collapsing into "N others" (decided: top 5).</summary>
    public const int LaneLimit = 5;

    /// <summary>16 bars in the rail, 26 on the stage.</summary>
    public const int WaveformBars = 16;
    public const int StageWaveformBars = 26;

    /// <summary>A lane below this share renders at 60% opacity — barely spoke.</summary>
    private const double FaintLaneShare = 0.02;

    private readonly double[] _waveform = new double[StageWaveformBars];

    /// <summary>
    /// Reads a member's persisted local playback gain (1.0 = unchanged). Set by
    /// the owning view model so the rail stays free of a network-client
    /// reference.
    /// </summary>
    public Func<uint, float>? LocalVolumeProvider { get; set; }

    /// <summary>Applies and persists a member's local playback gain.</summary>
    public Action<uint, float>? LocalVolumeSetter { get; set; }

    [ObservableProperty]
    private SpeakerCardViewModel? _currentSpeaker;

    [ObservableProperty]
    private ObservableCollection<TalkLaneViewModel> _lanes = new();

    [ObservableProperty]
    private ObservableCollection<SilentMemberViewModel> _silentMembers = new();

    /// <summary>Set when the channel has more speakers than <see cref="LaneLimit"/>.</summary>
    [ObservableProperty]
    private bool _hasOverflow;

    /// <summary>`12 others` — click to expand.</summary>
    [ObservableProperty]
    private string _overflowLabel = "";

    [ObservableProperty]
    private bool _isOverflowExpanded;

    /// <summary>No one has spoken in the window: show the empty chip, never a spinner.</summary>
    [ObservableProperty]
    private bool _isEmpty = true;

    /// <summary>
    /// The empty chip fills the hero slot whenever nobody else has the floor.
    ///
    /// Keyed only on the speaker card, not on whether lanes exist: talking to
    /// an empty room gives us a lane of our own but still no hero, and leaving
    /// a hole there looked like a rendering bug.
    /// </summary>
    public bool ShowEmptyState => CurrentSpeaker == null;

    /// <summary>
    /// Three readings. "No one is speaking" is plainly wrong while you are
    /// mid-sentence — the hero just does not put you in it — and both phrasings
    /// are wrong when there is nobody else in the room to speak.
    /// </summary>
    public string EmptyStateText =>
        IsAlone ? "You're the only one here"
        : IsOnlyLocalSpeaking ? "No one else is speaking"
        : "No one is speaking yet";

    [ObservableProperty] private bool _isOnlyLocalSpeaking;

    partial void OnIsOnlyLocalSpeakingChanged(bool value) => OnPropertyChanged(nameof(EmptyStateText));

    /// <summary>
    /// Nobody else is in the channel.
    ///
    /// Everything the rail measures is comparative — talk-share against other
    /// people, overlap with them, round-trip to them — so alone it renders a
    /// wall of statistics about an empty room: a lane showing you at 61% of
    /// nothing, a legend for overlaps that cannot happen, a latency reading to
    /// nobody. The HUD stays, because meters and mute still mean something.
    /// </summary>
    [ObservableProperty] private bool _isAlone;

    partial void OnIsAloneChanged(bool value)
    {
        OnPropertyChanged(nameof(EmptyStateText));
        OnPropertyChanged(nameof(ShowLanes));
        OnPropertyChanged(nameof(ShowLatency));
    }

    /// <summary>Lanes are a comparison; with nobody to compare against, hide them.</summary>
    public bool ShowLanes => !IsAlone && !IsEmpty;

    /// <summary>Round-trip to nobody is not a measurement.</summary>
    public bool ShowLatency => !IsAlone;

    /// <summary>
    /// A "SILENT" heading over a list containing only yourself is a list of one
    /// obvious fact.
    /// </summary>
    public bool ShowSilent => !IsAlone && HasSilentMembers;

    partial void OnIsEmptyChanged(bool value)
    {
        OnPropertyChanged(nameof(ShowEmptyState));
        OnPropertyChanged(nameof(ShowLanes));
    }

    partial void OnCurrentSpeakerChanged(SpeakerCardViewModel? value) => OnPropertyChanged(nameof(ShowEmptyState));

    [ObservableProperty]
    private bool _hasSilentMembers;

    /// <summary>Newest-last level history for the speaker card waveform.</summary>
    [ObservableProperty]
    private IReadOnlyList<double> _waveformLevels = Array.Empty<double>();

    /// <summary>Input meter level, 0..1.</summary>
    [ObservableProperty]
    private double _inputLevel;

    /// <summary>`−18 dB`, or `−∞ dB` on silence.</summary>
    [ObservableProperty]
    private string _inputDbLabel = "−∞ dB";

    [RelayCommand]
    private void ToggleOverflow() => IsOverflowExpanded = !IsOverflowExpanded;

    /// <summary>
    /// Push one capture level sample (0..1 linear RMS) into the waveform history
    /// and the input meter.
    /// </summary>
    public void PushLevel(double level)
    {
        Array.Copy(_waveform, 1, _waveform, 0, _waveform.Length - 1);
        _waveform[^1] = Math.Clamp(level, 0, 1);

        // A new list instance each tick: the bar meter is a render-only control
        // and only repaints when the property identity changes.
        WaveformLevels = _waveform.ToArray();
        InputLevel = _waveform[^1];
        InputDbLabel = FormatDb(_waveform[^1]);
    }

    /// <summary>dBFS, rounded, with the handoff's `−18 dB` shape.</summary>
    private static string FormatDb(double linear)
    {
        if (linear <= 0.0001) return "−∞ dB";
        var db = 20 * Math.Log10(linear);
        return db >= 0 ? "0 dB" : $"−{Math.Abs(db):0} dB";
    }

    /// <summary>
    /// Re-project the store onto the rail. Called on the 1s trim tick and
    /// whenever the participant set or speaking state changes.
    /// </summary>
    public void Rebuild(IReadOnlyList<VoiceParticipant> participants, TalkSegmentStore store)
    {
        var byId = participants.ToDictionary(p => p.SessionId);
        var share = store.TalkShare;

        IsAlone = !participants.Any(p => !p.IsLocal);

        // Lanes: everyone who spoke in the window, most talkative first, capped
        // at the top 5 with the rest behind an "N others" row.
        var ranked = share
            .Where(kv => byId.ContainsKey(kv.Key))
            .OrderByDescending(kv => kv.Value)
            .Select(kv => kv.Key)
            .ToList();

        var hidden = Math.Max(0, ranked.Count - LaneLimit);
        HasOverflow = hidden > 0;
        OverflowLabel = hidden == 1 ? "1 other" : $"{hidden} others";
        if (!HasOverflow) IsOverflowExpanded = false;

        var visible = HasOverflow && !IsOverflowExpanded ? ranked.Take(LaneLimit).ToList() : ranked;

        SyncLanes(visible, byId, store, share);

        // Silent list: everyone with no speech in the window.
        var silent = participants
            .Where(p => !share.ContainsKey(p.SessionId))
            .OrderBy(p => p.Name, StringComparer.OrdinalIgnoreCase)
            .ToList();

        SyncSilent(silent);

        IsEmpty = Lanes.Count == 0;
        HasSilentMembers = SilentMembers.Count > 0;

        UpdateSpeakerCard(byId, store, share);
    }

    /// <summary>
    /// Refresh only the speaker card. Called on the fast UI tick so `speaking
    /// 0:12` counts up and the ring reacts on the rising edge, without paying
    /// for a full lane re-layout.
    /// </summary>
    public void RefreshSpeaker(IReadOnlyList<VoiceParticipant> participants, TalkSegmentStore store)
    {
        UpdateSpeakerCard(participants.ToDictionary(p => p.SessionId), store, store.TalkShare);
    }

    private void UpdateSpeakerCard(
        IReadOnlyDictionary<uint, VoiceParticipant> byId,
        TalkSegmentStore store,
        IReadOnlyDictionary<uint, double> share)
    {
        IsOnlyLocalSpeaking = store.IsOnlyLocalSpeaking;

        var speakerId = store.CurrentSpeakerId;
        if (speakerId == null || !byId.TryGetValue(speakerId.Value, out var participant))
        {
            CurrentSpeaker = null;
            return;
        }

        share.TryGetValue(participant.SessionId, out var speakerShare);
        var elapsed = store.CurrentSpeakerElapsed;
        var isLive = store.IsSpeaking(participant.SessionId);

        if (CurrentSpeaker?.SessionId != participant.SessionId)
        {
            CurrentSpeaker = new SpeakerCardViewModel(
                participant, LocalVolumeProvider, LocalVolumeSetter);
        }

        CurrentSpeaker!.Update(participant, elapsed, speakerShare, isLive);
    }

    private void SyncLanes(
        IReadOnlyList<uint> order,
        IReadOnlyDictionary<uint, VoiceParticipant> byId,
        TalkSegmentStore store,
        IReadOnlyDictionary<uint, double> share)
    {
        for (int i = 0; i < order.Count; i++)
        {
            var sessionId = order[i];
            var existingIndex = IndexOfLane(sessionId);

            if (existingIndex < 0)
            {
                Lanes.Insert(i, new TalkLaneViewModel(byId[sessionId], i));
            }
            else if (existingIndex != i)
            {
                Lanes.Move(existingIndex, i);
            }

            share.TryGetValue(sessionId, out var laneShare);
            Lanes[i].Update(byId[sessionId], i, laneShare, store.LaneFor(sessionId), store.OverlapsFor(sessionId));
        }

        while (Lanes.Count > order.Count) Lanes.RemoveAt(Lanes.Count - 1);
    }

    private int IndexOfLane(uint sessionId)
    {
        for (int i = 0; i < Lanes.Count; i++)
        {
            if (Lanes[i].SessionId == sessionId) return i;
        }
        return -1;
    }

    private void SyncSilent(IReadOnlyList<VoiceParticipant> silent)
    {
        for (int i = 0; i < silent.Count; i++)
        {
            var existingIndex = -1;
            for (int j = 0; j < SilentMembers.Count; j++)
            {
                if (SilentMembers[j].SessionId == silent[i].SessionId) { existingIndex = j; break; }
            }

            if (existingIndex < 0)
            {
                SilentMembers.Insert(i, new SilentMemberViewModel(silent[i]));
            }
            else if (existingIndex != i)
            {
                SilentMembers.Move(existingIndex, i);
            }

            SilentMembers[i].Update(silent[i]);
        }

        while (SilentMembers.Count > silent.Count) SilentMembers.RemoveAt(SilentMembers.Count - 1);
    }

    /// <summary>Faint lane test, exposed for the lane view model.</summary>
    internal static bool IsFaint(double share) => share > 0 && share < FaintLaneShare;
}

/// <summary>The current speaker card: pulsing ring, waveform, local volume.</summary>
public partial class SpeakerCardViewModel : ObservableObject
{
    /// <summary>Pushes a gain change into the live mixer and persistence.</summary>
    private readonly Action<uint, float>? _setGain;

    /// <summary>Set while seeding the slider, so echoing it back is not treated as a user edit.</summary>
    private bool _suppressGainWrite;

    public SpeakerCardViewModel(
        VoiceParticipant participant,
        Func<uint, float>? getGain = null,
        Action<uint, float>? setGain = null)
    {
        SessionId = participant.SessionId;
        _setGain = setGain;

        // Gain is a 0.0–4.0 multiplier; the slider is 0–100 % of unity.
        _suppressGainWrite = true;
        _localVolume = (int)Math.Round((getGain?.Invoke(participant.SessionId) ?? 1.0f) * 100f);
        _suppressGainWrite = false;

        Update(participant, TimeSpan.Zero, 0, false);
    }

    public uint SessionId { get; }

    /// <summary>
    /// This speaker's local playback gain as a percentage. Local only — it never
    /// reaches the server and affects nobody else's mix. Distinct from master
    /// output volume, which is a separate control.
    /// </summary>
    [ObservableProperty]
    private int _localVolume = 100;

    partial void OnLocalVolumeChanged(int value)
    {
        if (_suppressGainWrite) return;
        _setGain?.Invoke(SessionId, value / 100f);
    }

    /// <summary>
    /// You never hear yourself, so per-sender gain is meaningless for the local
    /// user and the row is hidden rather than shown doing nothing.
    /// </summary>
    public bool CanAdjustVolume => !IsLocal;

    [ObservableProperty]
    private string _name = "";

    [ObservableProperty]
    private string _initial = "?";

    /// <summary>`speaking 0:12 · 41%` — mono, machine truth.</summary>
    [ObservableProperty]
    private string _subLine = "";

    [ObservableProperty]
    private bool _isVerified;

    /// <summary>The ring only pulses while speech is live, not during the 1200ms hold.</summary>
    [ObservableProperty]
    private bool _isLive;

    [ObservableProperty]
    private IBrush? _avatarBrush;

    public void Update(VoiceParticipant participant, TimeSpan elapsed, double share, bool isLive)
    {
        Name = participant.Name;
        Initial = AuraBrushes.Initial(participant.Name);
        IsVerified = participant.Trust == TrustState.Verified;
        IsLive = isLive;
        IsLocal = participant.IsLocal;
        AvatarBrush = AuraBrushes.ActiveBrush();
        SubLine = $"speaking {elapsed.Minutes}:{elapsed.Seconds:00} · {share * 100:0}%";
    }

    [ObservableProperty]
    private bool _isLocal;

    partial void OnIsLocalChanged(bool value) => OnPropertyChanged(nameof(CanAdjustVolume));
}

/// <summary>One row of the "LAST 3 MIN" panel.</summary>
public partial class TalkLaneViewModel : ObservableObject
{
    public TalkLaneViewModel(VoiceParticipant participant, int slot)
    {
        SessionId = participant.SessionId;
        Update(participant, slot, 0, Array.Empty<LaneSlice>(), Array.Empty<LaneSlice>());
    }

    public uint SessionId { get; }

    [ObservableProperty]
    private string _name = "";

    [ObservableProperty]
    private string _initial = "?";

    /// <summary>`41%`, mono, right-aligned in a 26px column.</summary>
    [ObservableProperty]
    private string _shareLabel = "0%";

    [ObservableProperty]
    private IReadOnlyList<LaneSlice> _segments = Array.Empty<LaneSlice>();

    /// <summary>Intersections attributed to this speaker — they talked over someone.</summary>
    [ObservableProperty]
    private IReadOnlyList<LaneSlice> _overlaps = Array.Empty<LaneSlice>();

    /// <summary>Segment color by lane slot: accent, then secondary, then primary.</summary>
    [ObservableProperty]
    private IBrush? _segmentBrush;

    [ObservableProperty]
    private IBrush? _avatarBrush;

    /// <summary>0.6 for a lane that barely spoke (handoff §4).</summary>
    [ObservableProperty]
    private double _laneOpacity = 1;

    public void Update(
        VoiceParticipant participant,
        int slot,
        double share,
        IReadOnlyList<LaneSlice> segments,
        IReadOnlyList<LaneSlice> overlaps)
    {
        Name = participant.IsLocal ? "you" : participant.Name;
        Initial = AuraBrushes.Initial(participant.Name);
        ShareLabel = $"{share * 100:0}%";
        Segments = segments;
        Overlaps = overlaps;
        SegmentBrush = AuraBrushes.SegmentBrush(slot);
        AvatarBrush = AuraBrushes.ActiveBrush();
        LaneOpacity = VoiceRailViewModel.IsFaint(share) ? 0.6 : 1;
    }
}

/// <summary>One row of the SILENT list.</summary>
public partial class SilentMemberViewModel : ObservableObject
{
    public SilentMemberViewModel(VoiceParticipant participant)
    {
        SessionId = participant.SessionId;
        Update(participant);
    }

    public uint SessionId { get; }

    [ObservableProperty]
    private string _name = "";

    [ObservableProperty]
    private string _initial = "?";

    /// <summary>`muted` in warn, `verify key` / `key changed` in caution.</summary>
    [ObservableProperty]
    private string _stateLabel = "";

    [ObservableProperty]
    private bool _isMuted;

    /// <summary>Trust rows get the caution fill + border and open the trust sheet.</summary>
    [ObservableProperty]
    private bool _needsTrust;

    [ObservableProperty]
    private IBrush? _avatarBrush;

    public void Update(VoiceParticipant participant)
    {
        Name = participant.IsLocal ? "you" : participant.Name;
        Initial = AuraBrushes.Initial(participant.Name);
        IsMuted = participant.IsMuted || participant.IsDeafened;

        // Mute state is the louder signal; an unchecked key is what's left.
        if (participant.IsDeafened)
        {
            StateLabel = "deafened";
            NeedsTrust = false;
        }
        else if (participant.IsMuted)
        {
            StateLabel = "muted";
            NeedsTrust = false;
        }
        else if (!participant.IsLocal && participant.Trust != TrustState.Verified)
        {
            StateLabel = participant.Trust == TrustState.KeyChanged ? "key changed" : "verify key";
            NeedsTrust = true;
        }
        else
        {
            StateLabel = "";
            NeedsTrust = false;
        }

        AvatarBrush = AuraBrushes.IdleBrush();
    }
}

/// <summary>
/// Avatar and lane brushes pulled from the active theme dictionary. Resolved per
/// rebuild rather than cached, so a palette swap lands within one tick.
/// </summary>
internal static class AuraBrushes
{
    private static readonly string[] SegmentKeys = { "AuraSegment1Brush", "AuraSegment2Brush", "AuraSegment3Brush" };

    public static string Initial(string name) =>
        string.IsNullOrWhiteSpace(name) ? "?" : name.Trim()[..1].ToUpperInvariant();

    public static IBrush? ActiveBrush() => Resource("AuraBrandGradientBrush");

    public static IBrush? IdleBrush() => Resource("AuraWellBrush");

    public static IBrush? SegmentBrush(int slot) => Resource(SegmentKeys[Math.Abs(slot) % SegmentKeys.Length]);

    /// <summary>Any token brush by key, for state colors the view model owns.</summary>
    public static IBrush? Named(string key) => Resource(key);

    private static IBrush? Resource(string key)
    {
        var app = Application.Current;
        if (app == null) return null;
        return app.Resources.TryGetResource(key, app.ActualThemeVariant, out var value) ? value as IBrush : null;
    }
}
