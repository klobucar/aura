using System;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Aura.Desktop.ViewModels;
using Avalonia.Input;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// The control stream must survive message types the client doesn't handle.
/// Every server frame is [type][u32 LE len][payload]; skipping only the type
/// byte (the old behaviour) read the length as the next type and desynced the
/// stream for the rest of the session — e.g. on the first ProfileUpdated (0x46).
/// </summary>
public class ControlFramesTests
{
    private static MemoryStream StreamOf(params byte[][] frames) =>
        new(frames.SelectMany(f => f).ToArray());

    [Fact]
    public async Task UnknownFrameIsSkippedWholeAndNextFrameReadsCleanly()
    {
        var unknown = ControlFrames.Frame(0x46, new byte[] { 1, 2, 3, 4, 5, 6, 7 });
        var next = ControlFrames.Frame(0x11, new byte[] { 0xAA, 0xBB });
        using var stream = StreamOf(unknown, next);

        var type = new byte[1];
        await ControlFrames.ReadExactAsync(stream, type, CancellationToken.None);
        Assert.Equal(0x46, type[0]);
        Assert.Equal(7, await ControlFrames.DiscardPayloadAsync(stream, 1024, CancellationToken.None));

        await ControlFrames.ReadExactAsync(stream, type, CancellationToken.None);
        Assert.Equal(0x11, type[0]);
        Assert.Equal(new byte[] { 0xAA, 0xBB }, await ControlFrames.ReadPayloadAsync(stream, 1024, CancellationToken.None));
    }

    [Fact]
    public async Task LargeUnknownFrameIsDiscardedInChunks()
    {
        var big = ControlFrames.Frame(0x7F, new byte[100_000]);
        var next = ControlFrames.Frame(0x12, Array.Empty<byte>());
        using var stream = StreamOf(big, next);

        var type = new byte[1];
        await ControlFrames.ReadExactAsync(stream, type, CancellationToken.None);
        Assert.Equal(100_000, await ControlFrames.DiscardPayloadAsync(stream, 2 * 1024 * 1024, CancellationToken.None));
        await ControlFrames.ReadExactAsync(stream, type, CancellationToken.None);
        Assert.Equal(0x12, type[0]);
    }

    [Fact]
    public async Task OversizedFrameIsRejected()
    {
        using var stream = new MemoryStream(ControlFrames.Frame(0x30, new byte[64])[1..]);
        await Assert.ThrowsAsync<ProtocolException>(() => ControlFrames.ReadPayloadAsync(stream, 16, CancellationToken.None));
    }

    [Fact]
    public async Task TruncatedFrameThrowsEndOfStream()
    {
        var frame = ControlFrames.Frame(0x30, new byte[10]);
        using var stream = new MemoryStream(frame[1..^3]);
        await Assert.ThrowsAsync<EndOfStreamException>(() => ControlFrames.ReadPayloadAsync(stream, 1024, CancellationToken.None));
    }

    [Fact]
    public void FrameLayoutMatchesTheServer()
    {
        var frame = ControlFrames.Frame(0x42, new byte[] { 9, 8 });
        Assert.Equal(new byte[] { 0x42, 2, 0, 0, 0, 9, 8 }, frame);
    }
}

public class InlineMarkdownTests
{
    private static string Render(string md) => string.Join("|", InlineMarkdown.ParseInline(md).Select(s =>
        $"{s.Style}:{s.Text}{(s.Link != null ? "@" + s.Link : "")}"));

    [Fact]
    public void PlainTextIsOneSpan() => Assert.Equal("None:hello world", Render("hello world"));

    [Fact]
    public void BoldItalicAndCode()
    {
        Assert.Equal("Bold:Audio nerd.|None: poke me", Render("**Audio nerd.** poke me"));
        Assert.Equal("None:a |Italic:b|None: c", Render("a *b* c"));
        Assert.Equal("None:it's the |Code:AEC", Render("it's the `AEC`"));
        Assert.Equal("Bold:x", Render("__x__"));
    }

