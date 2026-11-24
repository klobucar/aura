using System;
using System.Linq;
using Aura.Desktop.Services;
using SkiaSharp;
using Xunit;

namespace Aura.Desktop.Tests;

public class AvatarImageTests
{
    private static readonly byte[] PngMagic = { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A };
    private static readonly byte[] JpegMagic = { 0xFF, 0xD8, 0xFF };

    private static byte[] MakePng(int width, int height, bool transparentCorner = false, bool noisy = false)
    {
        using var bitmap = new SKBitmap(width, height, SKColorType.Rgba8888, SKAlphaType.Premul);
        using (var canvas = new SKCanvas(bitmap))
        {
            canvas.Clear(new SKColor(0x33, 0x80, 0xE6));
            if (noisy)
            {
                var rng = new Random(7);
                using var paint = new SKPaint();
                for (var i = 0; i < 4000; i++)
                {
                    paint.Color = new SKColor((byte)rng.Next(256), (byte)rng.Next(256), (byte)rng.Next(256));
                    canvas.DrawRect(rng.Next(width), rng.Next(height), rng.Next(1, 40), rng.Next(1, 40), paint);
                }
            }
            if (transparentCorner)
            {
                canvas.Save();
                canvas.ClipRect(new SKRect(0, 0, width / 2f, height / 2f));
                canvas.Clear(SKColors.Transparent);
                canvas.Restore();
            }
        }
        using var image = SKImage.FromBitmap(bitmap);
        return image.Encode(SKEncodedImageFormat.Png, 100).ToArray();
    }

    private static (int W, int H) SizeOf(byte[] data)
    {
        using var bitmap = SKBitmap.Decode(data);
        return (bitmap.Width, bitmap.Height);
    }

    [Fact]
    public void OpaqueImageBecomesSquareJpegWithinCap()
    {
        var output = AvatarImage.Encode(MakePng(1200, 900));
        Assert.NotNull(output);
        Assert.Equal(JpegMagic, output!.Take(3).ToArray());
        Assert.True(output.Length <= AvatarImage.MaxBytes);
        Assert.Equal((256, 256), SizeOf(output));
    }

    [Fact]
    public void TransparentImageStaysPng()
    {
        var output = AvatarImage.Encode(MakePng(400, 400, transparentCorner: true));
        Assert.NotNull(output);
        Assert.Equal(PngMagic, output!.Take(8).ToArray());
        Assert.True(output.Length <= AvatarImage.MaxBytes);
    }

    [Theory]
    [InlineData(2000, 100)]
    [InlineData(100, 2000)]
    [InlineData(257, 513)]
    public void OddAspectRatiosAreCroppedToSquare(int width, int height)
    {
        var output = AvatarImage.Encode(MakePng(width, height));
        Assert.NotNull(output);
        var (w, h) = SizeOf(output!);
        Assert.Equal(w, h);
        Assert.True(w <= AvatarImage.Side);
    }

    [Fact]
    public void SmallImagesAreNotUpscaled()
    {
        var output = AvatarImage.Encode(MakePng(64, 100));
        Assert.NotNull(output);
        Assert.Equal((64, 64), SizeOf(output!));
    }

    [Fact]
    public void NoisyImageStillFitsTheCap()
    {
        var output = AvatarImage.Encode(MakePng(1024, 1024, noisy: true));
        Assert.NotNull(output);
        Assert.True(output!.Length <= AvatarImage.MaxBytes);
    }

    [Fact]
    public void ImpossibleCapGivesNullRatherThanAnOversizedAvatar()
    {
        Assert.Null(AvatarImage.Encode(MakePng(512, 512, noisy: true), maxBytes: 200));
    }

    [Fact]
    public void NonImageDataIsRejected()
    {
        Assert.Null(AvatarImage.Encode(System.Text.Encoding.UTF8.GetBytes("not an image")));
        Assert.Null(AvatarImage.Encode(Array.Empty<byte>()));
    }

    [Fact]
    public void DecodeNeverThrowsAndRefusesEmptyOversizedOrGarbage()
    {
        Assert.Null(AvatarImage.Decode(null));
        Assert.Null(AvatarImage.Decode(Array.Empty<byte>()));
        Assert.Null(AvatarImage.Decode(new byte[AvatarImage.MaxBytes + 1]));
        Assert.Null(AvatarImage.Decode(new byte[] { 1, 2, 3, 4 }));
    }
}

public class AvatarCacheTests
{
    private sealed class Img
    {
    }

    private static (AvatarCache<Img> Cache, Func<int> Decodes) Make(Func<byte[], Img?>? decode = null)
    {
        var count = 0;
        var cache = new AvatarCache<Img>(bytes =>
        {
            count++;
            return decode != null ? decode(bytes) : new Img();
        });
        return (cache, () => count);
    }

    [Fact]
    public void UnchangedBytesAreDecodedOnce()
    {
        var (cache, decodes) = Make();
        var first = cache.Update("u1", new byte[] { 1, 2, 3 });
        var second = cache.Update("u1", new byte[] { 1, 2, 3 });
        Assert.NotNull(first);
        Assert.Same(first, second);
        Assert.Same(first, cache.Get("u1"));
        Assert.Equal(1, decodes());
    }

    [Fact]
    public void ChangedBytesAreDecodedAgain()
    {
        var (cache, decodes) = Make();
        var first = cache.Update("u1", new byte[] { 1 });
        var second = cache.Update("u1", new byte[] { 2 });
        Assert.NotSame(first, second);
        Assert.Equal(2, decodes());
    }

    [Fact]
    public void EmptyOrNullClearsTheEntryAndShowsTheInitial()
    {
        var (cache, _) = Make();
        cache.Update("u1", new byte[] { 1 });
        Assert.Null(cache.Update("u1", Array.Empty<byte>()));
        Assert.Null(cache.Get("u1"));
        cache.Update("u1", new byte[] { 1 });
        Assert.Null(cache.Update("u1", null));
        Assert.Null(cache.Get("u1"));
    }

    [Fact]
    public void OversizedAvatarIsIgnoredWithoutDecoding()
    {
        var (cache, decodes) = Make();
        Assert.Null(cache.Update("u1", new byte[AvatarImage.MaxBytes + 1]));
        Assert.Equal(0, decodes());
    }

    [Fact]
    public void UndecodableAvatarFallsBackAndIsNotRetried()
    {
        var (cache, decodes) = Make(_ => null);
        Assert.Null(cache.Update("u1", new byte[] { 9 }));
        Assert.Null(cache.Update("u1", new byte[] { 9 }));
        Assert.Equal(1, decodes());
    }

    [Fact]
    public void ThrowingDecoderFallsBack()
    {
        var (cache, _) = Make(_ => throw new InvalidOperationException("boom"));
        Assert.Null(cache.Update("u1", new byte[] { 9 }));
    }

    [Fact]
    public void AccountsAreKeptSeparate()
    {
        var (cache, _) = Make();
        var a = cache.Update("a", new byte[] { 1 });
        var b = cache.Update("b", new byte[] { 1 });
        Assert.NotSame(a, b);
        Assert.Null(cache.Get("c"));
        Assert.Null(cache.Get(null));
    }
}
