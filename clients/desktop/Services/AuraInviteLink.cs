using System;
using System.Globalization;
using System.Linq;
using System.Text;

namespace Aura.Desktop.Services;

/// <summary>
/// An invite link: <c>aura://&lt;host&gt;[:&lt;port&gt;][/&lt;channel-id&gt;][?fp=&lt;sha256-hex&gt;][&amp;pw=&lt;password&gt;]</c>.
/// The format is shared byte-for-byte with the macOS client.
///
/// <list type="bullet">
/// <item>IPv6 hosts are bracketed. The port defaults to 8443 and is omitted when default.</item>
/// <item>The channel is one percent-encoded path segment holding the channel <em>id</em>, not its name.</item>
/// <item><c>fp</c> is the SHA-256 of the server's leaf certificate (DER), 64 hex chars,
///   written lowercase and accepted in either case. It is only present for servers
///   whose certificate is pinned; a CA-issued certificate renews and would break links.</item>
/// <item><c>pw</c> (opt-in) is the server password, percent-encoded.</item>
/// <item>Unknown query items are ignored.</item>
/// </list>
/// </summary>
public sealed record AuraInviteLink(string Host, int Port, string? ChannelId, string? Fingerprint, string? Password)
{
    public const string Scheme = "aura";
    public const int DefaultPort = 8443;

    /// <summary>Format the link. The fingerprint is emitted lowercase.</summary>
    public override string ToString()
    {
        var sb = new StringBuilder("aura://");
        sb.Append(Host.Contains(':') ? $"[{Host}]" : Host);
        if (Port != DefaultPort) sb.Append(':').Append(Port.ToString(CultureInfo.InvariantCulture));
        if (!string.IsNullOrEmpty(ChannelId)) sb.Append('/').Append(Uri.EscapeDataString(ChannelId));

        var query = new StringBuilder();
        if (!string.IsNullOrEmpty(Fingerprint)) query.Append("fp=").Append(Fingerprint.ToLowerInvariant());
        if (!string.IsNullOrEmpty(Password))
        {
            if (query.Length > 0) query.Append('&');
            query.Append("pw=").Append(Uri.EscapeDataString(Password));
        }
        if (query.Length > 0) sb.Append('?').Append(query);
        return sb.ToString();
    }

    /// <summary>Whether <paramref name="text"/> even claims to be an invite link.</summary>
    public static bool LooksLikeInvite(string? text) =>
        text != null && text.TrimStart().StartsWith("aura://", StringComparison.OrdinalIgnoreCase);

    /// <summary>Parse a link. On failure <paramref name="error"/> says why, in words a person can act on.</summary>
    public static bool TryParse(string? text, out AuraInviteLink? link, out string error)
    {
        link = null;
        error = "";
        var s = text?.Trim() ?? "";

        const string prefix = "aura://";
        if (!s.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
        {
            error = "Invite links start with aura://";
            return false;
        }
        s = s[prefix.Length..];

        var hash = s.IndexOf('#');
        if (hash >= 0) s = s[..hash];

        string query = "";
        var q = s.IndexOf('?');
        if (q >= 0)
        {
            query = s[(q + 1)..];
            s = s[..q];
        }

        string path = "";
        string authority = s;
        // A bracketed IPv6 host may contain ':' but never '/'.
        var slash = s.IndexOf('/');
        if (slash >= 0)
        {
            authority = s[..slash];
            path = s[(slash + 1)..];
        }

        // Host and port.
        string host;
        string? portText = null;
        if (authority.StartsWith('['))
        {
            var close = authority.IndexOf(']');
            if (close < 0)
            {
                error = "The server address is missing a closing ]";
                return false;
            }
            host = authority[1..close];
            var rest = authority[(close + 1)..];
            if (rest.Length > 0)
            {
                if (rest[0] != ':')
                {
                    error = "The server address isn't valid";
                    return false;
                }
                portText = rest[1..];
            }
            if (host.Length == 0 || !host.All(c => Uri.IsHexDigit(c) || c is ':' or '.'))
            {
                error = "The server address isn't valid";
                return false;
            }
        }
        else
        {
            var colon = authority.IndexOf(':');
            host = colon >= 0 ? authority[..colon] : authority;
            if (colon >= 0) portText = authority[(colon + 1)..];
            if (host.Length > 0 && !host.All(c => char.IsAsciiLetterOrDigit(c) || c is '-' or '.'))
            {
                error = "The server address isn't valid";
                return false;
            }
        }

        if (host.Length == 0)
        {
            error = "The link has no server address";
            return false;
        }

        var port = DefaultPort;
        if (portText != null)
        {
            if (portText.Length == 0 || !portText.All(char.IsAsciiDigit) ||
                !int.TryParse(portText, NumberStyles.None, CultureInfo.InvariantCulture, out port) ||
                port is < 1 or > 65535)
            {
                error = "The port isn't valid";
                return false;
            }
        }

        // Channel: one path segment (a trailing slash is tolerated).
        string? channelId = null;
        if (path.EndsWith('/')) path = path[..^1];
        if (path.Length > 0)
        {
            if (path.Contains('/'))
            {
                error = "The channel part of the link isn't valid";
                return false;
            }
            try
            {
                channelId = Uri.UnescapeDataString(path);
            }
            catch (UriFormatException)
            {
                error = "The channel part of the link isn't valid";
                return false;
            }
            if (channelId.Length == 0) channelId = null;
        }

        // Query: fp and pw; anything else is ignored. First occurrence wins.
        string? fingerprint = null;
        string? password = null;
        bool sawFp = false, sawPw = false;
        foreach (var item in query.Split('&', StringSplitOptions.RemoveEmptyEntries))
        {
            var eq = item.IndexOf('=');
            var key = eq >= 0 ? item[..eq] : item;
            var value = eq >= 0 ? item[(eq + 1)..] : "";

            if (key == "fp" && !sawFp)
            {
                sawFp = true;
                if (value.Length != 64 || !value.All(Uri.IsHexDigit))
                {
                    error = "The certificate fingerprint in the link is malformed";
                    return false;
                }
                fingerprint = value.ToLowerInvariant();
            }
            else if (key == "pw" && !sawPw)
            {
                sawPw = true;
                try
                {
                    password = Uri.UnescapeDataString(value);
                }
                catch (UriFormatException)
                {
                    error = "The password in the link isn't valid";
                    return false;
                }
                if (password.Length == 0) password = null;
            }
        }

        link = new AuraInviteLink(host, port, channelId, fingerprint, password);
        return true;
    }
}
