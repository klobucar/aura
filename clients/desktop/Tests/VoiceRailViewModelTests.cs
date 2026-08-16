using System;
using System.Collections.Generic;
using Aura.Desktop.Services;
using Aura.Desktop.ViewModels;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// What the rail shows when there is nobody to compare against.
///
/// Everything the rail measures is comparative — talk-share against other
/// people, overlap with them, round-trip to them. Alone, those become
/// statistics about an empty room, which is what the first-run screen used to
/// be: a lane reading "61%" of nothing under a legend for overlaps that cannot
/// happen.
/// </summary>
public class VoiceRailViewModelTests
{
    private sealed class FakeClock
    {
        public DateTime Now = new(2026, 1, 1, 12, 0, 0, DateTimeKind.Utc);
        public DateTime Get() => Now;
        public void Advance(int ms) => Now = Now.AddMilliseconds(ms);
    }

    private static VoiceParticipant Person(uint id, string name, bool isLocal = false) =>
        new(id, name, IsMuted: false, IsDeafened: false, Trust: TrustState.Tofu, IsLocal: isLocal);

    [Fact]
    public void Alone_HidesLanesLatencyAndSilentList()
    {
        var rail = new VoiceRailViewModel();
        var clock = new FakeClock();
        var store = new TalkSegmentStore(clock.Get) { LocalSessionId = 1 };

        rail.Rebuild(new List<VoiceParticipant> { Person(1, "You", isLocal: true) }, store);

        Assert.True(rail.IsAlone);
        Assert.False(rail.ShowLanes);
        Assert.False(rail.ShowLatency);
        Assert.False(rail.ShowSilent);
    }

    [Fact]
    public void Alone_SaysSoRatherThanReportingSilence()
    {
        var rail = new VoiceRailViewModel();
        var clock = new FakeClock();
        var store = new TalkSegmentStore(clock.Get) { LocalSessionId = 1 };

        rail.Rebuild(new List<VoiceParticipant> { Person(1, "You", isLocal: true) }, store);

        Assert.True(rail.ShowEmptyState);
        Assert.Equal("You're the only one here", rail.EmptyStateText);
    }

    [Fact]
    public void Alone_StaysTrueWhileYouTalkToAnEmptyRoom()
    {
        // The case in the screenshot that started this: talking alone used to
        // promote you to the hero and draw a lane about yourself.
        var rail = new VoiceRailViewModel();
        var clock = new FakeClock();
        var store = new TalkSegmentStore(clock.Get) { LocalSessionId = 1 };

        store.SetSpeaking(1, true);
        clock.Advance(2000);
        store.Trim();

        rail.Rebuild(new List<VoiceParticipant> { Person(1, "You", isLocal: true) }, store);

        Assert.True(rail.IsAlone);
        Assert.Null(rail.CurrentSpeaker);
        Assert.False(rail.ShowLanes);
        Assert.Equal("You're the only one here", rail.EmptyStateText);
    }

    [Fact]
    public void SomeoneElsePresent_RestoresTheComparativeUi()
    {
        var rail = new VoiceRailViewModel();
        var clock = new FakeClock();
        var store = new TalkSegmentStore(clock.Get) { LocalSessionId = 1 };

        store.SetSpeaking(2, true);
        clock.Advance(2000);
        store.Trim();

        rail.Rebuild(
            new List<VoiceParticipant> { Person(1, "You", isLocal: true), Person(2, "Mira") },
            store);

        Assert.False(rail.IsAlone);
        Assert.True(rail.ShowLatency);
        Assert.True(rail.ShowLanes);
    }

    [Fact]
    public void OthersPresentButSilent_KeepsTheOriginalCopy()
    {
        var rail = new VoiceRailViewModel();
        var clock = new FakeClock();
        var store = new TalkSegmentStore(clock.Get) { LocalSessionId = 1 };

        rail.Rebuild(
            new List<VoiceParticipant> { Person(1, "You", isLocal: true), Person(2, "Mira") },
            store);

        Assert.False(rail.IsAlone);
        Assert.Equal("No one is speaking yet", rail.EmptyStateText);
    }
}
