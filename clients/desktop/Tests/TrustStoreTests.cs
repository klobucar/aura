using System;
using Aura.Desktop.Services;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// Trust bookkeeping behind the trust sheet. The rules come from the design
/// handoff's §7 ("three states only: verified / TOFU · not compared / key
/// changed today") and its state table, where verifiedKeys is described as the
/// thing that "drives every trust badge".
///
/// The property worth defending here is that trust attaches to a <em>key</em>,
/// not to a name — otherwise a takeover inherits the trust of whoever held the
/// name before.
/// </summary>
public class TrustStoreTests
{
    private sealed class FakeClock
    {
        public DateTimeOffset Now = new(2026, 1, 1, 12, 0, 0, TimeSpan.Zero);
        public DateTimeOffset Get() => Now;
        public void AdvanceDays(int d) => Now = Now.AddDays(d);
    }

    private const string Alice = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    private const string Bob = "6ba7b811-9dad-11d1-80b4-00c04fd430c8";

    private static byte[] Key(byte fill) => System.Linq.Enumerable.ToArray(
        System.Linq.Enumerable.Repeat(fill, 32));

    private static (TrustStore store, AppSettings settings, FakeClock clock, Func<int> saves) New()
    {
        var settings = new AppSettings();
        var clock = new FakeClock();
        var saveCount = 0;
        var store = new TrustStore(settings, () => saveCount++, clock.Get);
        return (store, settings, clock, () => saveCount);
    }

    [Fact]
    public void UnknownKey_StartsAsTofu()
    {
        var (store, _, _, _) = New();
        Assert.Equal(TrustState.Tofu, store.Evaluate(Alice, Key(0x11)));
    }

    [Fact]
    public void ObservedThenVerified_BecomesVerified()
    {
        var (store, _, _, _) = New();
        var key = Key(0x11);

        store.Observe(Alice, key);
        Assert.Equal(TrustState.Tofu, store.Evaluate(Alice, key));

        store.MarkVerified(Alice, key);
        Assert.Equal(TrustState.Verified, store.Evaluate(Alice, key));
    }

    [Fact]
    public void NewKeyForKnownUser_ReportsKeyChanged()
    {
        var (store, _, _, _) = New();

        store.Observe(Alice, Key(0x11));
        store.Observe(Alice, Key(0x22));   // reinstall, or a takeover

        Assert.Equal(TrustState.KeyChanged, store.Evaluate(Alice, Key(0x22)));
    }

    [Fact]
    public void VerifyingOneKeyDoesNotVerifyAnother()
    {
        // The whole point of keying on the key: trust must not transfer to a
        // different key just because the same person presents it.
        var (store, _, _, _) = New();

        store.Observe(Alice, Key(0x11));
        store.MarkVerified(Alice, Key(0x11));

        Assert.Equal(TrustState.Verified, store.Evaluate(Alice, Key(0x11)));
        Assert.NotEqual(TrustState.Verified, store.Evaluate(Alice, Key(0x22)));
    }

    [Fact]
    public void TrustDoesNotTransferBetweenUsersSharingAName()
    {
        // Bob has never been seen; presenting Alice's verified key does make
        // that key verified (trust is per key), but Bob's own key must not be.
        var (store, _, _, _) = New();

        store.Observe(Alice, Key(0x11));
        store.MarkVerified(Alice, Key(0x11));

        Assert.Equal(TrustState.Tofu, store.Evaluate(Bob, Key(0x99)));
    }

    [Fact]
    public void VerifyingTheNewKeyClearsKeyChanged()
    {
        var (store, _, _, _) = New();

        store.Observe(Alice, Key(0x11));
        store.Observe(Alice, Key(0x22));
        Assert.Equal(TrustState.KeyChanged, store.Evaluate(Alice, Key(0x22)));

        store.MarkVerified(Alice, Key(0x22));

        Assert.Equal(TrustState.Verified, store.Evaluate(Alice, Key(0x22)));
        Assert.Null(store.KeyChangedAt(Alice));
    }

