using System;
using System.Diagnostics;
using System.Linq;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Xunit;

namespace Aura.Desktop.Tests;

public class AuraInviteLinkTests
{
    private const string Fp = "3f8a91c2be0477dd5a1e0c93f6b8220ae7148db53c60a9f104d26e8fb17c5503";

    private static AuraInviteLink Parse(string text)
    {
        Assert.True(AuraInviteLink.TryParse(text, out var link, out var error), error);
        return link!;
    }

    private static string Reject(string text)
    {
        Assert.False(AuraInviteLink.TryParse(text, out _, out var error));
        Assert.NotEmpty(error);
        return error;
    }

    [Fact]
    public void MinimalLinkUsesDefaultPort()
    {
        var link = Parse("aura://chat.example.org");
        Assert.Equal(new AuraInviteLink("chat.example.org", 8443, null, null, null), link);
        Assert.Equal("aura://chat.example.org", link.ToString());
    }

    [Fact]
    public void FullLinkRoundTrips()
    {
        var link = new AuraInviteLink("10.0.0.5", 9000, "C_6fdb a/1", Fp, "p@ss w&rd=1");
        var text = link.ToString();
        Assert.Equal($"aura://10.0.0.5:9000/C_6fdb%20a%2F1?fp={Fp}&pw=p%40ss%20w%26rd%3D1", text);
        Assert.Equal(link, Parse(text));
    }

    [Fact]
    public void DefaultPortIsOmittedEvenIfGiven() =>
        Assert.Equal("aura://h.example/c1", Parse("aura://h.example:8443/c1").ToString());

    [Fact]
    public void Ipv6HostsAreBracketed()
    {
        var link = Parse("aura://[2001:db8::1]:9443/lobby");
        Assert.Equal("2001:db8::1", link.Host);
        Assert.Equal(9443, link.Port);
        Assert.Equal("aura://[2001:db8::1]:9443/lobby", link.ToString());
        Assert.Equal("aura://[::1]", new AuraInviteLink("::1", 8443, null, null, null).ToString());
    }

    [Fact]
    public void FingerprintIsAcceptedInEitherCaseAndEmittedLowercase()
    {
        var link = Parse($"aura://h.example?fp={Fp.ToUpperInvariant()}");
        Assert.Equal(Fp, link.Fingerprint);
        Assert.Equal($"aura://h.example?fp={Fp}", link.ToString());
    }

    [Fact]
    public void PasswordOnlyLinkHasNoFp() =>
        Assert.Equal("aura://h.example?pw=secret", new AuraInviteLink("h.example", 8443, null, null, "secret").ToString());

    [Fact]
    public void UnknownQueryItemsAreIgnored()
    {
        var link = Parse($"aura://h.example/c?utm=x&fp={Fp}&later=1&pw=a");
        Assert.Equal(Fp, link.Fingerprint);
        Assert.Equal("a", link.Password);
        Assert.Equal("c", link.ChannelId);
    }

    [Fact]
    public void SchemeIsCaseInsensitive() => Assert.Equal("h.example", Parse("AURA://h.example").Host);

    [Theory]
    [InlineData("https://h.example")]
    [InlineData("mumble://h.example")]
    [InlineData("h.example")]
    public void OtherSchemesAreRejected(string text) => Reject(text);

    [Theory]
    [InlineData("aura://")]
    [InlineData("aura:///c1")]
    [InlineData("aura://:9000")]
    [InlineData("aura://[]")]
    public void EmptyHostIsRejected(string text) => Reject(text);

    [Theory]
    [InlineData("aura://h.example:0")]
    [InlineData("aura://h.example:65536")]
    [InlineData("aura://h.example:abc")]
    [InlineData("aura://h.example:")]
    [InlineData("aura://h.example:-1")]
    [InlineData("aura://[::1]:99999")]
    public void BadPortsAreRejected(string text) => Reject(text);

    [Theory]
    [InlineData("aura://h.example?fp=abc")]
    [InlineData("aura://h.example?fp=3f8a91c2be0477dd5a1e0c93f6b8220ae7148db53c60a9f104d26e8fb17c550")]
    [InlineData("aura://h.example?fp=zz8a91c2be0477dd5a1e0c93f6b8220ae7148db53c60a9f104d26e8fb17c5503")]
    [InlineData("aura://h.example?fp=")]
    public void MalformedFingerprintsAreRejected(string text) => Reject(text);

    [Theory]
    [InlineData("aura://user@h.example")]
    [InlineData("aura://h ex.example")]
    [InlineData("aura://h.example/a/b")]
    public void OtherMalformedLinksAreRejected(string text) => Reject(text);
}