    [Fact]
    public void NestedEmphasis() =>
        Assert.Equal("Bold:bold |Bold, Italic:and italic", Render("**bold *and italic***"));

    [Fact]
    public void CodeContentsAreLiteral() =>
        Assert.Equal("Code:**not bold**", Render("`**not bold**`"));

    [Fact]
    public void SnakeCaseIsNotItalic() =>
        Assert.Equal("None:use some_long_name here", Render("use some_long_name here"));

    [Fact]
    public void UnmatchedDelimitersStayLiteral()
    {
        Assert.Equal("None:2 * 3 = 6", Render("2 * 3 = 6"));
        Assert.Equal("None:**unclosed", Render("**unclosed"));
        Assert.Equal("None:`tick", Render("`tick"));
    }

    [Fact]
    public void EscapesSuppressMarkup() => Assert.Equal("None:*literal*", Render(@"\*literal\*"));

    [Fact]
    public void LinksCarryTargets()
    {
        var spans = InlineMarkdown.ParseInline("see [focus room](aura://focus) or [docs](https://example.org/a)");
        var focus = spans.Single(s => s.Text == "focus room");
        var docs = spans.Single(s => s.Text == "docs");
        Assert.Equal("aura://focus", focus.Link);
        Assert.False(focus.IsOpenableLink);   // only web links are followed
        Assert.True(docs.IsOpenableLink);
    }

    [Fact]
    public void JavascriptLinksAreNeverOpenable() =>
        Assert.DoesNotContain(InlineMarkdown.ParseInline("[x](javascript:alert(1))"), s => s.IsOpenableLink);

    [Fact]
    public void NoHeadingsTablesOrHtml() =>
        Assert.Equal("None:# not a heading <b>no html</b>", Render("# not a heading <b>no html</b>"));

    [Fact]
    public void BlocksSplitBulletsAndQuotes()
    {
        var blocks = InlineMarkdown.ParseBlocks("**Audio nerd.**\n> PTT is ⌥Space here\n- rebuilds Thursdays\n- [focus room](aura://focus)");
        Assert.Equal(new[] { MdBlockKind.Paragraph, MdBlockKind.Quote, MdBlockKind.Bullet, MdBlockKind.Bullet },
            blocks.Select(b => b.Kind).ToArray());
        Assert.Equal("PTT is ⌥Space here", blocks[1].Spans.Single().Text);
    }

    [Fact]
    public void PlainTextStripsMarkup() =>
        Assert.Equal("bold and code", InlineMarkdown.PlainText("**bold** and `code`"));
}

public class ChatHistoryTests
{
    [Fact]
    public void ChannelsKeepSeparateHistories()
    {
        var history = new ChatHistory();
        history.Add(new ChatMessage { ChannelId = "a", Content = "in a" });
        history.Add(new ChatMessage { ChannelId = "b", Content = "in b" });

        Assert.Equal("in a", history.For("a").Single().Content);
        Assert.Equal("in b", history.For("b").Single().Content);
        Assert.Empty(history.For("c"));
    }

    [Fact]
    public void DuplicateMessageIdsAreDropped()
    {
        var history = new ChatHistory();
        Assert.True(history.Add(new ChatMessage { Id = "m1", ChannelId = "a" }));
        Assert.False(history.Add(new ChatMessage { Id = "m1", ChannelId = "a" }));
        Assert.Single(history.For("a"));
    }

    [Fact]
    public void OnlyTheLatestJoinMarkerIsKept()
    {
        var history = new ChatHistory();
        ChatMessage Joined(string channel) =>
            new() { ChannelId = channel, Content = "Joined", System = true, IsJoinMarker = true };

        history.AddJoinMarker(Joined("a"));
        history.Add(new ChatMessage { ChannelId = "a", Content = "hello" });
        history.Add(new ChatMessage { ChannelId = "a", Content = "jk-test joined", System = true });
        history.AddJoinMarker(Joined("b"));
        history.AddJoinMarker(Joined("a"));

        // One marker, after the messages; other notices are left alone.
        Assert.Equal(new[] { "hello", "jk-test joined", "Joined" }, history.For("a").Select(m => m.Content));
        Assert.Single(history.For("b"));
    }

