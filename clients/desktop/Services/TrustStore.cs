using System;
using System.Collections.Generic;

namespace Aura.Desktop.Services;

/// <summary>
/// How far a member's key has been checked. Drives every trust badge.
///
/// The design handoff's trust sheet renders exactly these, and nothing in
/// between: "three states only: verified / TOFU · not compared / key changed
/// today", plus the outcome of the sheet's Block action.
///
/// Owned here rather than in the view models because <see cref="TrustStore"/>
/// is what decides it; the rail and roster only display the result.
/// </summary>
public enum TrustState
{
    /// <summary>Seen, recorded, never compared out of band. The honest default.</summary>
    Tofu,

    /// <summary>Fingerprint compared out of band and accepted by the user.</summary>
    Verified,

    /// <summary>
    /// This user presented a different key than the one first recorded for them.
    /// A reinstall looks identical to a takeover from here, which is precisely
    /// why it is surfaced rather than resolved automatically.
    /// </summary>
    KeyChanged,

    /// <summary>Rejected from the trust sheet.</summary>
    Blocked,
}

/// <summary>
/// Remembers which identity keys the user has verified, blocked, or seen before,
/// and turns that history into a <see cref="TrustState"/> for any member.
///
/// Everything is keyed by the Ed25519 public key, not by user id or display
/// name. A user who rotates keys — or an impostor who takes over a name — starts
/// from <see cref="TrustState.Tofu"/> rather than inheriting trust, and a block
/// cannot be shaken off by reconnecting under a new session.
///
/// The keys themselves come from <c>MlsWrapper.GroupMembers</c>, i.e. the
/// authenticated MLS ratchet tree. They are never taken from the server's user
/// list, which carries no key material precisely so it cannot be used this way.
/// </summary>
public class TrustStore
{
    private readonly AppSettings _settings;
    private readonly Action _save;
    private readonly Func<DateTimeOffset> _clock;

    /// <param name="settings">Backing store; its dictionaries are mutated in place.</param>
    /// <param name="save">Called after every mutation. Verification is rare and
    /// losing one to a crash would mean asking the user to redo an out-of-band
    /// comparison, so this writes through rather than batching.</param>
    /// <param name="clock">Injectable for tests.</param>
    public TrustStore(AppSettings settings, Action save, Func<DateTimeOffset>? clock = null)
    {
        _settings = settings;
        _save = save;
        _clock = clock ?? (() => DateTimeOffset.UtcNow);
    }

    /// <summary>Lowercase hex, the form every dictionary here is keyed by.</summary>
    public static string KeyId(IEnumerable<byte> publicKey) => Convert.ToHexString(
        publicKey is byte[] arr ? arr : System.Linq.Enumerable.ToArray(publicKey)).ToLowerInvariant();

    /// <summary>
    /// Record that <paramref name="userUuid"/> is presenting <paramref name="publicKey"/>.
    ///
    /// The first key seen for a user becomes their baseline. Later keys are
    /// deliberately <em>not</em> written over it — the mismatch is the signal,
    /// and silently re-baselining would erase it. Call this for every member
    /// whenever group membership is refreshed.
    /// </summary>
    public void Observe(string userUuid, byte[] publicKey)
    {
        if (string.IsNullOrEmpty(userUuid) || publicKey.Length == 0) return;

        var id = KeyId(publicKey);

        if (!_settings.KnownUserKeys.TryGetValue(userUuid, out var baseline))
        {
            _settings.KnownUserKeys[userUuid] = id;
            _save();
            return;
        }

        if (baseline == id)
        {
            // Back to a key we already knew — clear any stale changed-marker so
            // the badge does not linger after the user resolves it.
            if (_settings.KeyChangedAt.Remove(userUuid)) _save();
            return;
        }

        // Stamp the first time we notice the disagreement, and only then, so
        // "key changed today" keeps meaning the day it actually changed.
        if (!_settings.KeyChangedAt.ContainsKey(userUuid))
        {
            _settings.KeyChangedAt[userUuid] = _clock();
            _save();
        }
    }

    /// <summary>Current trust state for a member.</summary>
    public TrustState Evaluate(string userUuid, byte[] publicKey)
    {
        if (publicKey.Length == 0) return TrustState.Tofu;

        var id = KeyId(publicKey);

        // Block outranks everything: a blocked key stays blocked even if it was
        // verified earlier, because blocking is the later, more deliberate act.
        if (_settings.BlockedKeys.ContainsKey(id)) return TrustState.Blocked;
        if (_settings.VerifiedKeys.ContainsKey(id)) return TrustState.Verified;

        if (_settings.KnownUserKeys.TryGetValue(userUuid, out var baseline) && baseline != id)
        {
            return TrustState.KeyChanged;
        }

        return TrustState.Tofu;
    }

    /// <summary>
    /// The user compared fingerprints out of band and accepted this key.
    ///
    /// Also re-baselines the user, so a verified rotation stops reporting as
    /// changed — the user has just told us this key is the right one.
    /// </summary>
    public void MarkVerified(string userUuid, byte[] publicKey)
    {
        if (publicKey.Length == 0) return;

        var id = KeyId(publicKey);
        _settings.VerifiedKeys[id] = _clock();
        _settings.BlockedKeys.Remove(id);

        if (!string.IsNullOrEmpty(userUuid))
        {
            _settings.KnownUserKeys[userUuid] = id;
            _settings.KeyChangedAt.Remove(userUuid);
        }

        _save();
    }

    /// <summary>
    /// The user rejected this key from the trust sheet. Blocks the key, not the
    /// person: if they later present the key we originally trusted, that one is
    /// still fine.
    /// </summary>
    public void Block(byte[] publicKey)
    {
        if (publicKey.Length == 0) return;

        var id = KeyId(publicKey);
        _settings.BlockedKeys[id] = _clock();
        _settings.VerifiedKeys.Remove(id);
        _save();
    }

    /// <summary>Undo a block. The key drops back to whatever it would otherwise be.</summary>
    public void Unblock(byte[] publicKey)
    {
        if (publicKey.Length == 0) return;
        if (_settings.BlockedKeys.Remove(KeyId(publicKey))) _save();
    }

    /// <summary>When this user's key was first seen to change, if it has.</summary>
    public DateTimeOffset? KeyChangedAt(string userUuid) =>
        _settings.KeyChangedAt.TryGetValue(userUuid, out var at) ? at : null;

    /// <summary>When this key was verified, if it was.</summary>
    public DateTimeOffset? VerifiedAt(byte[] publicKey) =>
        publicKey.Length > 0 && _settings.VerifiedKeys.TryGetValue(KeyId(publicKey), out var at)
            ? at
            : null;
}
