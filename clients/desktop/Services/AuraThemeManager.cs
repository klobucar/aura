using System;
using Avalonia;
using Avalonia.Markup.Xaml.Styling;

namespace Aura.Desktop.Services;

/// <summary>
/// The three palettes from the handoff. Raw values match the macOS client's
/// <c>AuraThemeType</c> so the persisted setting is portable between clients.
/// </summary>
public enum AuraThemeType
{
    Zenith,
    Frost,
    Bloom,
}

/// <summary>
/// Swaps the merged palette dictionary so all three themes (× light/dark) are
/// live resources rather than a compile-time choice. The palette dictionary is
/// always merged first, ahead of AuraTokens.axaml.
/// </summary>
public static class AuraThemeManager
{
    private const int PaletteSlot = 0;

    public static AuraThemeType Current { get; private set; } = AuraThemeType.Zenith;

    /// <summary>Parse a persisted theme name, falling back to zenith.</summary>
    public static AuraThemeType Parse(string? name) =>
        Enum.TryParse<AuraThemeType>(name, ignoreCase: true, out var parsed) ? parsed : AuraThemeType.Zenith;

    /// <summary>Persisted form — lowercase, matching the macOS raw values.</summary>
    public static string ToSettingValue(AuraThemeType theme) => theme.ToString().ToLowerInvariant();

    /// <summary>
    /// Replace the palette dictionary in Application.Resources. A no-op if the
    /// app isn't up yet or the requested theme is already current.
    /// </summary>
    public static void Apply(AuraThemeType theme)
    {
        var app = Application.Current;
        if (app == null) return;

        var merged = app.Resources.MergedDictionaries;
        if (merged.Count <= PaletteSlot) return;

        try
        {
            merged[PaletteSlot] = new ResourceInclude(new Uri("avares://Aura.Desktop/App.axaml"))
            {
                Source = new Uri($"avares://Aura.Desktop/Themes/{theme}.axaml"),
            };
            Current = theme;
        }
        catch
        {
            // A palette that won't load must never take the window with it; the
            // dictionary already merged in App.axaml stays in place.
        }
    }
}