    [Fact]
    public void HistoryIsBoundedPerChannel()
    {
        var history = new ChatHistory();
        for (var i = 0; i < ChatHistory.MaxPerChannel + 25; i++)
            history.Add(new ChatMessage { ChannelId = "a", Content = i.ToString() });

        var list = history.For("a");
        Assert.Equal(ChatHistory.MaxPerChannel, list.Count);
        Assert.Equal("25", list[0].Content);
    }

    [Fact]
    public void FindLocatesReplyTargetsInTheirChannel()
    {
        var history = new ChatHistory();
        history.Add(new ChatMessage { Id = "q", ChannelId = "a", Content = "quoted" });
        Assert.NotNull(history.Find("a", "q"));
        Assert.Null(history.Find("b", "q"));
        Assert.Null(history.Find("a", null));
    }

    [Fact]
    public void ReplyPreviewIsPlainAndTruncated()
    {
        Assert.Equal("shipping the meter first", ChatMessage.PreviewOf("**shipping** the meter\nfirst"));
        var longText = new string('x', 200);
        Assert.Equal(81, ChatMessage.PreviewOf(longText).Length);
    }

    [Fact]
    public void FailedSendIsVisibleOnTheMessage()
    {
        var message = new ChatMessage { IsFromCurrentUser = true };
        Assert.False(message.SendFailed);
        message.SendError = "the channel's encryption isn't ready yet";
        Assert.True(message.SendFailed);
        Assert.StartsWith("not sent · ", message.SendErrorLabel);
    }
}

public class PushToTalkTests
{
    [Fact]
    public void DefaultBindingIsCtrlSpace() => Assert.Equal("Ctrl+Space", PushToTalkBinding.Default.Display);

    [Fact]
    public void BindingRoundTripsThroughSettings()
    {
        var binding = new PushToTalkBinding(Key.F13, KeyModifiers.Alt | KeyModifiers.Shift);
        Assert.Equal(binding, PushToTalkBinding.Parse(binding.ToSetting()));
        Assert.Equal("Alt+Shift+F13", binding.Display);
    }

    [Fact]
    public void GarbageOrModifierOnlySettingsFallBackToDefault()
    {
        Assert.Equal(PushToTalkBinding.Default, PushToTalkBinding.Parse(null));
        Assert.Equal(PushToTalkBinding.Default, PushToTalkBinding.Parse("nonsense"));
        Assert.Equal(PushToTalkBinding.Default, PushToTalkBinding.Parse($"0:{Key.LeftCtrl}"));
    }

    [Fact]
    public void ExtraModifiersStillMatch()
    {
        var binding = new PushToTalkBinding(Key.Space, KeyModifiers.Control);
        Assert.True(binding.Matches(Key.Space, KeyModifiers.Control | KeyModifiers.Shift));
        Assert.False(binding.Matches(Key.Space, KeyModifiers.None));
        Assert.False(binding.Matches(Key.Enter, KeyModifiers.Control));
    }

    [Fact]
    public void VirtualKeysMatchWin32()
    {
        Assert.Equal(0x20, PushToTalkBinding.Default.VirtualKey);
        Assert.Equal(0x41, new PushToTalkBinding(Key.A, KeyModifiers.None).VirtualKey);
        Assert.Equal(0x7C, new PushToTalkBinding(Key.F13, KeyModifiers.None).VirtualKey);
        Assert.Equal(0xC0, new PushToTalkBinding(Key.OemTilde, KeyModifiers.None).VirtualKey);
    }

