using System;
using System.Buffers;
using System.Buffers.Binary;
using System.IO;
using System.Threading;
using System.Threading.Tasks;

namespace Aura.Desktop.Services;

/// <summary>
/// Framing for the server's control stream: <c>[type u8][length u32 LE][payload]</c>
/// (see <c>send_proto_response</c> in crates/aura-server/src/connection.rs).
///
/// Every server → client message is framed this way. The server only ever sends
/// a bare <c>0x00</c> keepalive as a <em>datagram</em>, never on this stream, so
/// a reader that meets a type it does not understand must still consume the
/// length and payload. Reading on from the length bytes would interpret them as
/// the next message type and desynchronise the stream for the rest of the
/// session.
/// </summary>
public static class ControlFrames
{
    /// <summary>Read exactly <c>buf.Length</c> bytes or throw on end of stream.</summary>
    public static async Task ReadExactAsync(Stream stream, byte[] buf, CancellationToken ct)
    {
        int offset = 0;
        while (offset < buf.Length)
        {
            int read = await stream.ReadAsync(buf.AsMemory(offset), ct);
            if (read == 0) throw new EndOfStreamException();
            offset += read;
        }
    }

    /// <summary>Read the length prefix and enforce the caller's limit.</summary>
    private static async Task<int> ReadLengthAsync(Stream stream, int maxLen, CancellationToken ct)
    {
        var lenBuf = new byte[4];
        await ReadExactAsync(stream, lenBuf, ct);
        var len = BinaryPrimitives.ReadUInt32LittleEndian(lenBuf);
        if (len > (uint)maxLen)
        {
            throw new ProtocolException($"Incoming frame too large: {len} bytes (max {maxLen})");
        }
        return (int)len;
    }

    /// <summary>Read a length-prefixed payload (the type byte is already consumed).</summary>
    public static async Task<byte[]> ReadPayloadAsync(Stream stream, int maxLen, CancellationToken ct)
    {
        var len = await ReadLengthAsync(stream, maxLen, ct);
        var payload = new byte[len];
        await ReadExactAsync(stream, payload, ct);
        return payload;
    }

    /// <summary>
    /// Consume and drop a length-prefixed payload, for message types this client
    /// does not handle. Reads in chunks so an unknown-but-large frame cannot force
    /// a matching allocation.
    /// </summary>
    public static async Task<int> DiscardPayloadAsync(Stream stream, int maxLen, CancellationToken ct)
    {
        var len = await ReadLengthAsync(stream, maxLen, ct);
        var remaining = len;
        var scratch = ArrayPool<byte>.Shared.Rent(Math.Min(Math.Max(len, 1), 16 * 1024));
        try
        {
            while (remaining > 0)
            {
                var read = await stream.ReadAsync(scratch.AsMemory(0, Math.Min(remaining, scratch.Length)), ct);
                if (read == 0) throw new EndOfStreamException();
                remaining -= read;
            }
        }
        finally
        {
            ArrayPool<byte>.Shared.Return(scratch);
        }
        return len;
    }

    /// <summary>Build <c>[type][len][payload]</c>.</summary>
    public static byte[] Frame(byte type, ReadOnlySpan<byte> payload)
    {
        var frame = new byte[1 + 4 + payload.Length];
        frame[0] = type;
        BinaryPrimitives.WriteUInt32LittleEndian(frame.AsSpan(1, 4), (uint)payload.Length);
        payload.CopyTo(frame.AsSpan(5));
        return frame;
    }
}