public class MentionTests
{
    [Theory]
    [InlineData("hey @bob", true)]
    [InlineData("hey @BOB!", true)]
    [InlineData("@bob, look", true)]
    [InlineData("(@bob)", true)]
    [InlineData("@bobby are you there", false)]
    [InlineData("bob without the at", false)]
    [InlineData("email me at x@bob", true)]   // only what follows the name is checked
    public void DisplayNameMentions(string content, bool expected) =>
        Assert.Equal(expected, Mentions.IsMention(content, "bob", null, isOwnMessage: false));

    [Fact]
    public void MultiWordNamesWork() =>
        Assert.True(Mentions.IsMention("ping @Ada Lovelace.", "ada lovelace", null, isOwnMessage: false));

    [Fact]
    public void OwnMessagesNeverMention() =>
        Assert.False(Mentions.IsMention("@bob raid", "bob", new[] { "raid" }, isOwnMessage: true));

    [Theory]
    [InlineData("raid at 9", true)]
    [InlineData("RAID!", true)]
    [InlineData("raiding at 9", false)]
    [InlineData("pre-raid check", true)]
    [InlineData("deploy_raid", false)]
    public void KeywordsAreWholeWords(string content, bool expected) =>
        Assert.Equal(expected, Mentions.IsMention(content, "bob", new[] { "raid" }, isOwnMessage: false));

    [Fact]
    public void KeywordListParsing() =>
        Assert.Equal(new[] { "raid", "standup", "Deploy now" }, Mentions.ParseKeywords(" raid, standup ,,Deploy now, RAID"));

    [Fact]
    public void FindsEveryNameToken()
    {
        var found = Mentions.FindNameMentions("@bob and @Bob. not @bobby", "bob");
        Assert.Equal(new[] { (0, 4), (9, 4) }, found.ToArray());
    }

    [Theory]
    [InlineData(NotifyLevel.Everything, false, true)]
    [InlineData(NotifyLevel.Mentions, false, false)]
    [InlineData(NotifyLevel.Mentions, true, true)]
    [InlineData(NotifyLevel.Nothing, true, false)]
    public void NotifyLevelsDecide(NotifyLevel level, bool mention, bool expected) =>
        Assert.Equal(expected, NotifyLevels.ShouldNotify(level, mention));

    [Fact]
    public void NotifyLevelSettingValuesMatchMacOs()
    {
        Assert.Equal("all", NotifyLevels.ToSetting(NotifyLevel.Everything));
        Assert.Equal("mentions", NotifyLevels.ToSetting(NotifyLevel.Mentions));
        Assert.Equal("none", NotifyLevels.ToSetting(NotifyLevel.Nothing));
        Assert.Equal(NotifyLevel.Mentions, NotifyLevels.Parse("garbage"));
    }
}

/// <summary>Review finding 6: a fast key-up must close a mic the hold opened.</summary>
public class PushToTalkLatchTests
{
    [Fact]
    public void ReleaseBeforeTheOpenFinishesStillCloses()
    {
        var latch = new PushToTalkLatch();
        Assert.True(latch.Press(micOpen: false, DateTime.UtcNow));   // caller starts opening (awaits)
        Assert.True(latch.Release());                                // key-up arrives first: must close
        Assert.False(latch.IsHeld);
    }

    [Fact]
    public void HoldOnAnAlreadyOpenMicLeavesItOpen()
    {
        var latch = new PushToTalkLatch();
        Assert.False(latch.Press(micOpen: true, DateTime.UtcNow));
        Assert.False(latch.Release());
    }

    [Fact]
    public void ManualClickTakesTheMicAwayFromTheHold()
    {
        var latch = new PushToTalkLatch();
        Assert.True(latch.Press(micOpen: false, DateTime.UtcNow));
        latch.Disown();
        Assert.False(latch.Release());
    }

    [Fact]
    public void RepeatedKeyDownIsIgnoredAndReleaseIsIdempotent()
    {
        var latch = new PushToTalkLatch();
        Assert.True(latch.Press(micOpen: false, DateTime.UtcNow));
        Assert.False(latch.Press(micOpen: true, DateTime.UtcNow));   // auto-repeat
        Assert.True(latch.Release());
        Assert.False(latch.Release());
    }
}