    [Fact]
    public void MacKeyCodesMatchHIToolbox()
    {
        Assert.Equal(0x31, PushToTalkBinding.Default.MacKeyCode);
        Assert.Equal(0x00, new PushToTalkBinding(Key.A, KeyModifiers.None).MacKeyCode);
        Assert.Equal(0x1D, new PushToTalkBinding(Key.D0, KeyModifiers.None).MacKeyCode);
        Assert.Equal(0x69, new PushToTalkBinding(Key.F13, KeyModifiers.None).MacKeyCode);
        Assert.Equal(0x32, new PushToTalkBinding(Key.OemTilde, KeyModifiers.None).MacKeyCode);
        Assert.Null(new PushToTalkBinding(Key.CapsLock, KeyModifiers.None).MacKeyCode);
    }

    [Fact]
    public void TransmissionModesUseMacOsRawValues()
    {
        foreach (var mode in Enum.GetValues<TransmissionMode>())
            Assert.Equal(mode, TransmissionModes.Parse(TransmissionModes.ToSetting(mode)));
        Assert.Equal("vad", TransmissionModes.ToSetting(TransmissionMode.VoiceActivation));
        Assert.Equal("Voice Activation", TransmissionModes.DisplayName(TransmissionMode.VoiceActivation));
    }

    [Fact]
    public void VadThresholdMatchesMacOsMapping()
    {
        Assert.Equal(-50f, TransmissionModes.VadThresholdDb(0));
        Assert.Equal(-35f, TransmissionModes.VadThresholdDb(0.5));
        Assert.Equal(-20f, TransmissionModes.VadThresholdDb(1));
    }

    [Fact]
    public void ReconnectBackoffMatchesMacOs()
    {
        Assert.Equal(new[] { 1.0, 2, 4, 8, 16 },
            Enumerable.Range(1, ReconnectPolicy.MaxAttempts).Select(a => ReconnectPolicy.DelayFor(a).TotalSeconds).ToArray());
    }
}

/// <summary>
/// No audio may leave the client until the channel's MLS voice group has
/// supplied a key: the bootstrap key is a public constant, and the old
/// fallback sent raw PCM outright.
/// </summary>
public class AudioKeyGateTests
{
    private static short[] LoudFrame()
    {
        var pcm = new short[960];
        for (var i = 0; i < pcm.Length; i++) pcm[i] = (short)(Math.Sin(i * 0.1) * 12000);
        return pcm;
    }

    [Fact]
    public void NothingIsSentBeforeAGroupKey()
    {
        using var manager = new AudioManager();
        Assert.Null(manager.ProcessCapture(LoudFrame()));   // not initialised

        manager.Initialize(7, Enumerable.Repeat((byte)0x42, 32).ToArray());
        Assert.False(manager.HasGroupKey);
        Assert.Null(manager.ProcessCapture(LoudFrame()));   // bootstrap key only
    }

    [Fact]
    public void GroupKeyArmsSendingAndChannelSwitchDisarms()
    {
        using var manager = new AudioManager();
        manager.Initialize(7, new byte[32]);
        manager.UpdateSenderKey(Enumerable.Range(0, 32).Select(i => (byte)i).ToArray(), 1);
        Assert.True(manager.HasGroupKey);

        // The encoder may need a frame or two of lookahead before emitting.
        byte[]? packet = null;
        for (var i = 0; i < 5 && packet == null; i++) packet = manager.ProcessCapture(LoudFrame());
        Assert.NotNull(packet);

        manager.ClearGroupKey();
        Assert.Null(manager.ProcessCapture(LoudFrame()));
    }
}

