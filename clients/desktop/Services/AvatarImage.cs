using System;
using System.Collections.Generic;
using System.IO;
using Avalonia.Media.Imaging;
using SkiaSharp;

namespace Aura.Desktop.Services;

/// <summary>
/// Avatars: shrinking a picked image into something the server accepts
/// (square, at most 256 px, JPEG or PNG, at most 128 KiB), and decoding what
/// the server sends back without ever throwing.
/// </summary>
public static class AvatarImage
{
    /// <summary>The server's limit on <c>avatar_data</c>.</summary>
    public const int MaxBytes = 128 * 1024;

    /// <summary>Edge length of the square avatar we upload.</summary>
    public const int Side = 256;

    /// <summary>
    /// Center-crop <paramref name="source"/> to a square, scale it down to at
    /// most <paramref name="side"/> pixels (never up), and encode it: PNG when
    /// the picture really uses transparency and fits, JPEG otherwise. Null when
    /// the bytes aren't a decodable image or can't be brought under
    /// <paramref name="maxBytes"/>.
    /// </summary>
    public static byte[]? Encode(byte[] source, int side = Side, int maxBytes = MaxBytes)
    {
        try
        {
            using var bitmap = SKBitmap.Decode(source);
            if (bitmap == null || bitmap.Width <= 0 || bitmap.Height <= 0) return null;
            using var image = SKImage.FromBitmap(bitmap);

            var edge = Math.Min(bitmap.Width, bitmap.Height);
            var crop = SKRect.Create((bitmap.Width - edge) / 2f, (bitmap.Height - edge) / 2f, edge, edge);
            var sampling = new SKSamplingOptions(SKFilterMode.Linear, SKMipmapMode.Linear);

            for (var target = Math.Min(side, edge); target >= 32; target = target * 3 / 4)
            {
                var dest = SKRect.Create(0, 0, target, target);
                using var surface = SKSurface.Create(new SKImageInfo(target, target, SKColorType.Rgba8888, SKAlphaType.Premul));
                surface.Canvas.Clear(SKColors.Transparent);
                surface.Canvas.DrawImage(image, crop, dest, sampling);
                using var square = surface.Snapshot();

                if (HasTransparency(square))
                {
                    var png = Encode(square, SKEncodedImageFormat.Png, 100);
                    if (png != null && png.Length <= maxBytes) return png;
                }

                // JPEG has no alpha: composite over white.
                using var flat = SKSurface.Create(new SKImageInfo(target, target, SKColorType.Rgba8888, SKAlphaType.Opaque));
                flat.Canvas.Clear(SKColors.White);
                flat.Canvas.DrawImage(square, 0, 0);
                using var flatImage = flat.Snapshot();
                for (var quality = 85; quality >= 35; quality -= 10)
                {
                    var jpeg = Encode(flatImage, SKEncodedImageFormat.Jpeg, quality);
                    if (jpeg != null && jpeg.Length <= maxBytes) return jpeg;
                }
            }
            return null;
        }
        catch (Exception)
        {
            return null;
        }
    }

    /// <summary>
    /// Decode avatar bytes from the server into a small bitmap. Null for empty
    /// data, anything over the 128 KiB limit, or anything that won't decode:
    /// callers then fall back to the initial.
    /// </summary>
    public static Bitmap? Decode(byte[]? data)
    {
        if (data == null || data.Length == 0 || data.Length > MaxBytes) return null;
        try
        {
            using var stream = new MemoryStream(data, writable: false);
            return Bitmap.DecodeToWidth(stream, 128);
        }
        catch (Exception)
        {
            return null;
        }
    }

    private static bool HasTransparency(SKImage image)
    {
        using var bitmap = SKBitmap.FromImage(image);
        foreach (var pixel in bitmap.Pixels)
        {
            if (pixel.Alpha < 255) return true;
        }
        return false;
    }

    private static byte[]? Encode(SKImage image, SKEncodedImageFormat format, int quality)
    {
        using var data = image.Encode(format, quality);
        return data?.ToArray();
    }
}

/// <summary>
/// Decoded avatars per account, so a roster rebuild or a repeated profile
/// broadcast doesn't decode the same bytes again. Generic over the image type
/// so the logic is testable without a rendering platform.
/// </summary>
public sealed class AvatarCache<T> where T : class
{
    private readonly Func<byte[], T?> _decode;
    private readonly Dictionary<string, (byte[] Data, T? Image)> _entries = new();

    public AvatarCache(Func<byte[], T?> decode) => _decode = decode;

    /// <summary>The cached image for an account, or null (show the initial).</summary>
    public T? Get(string? uuid) =>
        uuid != null && _entries.TryGetValue(uuid, out var entry) ? entry.Image : null;

    /// <summary>
    /// Record an account's avatar bytes and return its image. Empty,
    /// oversized and undecodable data all give null; an unchanged avatar
    /// reuses the cached image, including a failure, so a bad avatar is
    /// decoded once rather than on every broadcast.
    /// </summary>
    public T? Update(string? uuid, byte[]? data)
    {
        if (string.IsNullOrEmpty(uuid)) return null;
        if (data == null || data.Length == 0 || data.Length > AvatarImage.MaxBytes)
        {
            _entries.Remove(uuid);
            return null;
        }
        if (_entries.TryGetValue(uuid, out var existing) && existing.Data.AsSpan().SequenceEqual(data))
            return existing.Image;

        T? image;
        try { image = _decode(data); }
        catch (Exception) { image = null; }
        _entries[uuid] = (data, image);
        return image;
    }

    public void Clear() => _entries.Clear();
}
