using System;
using System.Linq;
using Aura.Desktop.Services;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// The rolling talk history behind the voice rail. Every rule here is from the
/// design handoff's "Talk lanes" and "Speaking state" sections: 3 minute window,
/// 1200ms speaker hold, 400ms fringe merge within 250ms, overlap attributed to
/// the later speaker.
/// </summary>
public class TalkSegmentStoreTests
{
    /// <summary>Manual clock so the timing rules can be exercised without waiting.</summary>
    private sealed class FakeClock
    {
        public DateTime Now = new(2026, 1, 1, 12, 0, 0, DateTimeKind.Utc);
        public DateTime Get() => Now;
        public void Advance(int ms) => Now = Now.AddMilliseconds(ms);
    }

    private static (TalkSegmentStore store, FakeClock clock) NewStore()
    {
        var clock = new FakeClock();
        return (new TalkSegmentStore(clock.Get), clock);
    }

    [Fact]
    public void CurrentSpeaker_HoldsFor1200MsAfterSpeechStops()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(2000);
        store.SetSpeaking(1, false);

        clock.Advance(1100);
        Assert.Equal(1u, store.CurrentSpeakerId);   // still held

        clock.Advance(200);                          // 1300ms total
        Assert.Null(store.CurrentSpeakerId);         // hold lapsed
    }

    [Fact]
    public void CurrentSpeaker_NeverPromotesTheLocalUser()
    {
        // The hero answers "who has the floor", which is only a question about
        // other people. Watching yourself talk beside the HUD meter that
        // already says you are live reads as a mirror, not information.
        var (store, clock) = NewStore();
        store.LocalSessionId = 1;

        store.SetSpeaking(1, true);
        clock.Advance(2000);

        Assert.Null(store.CurrentSpeakerId);
        Assert.True(store.IsOnlyLocalSpeaking);
    }

    [Fact]
    public void CurrentSpeaker_RemoteSpeakerStillTakesTheFloorOverLocal()
    {
        // Excluding ourselves must not exclude whoever talks over us.
        var (store, clock) = NewStore();
        store.LocalSessionId = 1;

        store.SetSpeaking(1, true);
        clock.Advance(500);
        store.SetSpeaking(2, true);
        clock.Advance(500);

        Assert.Equal(2u, store.CurrentSpeakerId);
        Assert.False(store.IsOnlyLocalSpeaking);
    }

    [Fact]
    public void IsOnlyLocalSpeaking_FalseWhenSilent()
    {
        var (store, clock) = NewStore();
        store.LocalSessionId = 1;

        Assert.False(store.IsOnlyLocalSpeaking);

        store.SetSpeaking(1, true);
        clock.Advance(500);
        store.SetSpeaking(1, false);

        // Stopped talking — back to "no one is speaking yet", not "no one else".
        Assert.False(store.IsOnlyLocalSpeaking);
    }

    [Fact]
    public void CurrentSpeaker_LiveSpeakerWinsOverHeldOne()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(500);
        store.SetSpeaking(1, false);

        clock.Advance(100);
        store.SetSpeaking(2, true);

        Assert.Equal(2u, store.CurrentSpeakerId);
    }

    [Fact]
    public void Trim_MergesAFringeSegmentIntoTheNeighbourWithin250Ms()
    {
        var (store, clock) = NewStore();

        // A real turn, a 200ms pause, then a 300ms blip: the blip is under 400ms
        // and its neighbour is within 250ms, so the lane must not fringe.
        store.SetSpeaking(1, true);
        clock.Advance(1000);
        store.SetSpeaking(1, false);

        clock.Advance(200);
        store.SetSpeaking(1, true);
        clock.Advance(300);
        store.SetSpeaking(1, false);

        Assert.Equal(2, store.Segments.Count);
        store.Trim();

        var merged = Assert.Single(store.Segments);
        Assert.Equal(1500, (merged.End!.Value - merged.Start).TotalMilliseconds, 1);
    }

    [Fact]
    public void Trim_LeavesFullLengthTurnsAlone()
    {
        var (store, clock) = NewStore();

        // Both turns are over 400ms, so a short pause between them is real
        // silence and stays visible in the lane.
        store.SetSpeaking(1, true);
        clock.Advance(1000);
        store.SetSpeaking(1, false);
        clock.Advance(200);
        store.SetSpeaking(1, true);
        clock.Advance(1000);
        store.SetSpeaking(1, false);

        store.Trim();

        Assert.Equal(2, store.Segments.Count);
    }

    [Fact]
    public void Trim_LeavesAnIsolatedBlipAlone()
    {
        var (store, clock) = NewStore();

        // Under 400ms but with no neighbour inside 250ms: a real, isolated blip.
        store.SetSpeaking(1, true);
        clock.Advance(200);
        store.SetSpeaking(1, false);
        clock.Advance(5000);
        store.SetSpeaking(1, true);
        clock.Advance(1000);
        store.SetSpeaking(1, false);

        store.Trim();

        Assert.Equal(2, store.Segments.Count);
    }

    [Fact]
    public void CurrentSpeakerElapsed_SpansABreathInsteadOfRestarting()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(4000);
        store.SetSpeaking(1, false);
        clock.Advance(200);            // a breath, not a new turn
        store.SetSpeaking(1, true);
        clock.Advance(1000);

        Assert.Equal(5200, store.CurrentSpeakerElapsed.TotalMilliseconds, 1);
    }

    [Fact]
    public void Trim_DropsSpeechOlderThanTheWindowAndClipsWhatStraddlesIt()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(1000);
        store.SetSpeaking(1, false);

        // Push that turn out of the 3 minute window.
        clock.Advance((int)TalkSegmentStore.Window.TotalMilliseconds + 1000);
        store.SetSpeaking(2, true);
        clock.Advance(1000);
        store.SetSpeaking(2, false);

        store.Trim();

        Assert.Single(store.Segments);
        Assert.Equal(2u, store.Segments[0].SessionId);
        Assert.False(store.TalkShare.ContainsKey(1));
    }

    [Fact]
    public void TalkShare_IsTheFractionOfTheWholeWindow()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(18_000);          // 18s of a 180s window == 10%
        store.SetSpeaking(1, false);
        store.Trim();

        Assert.Equal(0.10, store.TalkShare[1], 3);
    }

    [Fact]
    public void Overlap_IsAttributedToTheLaterSpeaker()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);     // 1 has the floor
        clock.Advance(1000);
        store.SetSpeaking(2, true);     // 2 talks over 1
        clock.Advance(1000);
        store.SetSpeaking(1, false);
        clock.Advance(1000);
        store.SetSpeaking(2, false);
        store.Trim();

        // The marker lands on the interrupter's lane, not the interrupted one.
        Assert.NotEmpty(store.OverlapsFor(2));
        Assert.Empty(store.OverlapsFor(1));
    }

    [Fact]
    public void Lane_IsWindowRelativeWithTheLiveSegmentAtTheRightEdge()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(1000);

        var lane = store.LaneFor(1);

        var live = Assert.Single(lane);
        Assert.True(live.IsLive);
        Assert.Equal(1.0, live.End, 3);              // ends at "now"
        Assert.InRange(live.Start, 0.99, 1.0);       // 1s of a 3 minute window
    }

    [Fact]
    public void ApplyRemoteSpeakers_ClosesSegmentsForAnyoneNoLongerInTheSet()
    {
        var (store, clock) = NewStore();

        store.ApplyRemoteSpeakers(new[] { 1u, 2u }, localSessionId: 9);
        clock.Advance(1000);
        store.ApplyRemoteSpeakers(new[] { 1u }, localSessionId: 9);

        Assert.True(store.IsSpeaking(1));
        Assert.False(store.IsSpeaking(2));
    }

    [Fact]
    public void ApplyRemoteSpeakers_NeverTouchesTheLocalLane()
    {
        var (store, clock) = NewStore();

        // The local user is talking, driven by the capture path.
        store.SetSpeaking(9, true);
        clock.Advance(500);

        // A remote set that (wrongly) contains us must not close our segment,
        // and must not open one either.
        store.ApplyRemoteSpeakers(new[] { 9u }, localSessionId: 9);
        Assert.True(store.IsSpeaking(9));

        store.ApplyRemoteSpeakers(Array.Empty<uint>(), localSessionId: 9);
        Assert.True(store.IsSpeaking(9));
    }

    [Fact]
    public void SpeakersByShare_RanksTheTalkativeFirstSoTheTop5CutIsStable()
    {
        var (store, clock) = NewStore();

        void Talk(uint id, int ms)
        {
            store.SetSpeaking(id, true);
            clock.Advance(ms);
            store.SetSpeaking(id, false);
            clock.Advance(300);
        }

        Talk(1, 1000);
        Talk(2, 5000);
        Talk(3, 3000);
        store.Trim();

        Assert.Equal(new[] { 2u, 3u, 1u }, store.SpeakersByShare().ToArray());
    }

    [Fact]
    public void Forget_RemovesAParticipantWhoLeft()
    {
        var (store, clock) = NewStore();

        store.SetSpeaking(1, true);
        clock.Advance(1000);
        store.SetSpeaking(1, false);
        store.Trim();

        store.Forget(1);

        Assert.Empty(store.Segments);
        Assert.False(store.TalkShare.ContainsKey(1));
        Assert.Null(store.CurrentSpeakerId);
    }
}