public class P0BlockerTests
{
    [Fact]
    public void AudioDeviceSettingsDefaultToEmptyAndRoundTrip()
    {
        var settings = new AppSettings
        {
            AudioInputDevice = "USB Mic",
            AudioOutputDevice = "Studio Headphones",
        };
        var json = System.Text.Json.JsonSerializer.Serialize(settings);
        Assert.Contains("AuraAudioInputDevice", json);
        Assert.Contains("AuraAudioOutputDevice", json);

        var loaded = System.Text.Json.JsonSerializer.Deserialize<AppSettings>(json);
        Assert.NotNull(loaded);
        Assert.Equal("USB Mic", loaded!.AudioInputDevice);
        Assert.Equal("Studio Headphones", loaded.AudioOutputDevice);
    }

    [Fact]
    public void ChatMessageFormatsAttachmentStateAndSize()
    {
        var msg = new Aura.Desktop.ViewModels.ChatMessage
        {
            AttachmentFileIdHex = "abcd",
            AttachmentFileName = "screenshot.png",
            AttachmentSize = 1_572_864,
            AttachmentStatus = "Ready",
        };
        Assert.True(msg.HasAttachment);
        Assert.Equal("1.5 MB", msg.AttachmentSizeLabel);
        Assert.False(msg.CanSaveAttachment);

        msg.AttachmentLocalPath = "/tmp/screenshot.png";
        Assert.True(msg.CanSaveAttachment);
    }

    [Fact]
    public void SanitizeFileNameStripsTraversalAndBidiControls()
    {
        Assert.Equal(".._.._etc_passwd", AuraNetworkClient.SanitizeFileName("../../etc/passwd"));
        Assert.Equal("evil_exe", AuraNetworkClient.SanitizeFileName("evil\u202Eexe"));
        Assert.Equal("bad_name_.txt", AuraNetworkClient.SanitizeFileName("bad:name?.txt"));
        Assert.Equal("_CON.txt", AuraNetworkClient.SanitizeFileName("CON.txt"));
        Assert.Equal("_nul", AuraNetworkClient.SanitizeFileName("nul"));
        Assert.Equal("attachment", AuraNetworkClient.SanitizeFileName(".."));
        Assert.Equal("attachment", AuraNetworkClient.SanitizeFileName(""));
    }
}

public class P2DesktopFeaturesTests
{
    [Fact]
    public void CallQualityStatsCalculatesLossPercentAndFormatsSummary()
    {
        var stats = new CallQualityStats(PacketsReceived: 98, PacketsLost: 2, PlcCount: 3, JitterMs: 12.5);
        Assert.Equal(2.0, stats.LossPercent, precision: 2);
        var summary = stats.FormatSummary();
        Assert.Contains("Loss: 2.0%", summary);
        Assert.Contains("PLC: 3", summary);
        Assert.Contains("Jitter: 12.5 ms", summary);

        var empty = new CallQualityStats(0, 0, 0, 0);
        Assert.Equal(0.0, empty.LossPercent);
    }

    [Fact]
    public void OpusBitrateDefaultsTo64KbpsAndRoundTripsInAppSettings()
    {
        Assert.Equal(64, new AppSettings().OpusBitrateKbps);
        var settings = new AppSettings
        {
            OpusBitrateKbps = 96,
            PresenceStatus = "dnd",
            CustomStatus = "In a meeting",
            VoiceSoundCues = false,
        };
        var json = System.Text.Json.JsonSerializer.Serialize(settings);
        Assert.Contains("AuraOpusBitrateKbps", json);
        Assert.Contains("AuraPresenceStatus", json);
        Assert.Contains("AuraCustomStatus", json);
        Assert.Contains("AuraVoiceSoundCues", json);

        var loaded = System.Text.Json.JsonSerializer.Deserialize<AppSettings>(json);
        Assert.NotNull(loaded);
        Assert.Equal(96, loaded!.OpusBitrateKbps);
        Assert.Equal("dnd", loaded.PresenceStatus);
        Assert.Equal("In a meeting", loaded.CustomStatus);
        Assert.False(loaded.VoiceSoundCues);
    }

