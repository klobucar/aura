using System;
using System.Text;
using System.Security.Cryptography;
using NSec.Cryptography;
using Google.Protobuf;

namespace Aura.Desktop.Services;

/// <summary>
/// Text message encryption/decryption using XChaCha20-Poly1305 (DAVE protocol).
/// Matches the Rust implementation in aura-core/src/text_crypto.rs
/// </summary>
public class TextCryptoService
{
    private readonly Key _daveKey;
    private static readonly XChaCha20Poly1305 _algorithm = new XChaCha20Poly1305();
    
    public TextCryptoService(byte[] key)
    {
        if (key.Length != 32)
            throw new ArgumentException("DAVE key must be 32 bytes", nameof(key));
        
        _daveKey = Key.Import(_algorithm, key, KeyBlobFormat.RawSymmetricKey);
    }
    
    /// <summary>
    /// Encrypt a text message using DAVE (XChaCha20-Poly1305 with zero-padding commitment).
    /// </summary>
    public EncryptedTextPacket Encrypt(
        ulong epoch,
        uint channelId,
        uint senderSessionId,
        string senderUuid,
        string content,
        string messageId,
        string replyToId = "")
    {
        // Create protobuf TextMessage
        var textMsg = new Aura.V1Alpha1.TextMessage
        {
            SenderUuid = senderUuid,
            Timestamp = (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            Content = content,
            MessageId = messageId,
            ReplyToId = replyToId
        };
        
        // Serialize to bytes
        var plaintext = textMsg.ToByteArray();
        
        // Add zero-padding commitment (16 bytes of 0x00)
        var paddedPlaintext = new byte[16 + plaintext.Length];
        Array.Copy(plaintext, 0, paddedPlaintext, 16, plaintext.Length);
        
        // Generate random 24-byte nonce for XChaCha20
        var nonce = new byte[24];
        RandomNumberGenerator.Fill(nonce);
        
        // Encrypt with XChaCha20-Poly1305
        var ciphertext = _algorithm.Encrypt(_daveKey, nonce, null, paddedPlaintext);
        
        // Split ciphertext and tag (last 16 bytes)
        var ciphertextOnly = new byte[ciphertext.Length - 16];
        var tag = new byte[16];
        Array.Copy(ciphertext, 0, ciphertextOnly, 0, ciphertextOnly.Length);
        Array.Copy(ciphertext, ciphertextOnly.Length, tag, 0, 16);
        
        return new EncryptedTextPacket
        {
            SenderSessionId = senderSessionId,
            ChannelId = channelId,
            Epoch = epoch,
            Ciphertext = ciphertextOnly,
            Nonce = nonce,
            Tag = tag,
            MessageId = messageId,
            ReplyToId = replyToId
        };
    }
    
    /// <summary>
    /// Decrypt an encrypted text packet using DAVE.
    /// </summary>
    public Aura.V1Alpha1.TextMessage Decrypt(EncryptedTextPacket packet)
    {
        // Reconstruct ciphertext with tag appended
        var ciphertextWithTag = new byte[packet.Ciphertext.Length + packet.Tag.Length];
        Array.Copy(packet.Ciphertext, 0, ciphertextWithTag, 0, packet.Ciphertext.Length);
        Array.Copy(packet.Tag, 0, ciphertextWithTag, packet.Ciphertext.Length, packet.Tag.Length);
        
        // Decrypt with XChaCha20-Poly1305
        var paddedPlaintext = _algorithm.Decrypt(_daveKey, packet.Nonce, null, ciphertextWithTag);
        
        if (paddedPlaintext == null)
            throw new CryptographicException("Decryption failed");
        
        // Verify zero-padding commitment (first 16 bytes must be 0x00)
        for (int i = 0; i < 16; i++)
        {
            if (paddedPlaintext[i] != 0)
                throw new CryptographicException("Zero-padding commitment verification failed");
        }
        
        // Extract actual plaintext (skip first 16 bytes)
        var plaintext = new byte[paddedPlaintext.Length - 16];
        Array.Copy(paddedPlaintext, 16, plaintext, 0, plaintext.Length);
        
        // Deserialize protobuf TextMessage
        return Aura.V1Alpha1.TextMessage.Parser.ParseFrom(plaintext);
    }
}

/// <summary>
/// Encrypted text packet structure (matches Rust EncryptedTextPacket).
/// </summary>
public class EncryptedTextPacket
{
    public uint SenderSessionId { get; set; }
    public uint ChannelId { get; set; }
    public ulong Epoch { get; set; }
    public byte[] Ciphertext { get; set; } = Array.Empty<byte>();
    public byte[] Nonce { get; set; } = Array.Empty<byte>();
    public byte[] Tag { get; set; } = Array.Empty<byte>();
    public string MessageId { get; set; } = string.Empty;
    public string ReplyToId { get; set; } = string.Empty;
}