    [Fact]
    public void ReturningToTheBaselineKeyClearsTheChangedMarker()
    {
        // Someone switches devices and switches back. The warning should not
        // outlive the condition that raised it.
        var (store, _, _, _) = New();

        store.Observe(Alice, Key(0x11));
        store.Observe(Alice, Key(0x22));
        Assert.NotNull(store.KeyChangedAt(Alice));

        store.Observe(Alice, Key(0x11));

        Assert.Null(store.KeyChangedAt(Alice));
        Assert.Equal(TrustState.Tofu, store.Evaluate(Alice, Key(0x11)));
    }

    [Fact]
    public void KeyChangedAt_StampsFirstSightingOnly()
    {
        var (store, _, clock, _) = New();

        store.Observe(Alice, Key(0x11));
        store.Observe(Alice, Key(0x22));
        var first = store.KeyChangedAt(Alice);

        clock.AdvanceDays(3);
        store.Observe(Alice, Key(0x22));   // still the same disagreement

        Assert.Equal(first, store.KeyChangedAt(Alice));
    }

    [Fact]
    public void BlockOutranksVerified()
    {
        var (store, _, _, _) = New();
        var key = Key(0x11);

        store.MarkVerified(Alice, key);
        store.Block(key);

        Assert.Equal(TrustState.Blocked, store.Evaluate(Alice, key));
    }

    [Fact]
    public void BlockAppliesToTheKeyNotThePerson()
    {
        var (store, _, _, _) = New();

        store.Observe(Alice, Key(0x11));
        store.MarkVerified(Alice, Key(0x11));
        store.Block(Key(0x22));

        // The impostor key is blocked; the key Alice actually verified is not.
        Assert.Equal(TrustState.Blocked, store.Evaluate(Alice, Key(0x22)));
        Assert.Equal(TrustState.Verified, store.Evaluate(Alice, Key(0x11)));
    }

    [Fact]
    public void VerifyingAfterBlockClearsTheBlock()
    {
        var (store, _, _, _) = New();
        var key = Key(0x11);

        store.Block(key);
        store.MarkVerified(Alice, key);

        Assert.Equal(TrustState.Verified, store.Evaluate(Alice, key));
    }

    [Fact]
    public void UnblockRestoresThePreviousState()
    {
        var (store, _, _, _) = New();
        var key = Key(0x11);

        store.Observe(Alice, key);
        store.Block(key);
        store.Unblock(key);

        Assert.Equal(TrustState.Tofu, store.Evaluate(Alice, key));
    }

    [Fact]
    public void DecisionsArePersistedThroughSettings()
    {
        // A verification is an out-of-band ritual; losing it to a crash means
        // asking the user to do it again, so every mutation writes through.
        var (store, settings, clock, saves) = New();
        var key = Key(0x11);

        store.MarkVerified(Alice, key);

        var id = TrustStore.KeyId(key);
        Assert.True(settings.VerifiedKeys.ContainsKey(id));
        Assert.Equal(clock.Get(), settings.VerifiedKeys[id]);
        Assert.Equal(id, settings.KnownUserKeys[Alice]);
        Assert.True(saves() > 0);
    }

    [Fact]
    public void ReloadedSettingsPreserveVerification()
    {
        var (store, settings, _, _) = New();
        var key = Key(0x11);
        store.MarkVerified(Alice, key);

        // Same dictionaries, fresh store — stands in for a restart.
        var reloaded = new TrustStore(settings, () => { });

        Assert.Equal(TrustState.Verified, reloaded.Evaluate(Alice, key));
    }

    [Fact]
    public void KeyIdIsLowercaseHex()
    {
        Assert.Equal(new string('1', 64), TrustStore.KeyId(Key(0x11)));
        Assert.Equal(new string('a', 64), TrustStore.KeyId(Key(0xAA)));
    }

    [Fact]
    public void EmptyInputsAreIgnoredRatherThanStored()
    {
        var (store, settings, _, _) = New();

        store.Observe(Alice, Array.Empty<byte>());
        store.Observe("", Key(0x11));
        store.MarkVerified(Alice, Array.Empty<byte>());
        store.Block(Array.Empty<byte>());

        Assert.Empty(settings.KnownUserKeys);
        Assert.Empty(settings.VerifiedKeys);
        Assert.Empty(settings.BlockedKeys);
        Assert.Equal(TrustState.Tofu, store.Evaluate(Alice, Array.Empty<byte>()));
    }
}