    [Fact]
    public void VoiceSoundCuesSynthesizesNonEmptyPcmForAllKinds()
    {
        foreach (VoiceSoundCues.CueKind kind in Enum.GetValues(typeof(VoiceSoundCues.CueKind)))
        {
            var pcm = VoiceSoundCues.Synthesize(kind);
            Assert.NotEmpty(pcm);
            Assert.Contains(pcm, s => s != 0);
        }
    }
}

public class P1BetaUsabilityTests
{
    [Fact]
    public void SavedServerBookmarksRoundTripAndUpsert()
    {
        var tempDir = Path.Combine(Path.GetTempPath(), $"aura-test-{Guid.NewGuid():N}");
        Directory.CreateDirectory(tempDir);
        var path = Path.Combine(tempDir, "servers.json");
        try
        {
            var list = new System.Collections.Generic.List<ServerBookmark>();
            AppSettings.UpsertSavedServer(list, new ServerBookmark
            {
                Name = "Home",
                Host = "aura.example.com",
                Port = 8443,
            });
            AppSettings.UpsertSavedServer(list, new ServerBookmark
            {
                Name = "Home Updated",
                Host = "AURA.EXAMPLE.COM",
                Port = 8443,
            });

            Assert.Single(list);
            Assert.Equal("Home Updated", list[0].Name);

            AppSettings.SaveSavedServers(list, path);
            var loaded = AppSettings.LoadSavedServers(path);
            Assert.Single(loaded);
            Assert.Equal("Home Updated", loaded[0].Name);
            Assert.Equal("AURA.EXAMPLE.COM", loaded[0].Host);
            Assert.Equal(8443, loaded[0].Port);
        }
        finally
        {
            if (Directory.Exists(tempDir)) Directory.Delete(tempDir, recursive: true);
        }
    }

    [Fact]
    public void InlineMarkdownOnlyTreatsIdentifierFirstLineAsLanguageTagAndRendersUnterminatedFenceToEof()
    {
        var nonTag = InlineMarkdown.ParseInline("```let x = 1;\nlet y = 2;\n```");
        Assert.Single(nonTag);
        Assert.Equal("let x = 1;\nlet y = 2;", nonTag[0].Text);
        Assert.Equal(MdStyle.Code, nonTag[0].Style);

        var unterminated = InlineMarkdown.ParseInline("Hello\n```python\nprint(1)");
        Assert.Equal(2, unterminated.Count);
        Assert.Equal("Hello\n", unterminated[0].Text);
        Assert.Equal("print(1)", unterminated[1].Text);
        Assert.Equal(MdStyle.Code, unterminated[1].Style);
    }

    [Fact]
    public void InlineMarkdownParsesFencedCodeBlocksWithLanguageAndLiteralBody()
    {
        var md = "Before\n```rust\nlet **x** = 1;\nlet y = `2`;\n```\nAfter";
        var spans = InlineMarkdown.ParseInline(md);

        Assert.Equal(3, spans.Count);
        Assert.Equal("Before\n", spans[0].Text);
        Assert.Equal(MdStyle.None, spans[0].Style);

        Assert.Equal("let **x** = 1;\nlet y = `2`;", spans[1].Text);
        Assert.Equal(MdStyle.Code, spans[1].Style);

        Assert.Equal("\nAfter", spans[2].Text);
        Assert.Equal(MdStyle.None, spans[2].Style);
    }

    [Fact]
    public void ParseBlocksHandlesMultiLineFencedCodeBlocks()
    {
        var md = "Intro\n```csharp\nvar x = 1;\nvar y = 2;\n```\n- item";
        var blocks = InlineMarkdown.ParseBlocks(md);

        Assert.Equal(new[] { MdBlockKind.Paragraph, MdBlockKind.CodeBlock, MdBlockKind.Bullet },
            blocks.Select(b => b.Kind).ToArray());
        Assert.Equal("var x = 1;\nvar y = 2;", blocks[1].Spans.Single().Text);
        Assert.Equal(MdStyle.Code, blocks[1].Spans.Single().Style);
    }
}