/// <summary>Review finding 2: concurrent connect attempts share one run.</summary>
public class SingleFlightTests
{
    [Fact]
    public async Task ConcurrentCallersShareOneRun()
    {
        var flight = new SingleFlight<bool>();
        var started = 0;
        var gate = new TaskCompletionSource<bool>();

        var a = flight.Run(() => { started++; return gate.Task; });
        var b = flight.Run(() => { started++; return Task.FromResult(false); });
        Assert.Same(a, b);
        Assert.True(flight.IsRunning);

        gate.SetResult(true);
        Assert.True(await b);
        Assert.Equal(1, started);

        // Once finished, the next call runs again.
        var c = flight.Run(() => { started++; return Task.FromResult(false); });
        Assert.False(await c);
        Assert.Equal(2, started);
    }
}

/// <summary>Review finding 3: a lingering channel's MLS state must not become our sending key.</summary>
public class VoiceKeyScopeTests
{
    [Theory]
    [InlineData("C_now", "C_now", true)]
    [InlineData("C_now", "C_left", false)]
    [InlineData("", "C_left", false)]
    [InlineData("C_now", "", false)]
    public void OnlyTheCurrentChannelsKeysApply(string current, string updated, bool expected) =>
        Assert.Equal(expected, AuraNetworkClient.ShouldApplyVoiceKeys(current, updated));
}

/// <summary>
/// Review finding 4: markdown parsing runs on the UI thread over peer-controlled
/// text up to the server's 256 KB frame limit; it must stay fast.
/// </summary>
public class MarkdownTimingTests
{
    private static long Ms(Action action)
    {
        action(); // warm-up / JIT
        var sw = Stopwatch.StartNew();
        action();
        return sw.ElapsedMilliseconds;
    }

    [Theory]
    [InlineData("*a ")]
    [InlineData("_a ")]
    [InlineData("**a ")]
    [InlineData("`")]
    [InlineData("[a ")]
    [InlineData("*")]
    public void HostileInputAtTheComposerCapIsFast(string unit)
    {
        var text = string.Concat(Enumerable.Repeat(unit, InlineMarkdown.MaxMarkdownLength / unit.Length));
        Assert.True(text.Length <= InlineMarkdown.MaxMarkdownLength);
        Assert.InRange(Ms(() => InlineMarkdown.ParseInline(text)), 0, 50);
    }

    [Fact]
    public void ServerMaximumIsRenderedPlainAndFast()
    {
        var text = string.Concat(Enumerable.Repeat("*a ", 256 * 1024 / 3));
        Assert.InRange(Ms(() =>
        {
            var spans = InlineMarkdown.ParseInline(text);
            Assert.Single(spans);
            Assert.Equal(MdStyle.None, spans[0].Style);
        }), 0, 50);
        Assert.InRange(Ms(() => InlineMarkdown.PlainText(text)), 0, 50);
    }

    [Fact]
    public void MemoisationDoesNotChangeOrdinaryResults()
    {
        var spans = InlineMarkdown.ParseInline("a then **bold** and *it* and `x` and * stray");
        Assert.Contains(spans, s => s.Text == "bold" && s.Style == MdStyle.Bold);
        Assert.Contains(spans, s => s.Text == "it" && s.Style == MdStyle.Italic);
        Assert.Contains(spans, s => s.Text == "x" && s.Style == MdStyle.Code);
    }
}

/// <summary>Identity backups round-trip through aura-core's format; failures read well.</summary>
public class IdentityBackupTests
{
    [Fact]
    public void BackupRoundTripsAndWrongPassphraseIsExplained()
    {
        using var identity = UserIdentity.Generate("ada");
        var backup = IdentityBackup.Encrypt(identity, "correct horse battery");

        var restored = IdentityBackup.Decrypt(backup, "correct horse battery");
        Assert.Equal("ada", restored.DisplayName);
        using var again = UserIdentity.FromSecret(restored.Secret, restored.DisplayName);
        Assert.Equal(identity.IdentityPublicKey, again.IdentityPublicKey);
        Assert.Equal(IdentityBackup.ShortFingerprint(identity.IdentityPublicKey), restored.ShortFingerprint);

        var wrong = Assert.ThrowsAny<Exception>(() => IdentityBackup.Decrypt(backup, "wrong passphrase"));
        Assert.Equal("Wrong passphrase, or the file is damaged.", IdentityBackup.ErrorMessage(wrong));
    }

    [Fact]
    public void ArbitraryFilesAreNotBackups()
    {
        var notBackup = Assert.ThrowsAny<Exception>(() =>
            IdentityBackup.Decrypt(System.Text.Encoding.UTF8.GetBytes("{\"version\":2}"), "whatever"));
        Assert.Equal("That file isn't an Aura identity backup.", IdentityBackup.ErrorMessage(notBackup));
    }
}
