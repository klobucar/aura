//! MLS Encryption Client for DAVE Protocol
//!
//! Provides MLS group management for E2EE voice and text channels, using
//! OpenMLS with post-quantum signatures (ML-DSA-44) and hybrid post-quantum
//! key exchange (X-Wing: X25519 + ML-KEM-768); see [`CIPHERSUITE`].
//!
//! Each member signs MLS messages with an ML-DSA-44 key derived from their
//! long-term identity ([`aura_identity::IdentitySecret`]), so it is the same
//! across connections. Their MLS credential is an
//! [`aura_identity::MemberCredential`]: the identity's hybrid Ed25519 +
//! ML-DSA-65 signature over that MLS key. Every leaf that enters a group, by
//! Welcome, Add or Commit, has its credential checked against its own
//! signature key before we act on it.

use aura_identity::{IdentitySecret, MemberCredential, MLS_SIGNATURE_SCHEME_MLDSA44};
use openmls::prelude::tls_codec::{Deserialize, Serialize};
use openmls::prelude::*;
use openmls_rust_crypto::OpenMlsRustCrypto;
use std::collections::{HashMap, HashSet};

/// DAVE key derivation label
pub const DAVE_KEY_LABEL: &str = "aura-dave-key";

/// Key length for DAVE encryption (XChaCha20-Poly1305)
pub const DAVE_KEY_LEN: usize = 32;

/// How many epochs of exported sender keys to retain per group.
///
/// MLS only lets us export secrets for the epoch the group is currently in, so
/// a message that was in flight when the epoch advanced would otherwise become
/// undecryptable. We snapshot the exported keys just before every epoch change
/// and keep this many epochs' worth around. The audio path does the equivalent
/// in `AudioReceiver::update_sender_key`.
pub const EPOCH_KEY_RETENTION: usize = 4;

/// Ciphersuite for all MLS operations.
///
/// `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (draft-ietf-mls-pq-ciphersuites):
/// - **KEM:** MLKEM768-X25519, i.e. X-Wing (draft-connolly-cfrg-xwing-kem-06),
///   a hybrid of X25519 and ML-KEM-768. Group secrets stay confidential unless
///   *both* are broken, so recorded handshakes resist "harvest now, decrypt
///   later".
/// - **AEAD:** ChaCha20-Poly1305 (256-bit key).
/// - **Hash/KDF:** SHA-384 / HKDF-SHA384.
/// - **Signatures:** ML-DSA-44 (FIPS 204). MLS has no hybrid signature
///   scheme; the classical half lives in each member's credential, which their
///   hybrid Ed25519 + ML-DSA-65 identity signs. This is the only OpenMLS suite
///   that pairs ML-DSA with the hybrid KEM.
///
/// The "128" is the draft's security-category label, not the AEAD key size.
/// The code point (0x0052) is provisional until IANA assigns one; adopting
/// the final value is another all-clients-at-once upgrade, as is moving to a
/// future suite with a stronger signature (only the MLS key changes; the
/// identity does not).
///
/// Every peer must use the same suite: a group only admits KeyPackages of its
/// own suite, and [`MlsClient`] refuses anything else rather than downgrade.
pub const CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;

/// Leaf capabilities advertised in our KeyPackages and leaves: exactly
/// [`CIPHERSUITE`], since that is the only suite we will join or admit.
/// (OpenMLS's default list advertises classical suites we refuse.)
fn leaf_capabilities() -> Capabilities {
    Capabilities::new(None, Some(&[CIPHERSUITE]), None, None, None)
}

/// Check that a leaf's `credential` is an Aura [`MemberCredential`] whose
/// identity certified `signature_key`, the key the leaf signs with.
///
/// OpenMLS verifies that each leaf's signatures match its signature key, but
/// not who that key belongs to; that is the application's job. Without this
/// check, anyone could join with a credential naming someone else.
fn verify_leaf_credential(
    credential: &Credential,
    signature_key: &[u8],
) -> Result<MemberCredential, MlsError> {
    if credential.credential_type() != CredentialType::Basic {
        return Err(MlsError::InvalidCredential("not a basic credential".into()));
    }
    let member = MemberCredential::from_bytes(credential.serialized_content())
        .map_err(|e| MlsError::InvalidCredential(e.to_string()))?;
    member
        .verify(MLS_SIGNATURE_SCHEME_MLDSA44, signature_key)
        .map_err(|e| MlsError::InvalidCredential(format!("{e} (user {})", member.user_id())))?;
    Ok(member)
}

/// Run `f` inside `span` (an `aura::core` telemetry span) and record whether
/// it succeeded. Spans carry an operation name and an outcome only; see
/// `telemetry_ffi`. A no-op when telemetry is off.
pub(crate) fn traced<T, E>(span: tracing::Span, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let result = span.in_scope(f);
    span.record("outcome", if result.is_ok() { "ok" } else { "error" });
    result
}

#[derive(Debug, thiserror::Error)]
pub enum MlsError {
    #[error("OpenMLS error: {0}")]
    OpenMls(String),
    /// A peer offered MLS material for a different ciphersuite — typically a
    /// client that predates the post-quantum suite. Never silently accepted,
    /// since that would downgrade the group's key exchange.
    #[error("Ciphersuite mismatch: expected {expected:?}, peer used {actual:?} (peer client may need updating)")]
    CiphersuiteMismatch {
        expected: Ciphersuite,
        actual: Ciphersuite,
    },
    /// A leaf's credential is not a valid Aura member credential for that
    /// leaf's signature key: malformed, or its identity did not certify the key.
    #[error("Invalid member credential: {0}")]
    InvalidCredential(String),
    #[error("Group not found: {0}")]
    GroupNotFound(String),
    #[error("No credential available")]
    NoCredential,
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("No retained key for epoch {epoch} (current epoch is {current})")]
    EpochUnavailable { epoch: u64, current: u64 },
}

/// One member of an MLS group, as the ratchet tree sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    /// The member's user id (a UUID), from their verified credential.
    pub identity: String,
    /// The member's long-term identity public key (hybrid Ed25519 +
    /// ML-DSA-65, 1,984 bytes), verified to have certified the MLS key they
    /// sign with. Stable across connections: trust decisions key on this.
    pub identity_key: Vec<u8>,
    /// True for the local user's own leaf, so callers can skip verifying
    /// themselves against themselves.
    pub is_self: bool,
}

/// Stored identity for the MLS client
struct ClientIdentity {
    credential_with_key: CredentialWithKey,
    signer: openmls_basic_credential::SignatureKeyPair,
}

/// MLS Client managing identity and groups
pub struct MlsClient {
    /// OpenMLS crypto provider
    provider: OpenMlsRustCrypto,
    /// Our identity (credential + signer)
    identity: Option<ClientIdentity>,
    /// Active MLS groups by group ID
    groups: HashMap<Vec<u8>, MlsGroup>,
    /// Exported per-sender DAVE keys, retained across epoch changes.
    /// Keyed by (group_id, epoch), then by sender session ID.
    exported_keys: HashMap<(Vec<u8>, u64), HashMap<u32, [u8; DAVE_KEY_LEN]>>,
    /// Sender session IDs we have derived a key for at least once, per group.
    /// Used to re-derive the whole set immediately before an epoch change so
    /// that late-arriving messages from any known sender stay decryptable.
    known_senders: HashMap<Vec<u8>, HashSet<u32>>,
}

impl MlsClient {
    /// Create an MLS client for `user_id` (the user's UUID).
    ///
    /// It signs with the ML-DSA-44 key derived from `identity`, the same key
    /// on every connection, and presents a credential in which `identity`
    /// certifies that key.
    pub fn new(user_id: &str, identity: &IdentitySecret) -> Result<Self, MlsError> {
        let provider = OpenMlsRustCrypto::default();

        let scheme = CIPHERSUITE.signature_algorithm();
        debug_assert_eq!(scheme as u16, MLS_SIGNATURE_SCHEME_MLDSA44);
        let mls_key = identity.mls_signing_key(0);
        // OpenMLS's basic credential signs ML-DSA from the FIPS 204 seed.
        let signer = openmls_basic_credential::SignatureKeyPair::from_raw(
            scheme,
            mls_key.seed.to_vec(),
            mls_key.public_key.clone(),
        );

        // Store the keypair in the provider's key store
        signer
            .store(provider.storage())
            .map_err(|e| MlsError::OpenMls(format!("KeyStore error: {:?}", e)))?;

        let member = MemberCredential::issue(
            identity,
            user_id,
            MLS_SIGNATURE_SCHEME_MLDSA44,
            &mls_key.public_key,
        )
        .map_err(|e| MlsError::InvalidCredential(e.to_string()))?;
        let credential = BasicCredential::new(member.to_bytes());
        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signer.public().into(),
        };

        let identity = ClientIdentity {
            credential_with_key,
            signer,
        };

        Ok(Self {
            provider,
            identity: Some(identity),
            groups: HashMap::new(),
            exported_keys: HashMap::new(),
            known_senders: HashMap::new(),
        })
    }

    /// Generate a KeyPackage for joining groups
    pub fn generate_key_package(&self) -> Result<KeyPackageBundle, MlsError> {
        traced(
            tracing::info_span!(target: "aura::core", "mls.generate_key_package", outcome = tracing::field::Empty),
            || self.generate_key_package_inner(),
        )
    }

    fn generate_key_package_inner(&self) -> Result<KeyPackageBundle, MlsError> {
        let identity = self.identity.as_ref().ok_or(MlsError::NoCredential)?;

        let key_package_bundle = KeyPackage::builder()
            .leaf_node_capabilities(leaf_capabilities())
            .build(
                CIPHERSUITE,
                &self.provider,
                &identity.signer,
                identity.credential_with_key.clone(),
            )
            .map_err(|e| MlsError::OpenMls(format!("KeyPackage build error: {:?}", e)))?;

        Ok(key_package_bundle)
    }

    /// Create a new MLS group (as the creator/admin)
    pub fn create_group(&mut self, group_id: &[u8]) -> Result<(), MlsError> {
        traced(
            tracing::info_span!(target: "aura::core", "mls.create_group", outcome = tracing::field::Empty),
            || self.create_group_inner(group_id),
        )
    }

    fn create_group_inner(&mut self, group_id: &[u8]) -> Result<(), MlsError> {
        let identity = self.identity.as_ref().ok_or(MlsError::NoCredential)?;

        let group_config = MlsGroupCreateConfig::builder()
            .use_ratchet_tree_extension(true)
            .ciphersuite(CIPHERSUITE)
            .capabilities(leaf_capabilities())
            .build();

        let group = MlsGroup::new_with_group_id(
            &self.provider,
            &identity.signer,
            &group_config,
            GroupId::from_slice(group_id),
            identity.credential_with_key.clone(),
        )
        .map_err(|e| MlsError::OpenMls(format!("Group creation error: {:?}", e)))?;

        // A freshly created group restarts at epoch 0. Any keys retained from a
        // previous membership of the same channel belong to a different group
        // instance and would decrypt to garbage.
        self.forget_group_keys(group_id);
        self.groups.insert(group_id.to_vec(), group);
        Ok(())
    }

    /// Derive a per-sender DAVE key at the group's current epoch, without
    /// touching the retention cache.
    fn derive_sender_key(
        &self,
        group_id: &[u8],
        sender_id: u32,
    ) -> Result<([u8; DAVE_KEY_LEN], u64), MlsError> {
        let group = self
            .groups
            .get(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        let epoch = group.epoch().as_u64();

        // Context = sender_id (little-endian, per DAVE spec)
        let context = sender_id.to_le_bytes();

        let secret = group
            .export_secret(
                self.provider.crypto(),
                DAVE_KEY_LABEL,
                &context,
                DAVE_KEY_LEN,
            )
            .map_err(|e| MlsError::OpenMls(format!("Export secret error: {:?}", e)))?;

        let mut key = [0u8; DAVE_KEY_LEN];
        key.copy_from_slice(&secret);

        Ok((key, epoch))
    }

    /// Export a per-sender DAVE encryption key for the current epoch
    ///
    /// Each sender in the group derives a unique key by using their sender_id
    /// as context in the MLS key export. This prevents impersonation attacks
    /// where group members could forge packets for other members.
    ///
    /// The result is retained so it stays available after the epoch advances;
    /// see [`export_sender_key_at_epoch`](Self::export_sender_key_at_epoch).
    ///
    /// # Arguments
    /// * `group_id` - The MLS group identifier
    /// * `sender_id` - The unique session ID of the sender (little-endian in context)
    pub fn export_sender_key(
        &mut self,
        group_id: &[u8],
        sender_id: u32,
    ) -> Result<([u8; DAVE_KEY_LEN], u64), MlsError> {
        let (key, epoch) = self.derive_sender_key(group_id, sender_id)?;

        self.exported_keys
            .entry((group_id.to_vec(), epoch))
            .or_default()
            .insert(sender_id, key);
        self.known_senders
            .entry(group_id.to_vec())
            .or_default()
            .insert(sender_id);
        self.prune_epoch_keys(group_id, epoch);

        Ok((key, epoch))
    }

    /// Export a sender's DAVE key for a *specific* epoch.
    ///
    /// Messages carry the epoch they were encrypted under. Deriving at the
    /// current epoch instead would silently fail to decrypt anything that was
    /// in flight across a membership change, so receivers must pass the
    /// epoch off the wire.
    pub fn export_sender_key_at_epoch(
        &mut self,
        group_id: &[u8],
        sender_id: u32,
        epoch: u64,
    ) -> Result<[u8; DAVE_KEY_LEN], MlsError> {
        let current = self.epoch(group_id)?;

        if epoch == current {
            return self.export_sender_key(group_id, sender_id).map(|(k, _)| k);
        }

        self.exported_keys
            .get(&(group_id.to_vec(), epoch))
            .and_then(|senders| senders.get(&sender_id))
            .copied()
            .ok_or(MlsError::EpochUnavailable { epoch, current })
    }

    /// Pre-derive and retain a sender's key at the current epoch.
    ///
    /// Call this when a member becomes visible in a channel. Without it, a
    /// sender whose *first* message races an epoch change would have no
    /// retained key to fall back on, because nothing had derived one yet.
    pub fn register_sender(&mut self, group_id: &[u8], sender_id: u32) -> Result<(), MlsError> {
        self.export_sender_key(group_id, sender_id).map(|_| ())
    }

    /// Re-derive keys for every known sender at the group's current epoch.
    ///
    /// Must run *before* a commit is merged: once the epoch advances, MLS can
    /// no longer export the old epoch's secrets.
    fn snapshot_epoch_keys(&mut self, group_id: &[u8]) {
        let Some(senders) = self.known_senders.get(group_id) else {
            return;
        };
        let senders: Vec<u32> = senders.iter().copied().collect();

        let mut derived = Vec::with_capacity(senders.len());
        let mut epoch = None;
        for sender_id in senders {
            match self.derive_sender_key(group_id, sender_id) {
                Ok((key, e)) => {
                    epoch = Some(e);
                    derived.push((sender_id, key));
                }
                Err(_) => return,
            }
        }

        if let Some(epoch) = epoch {
            let bucket = self
                .exported_keys
                .entry((group_id.to_vec(), epoch))
                .or_default();
            for (sender_id, key) in derived {
                bucket.insert(sender_id, key);
            }
            self.prune_epoch_keys(group_id, epoch);
        }
    }

    /// Drop all but the newest [`EPOCH_KEY_RETENTION`] epochs for a group.
    fn prune_epoch_keys(&mut self, group_id: &[u8], current_epoch: u64) {
        let mut epochs: Vec<u64> = self
            .exported_keys
            .keys()
            .filter(|(gid, _)| gid == group_id)
            .map(|(_, epoch)| *epoch)
            .collect();

        if epochs.len() <= EPOCH_KEY_RETENTION {
            return;
        }

        epochs.sort_unstable();
        let excess = epochs.len() - EPOCH_KEY_RETENTION;
        for epoch in epochs.into_iter().take(excess) {
            // Never evict the epoch we are actively using.
            if epoch != current_epoch {
                self.exported_keys.remove(&(group_id.to_vec(), epoch));
            }
        }
    }

    /// Forget all retained keys and sender bookkeeping for a group.
    fn forget_group_keys(&mut self, group_id: &[u8]) {
        self.exported_keys.retain(|(gid, _), _| gid != group_id);
        self.known_senders.remove(group_id);
    }

    /// Get the current epoch for a group
    pub fn epoch(&self, group_id: &[u8]) -> Result<u64, MlsError> {
        let group = self
            .groups
            .get(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        Ok(group.epoch().as_u64())
    }

    /// Check if we are a member of a group
    pub fn is_member(&self, group_id: &[u8]) -> bool {
        self.groups.contains_key(group_id)
    }

    /// Every member of a group, with the identity and signature key MLS
    /// itself vouches for.
    ///
    /// This is the trustworthy source for identity verification: the keys come
    /// out of the ratchet tree, which is authenticated, rather than from a
    /// server-supplied user list the server could lie about. `UserInfo` on the
    /// wire deliberately carries no key material for exactly this reason.
    ///
    /// Each member's user id and identity key come from their credential,
    /// re-verified here against their leaf's signature key, and pair up as
    /// input to [`crate::verification::pairwise_fingerprint`].
    pub fn group_members(&self, group_id: &[u8]) -> Result<Vec<GroupMember>, MlsError> {
        let group = self
            .groups
            .get(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        let own_index = group.own_leaf_index();

        group
            .members()
            .map(|m| {
                let member = verify_leaf_credential(&m.credential, &m.signature_key)?;
                Ok(GroupMember {
                    identity: member.user_id().to_owned(),
                    identity_key: member.identity().as_bytes().to_vec(),
                    is_self: m.index == own_index,
                })
            })
            .collect()
    }

    // ============================================================================
    // MLS Group Management - Full Implementation
    // ============================================================================

    /// Process a Welcome message to join a group
    /// Returns the group_id of the joined group
    pub fn process_welcome(&mut self, welcome_bytes: &[u8]) -> Result<Vec<u8>, MlsError> {
        traced(
            tracing::info_span!(target: "aura::core", "mls.process_welcome", outcome = tracing::field::Empty),
            || self.process_welcome_inner(welcome_bytes),
        )
    }

    fn process_welcome_inner(&mut self, welcome_bytes: &[u8]) -> Result<Vec<u8>, MlsError> {
        // Deserialize the Welcome message using tls_codec
        let mut welcome_slice = welcome_bytes;
        let mls_message_in = MlsMessageIn::tls_deserialize(&mut welcome_slice)
            .map_err(|e| MlsError::Serialization(format!("Welcome deserialize error: {:?}", e)))?;

        // Extract the body and get the Welcome variant
        let welcome = match mls_message_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(MlsError::OpenMls("Expected Welcome message".into())),
        };

        // Build group configuration for joining
        let group_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        // Process the Welcome
        let staged = StagedWelcome::new_from_welcome(
            &self.provider,
            &group_config,
            welcome,
            None, // No ratchet tree provided separately
        )
        .map_err(|e| MlsError::OpenMls(format!("Welcome processing error: {:?}", e)))?;

        // Whoever built this Welcome chose the group's ciphersuite, and
        // OpenMLS will join any suite the provider supports. Refuse anything
        // but ours so a stale or hostile peer cannot move us onto
        // classical-only key exchange. Checked before `into_group` so a
        // rejected group never reaches the provider's storage.
        let suite = staged.group_context().ciphersuite();
        if suite != CIPHERSUITE {
            return Err(MlsError::CiphersuiteMismatch {
                expected: CIPHERSUITE,
                actual: suite,
            });
        }

        // Every existing member must carry a valid credential for the key they
        // sign with; otherwise the group could contain an impostor we'd then
        // show as a named, verifiable member.
        for member in staged.members() {
            verify_leaf_credential(&member.credential, &member.signature_key)?;
        }

        let group = staged.into_group(&self.provider).map_err(|e| {
            MlsError::OpenMls(format!("Group creation from welcome error: {:?}", e))
        })?;

        let group_id = group.group_id().as_slice().to_vec();
        // Same reasoning as `create_group`: keys retained from a prior
        // membership of this channel are for a different group instance.
        self.forget_group_keys(&group_id);
        self.groups.insert(group_id.clone(), group);

        Ok(group_id)
    }

    /// Add a member to a group (returns serialized Commit and Welcome messages)
    pub fn add_member(
        &mut self,
        group_id: &[u8],
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), MlsError> {
        traced(
            tracing::info_span!(target: "aura::core", "mls.add_member", outcome = tracing::field::Empty),
            || self.add_member_inner(group_id, key_package_bytes),
        )
    }

    fn add_member_inner(
        &mut self,
        group_id: &[u8],
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), MlsError> {
        // Retain the outgoing epoch's keys before the commit is merged.
        self.snapshot_epoch_keys(group_id);

        let identity = self.identity.as_ref().ok_or(MlsError::NoCredential)?;

        let group = self
            .groups
            .get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        // Deserialize the KeyPackage
        let mut kp_slice = key_package_bytes;
        let key_package_in = KeyPackageIn::tls_deserialize(&mut kp_slice).map_err(|e| {
            MlsError::Serialization(format!("KeyPackage deserialize error: {:?}", e))
        })?;

        // Validate the KeyPackage
        let key_package = key_package_in
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::OpenMls(format!("KeyPackage validation error: {:?}", e)))?;

        // OpenMLS would also refuse this (as an opaque proposal-validation
        // error); checking here names the cause, which is almost always a
        // joiner running a client from before the post-quantum suite.
        if key_package.ciphersuite() != CIPHERSUITE {
            return Err(MlsError::CiphersuiteMismatch {
                expected: CIPHERSUITE,
                actual: key_package.ciphersuite(),
            });
        }

        // The joiner's identity must have certified the key they'll sign with.
        let leaf = key_package.leaf_node();
        verify_leaf_credential(leaf.credential(), leaf.signature_key().as_slice())?;

        // Add the member and get (commit, welcome, group_info)
        let (commit_out, welcome, _group_info) = group
            .add_members(&self.provider, &identity.signer, &[key_package])
            .map_err(|e| MlsError::OpenMls(format!("Add member error: {:?}", e)))?;

        // Merge the pending commit
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("Merge commit error: {:?}", e)))?;

        // Serialize the commit
        let commit_bytes = commit_out
            .tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Commit serialize error: {:?}", e)))?;

        // Serialize the welcome
        let welcome_bytes = welcome
            .tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Welcome serialize error: {:?}", e)))?;

        Ok((commit_bytes, welcome_bytes))
    }

    /// Process a Commit message from another member
    /// Returns the new epoch number
    pub fn process_commit(
        &mut self,
        group_id: &[u8],
        commit_bytes: &[u8],
    ) -> Result<u64, MlsError> {
        traced(
            tracing::info_span!(target: "aura::core", "mls.process_commit", outcome = tracing::field::Empty),
            || self.process_commit_inner(group_id, commit_bytes),
        )
    }

    fn process_commit_inner(
        &mut self,
        group_id: &[u8],
        commit_bytes: &[u8],
    ) -> Result<u64, MlsError> {
        // Retain the outgoing epoch's keys before the commit is merged.
        self.snapshot_epoch_keys(group_id);

        let group = self
            .groups
            .get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        // Deserialize the commit message
        let mut commit_slice = commit_bytes;
        let message_in = MlsMessageIn::tls_deserialize(&mut commit_slice)
            .map_err(|e| MlsError::Serialization(format!("Commit deserialize error: {:?}", e)))?;

        // Process the incoming message
        let protocol_message = message_in
            .try_into_protocol_message()
            .map_err(|e| MlsError::OpenMls(format!("Protocol message error: {:?}", e)))?;

        let processed = group
            .process_message(&self.provider, protocol_message)
            .map_err(|e| MlsError::OpenMls(format!("Process message error: {:?}", e)))?;

        // Handle the processed message content
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                // Check every leaf this commit introduces or replaces (the
                // committer's update-path leaf, added members, updated leaves)
                // before merging, so a rejected commit leaves the group as it was.
                if let Some(leaf) = staged_commit.update_path_leaf_node() {
                    verify_leaf_credential(leaf.credential(), leaf.signature_key().as_slice())?;
                }
                for queued in staged_commit.queued_proposals() {
                    let leaf = match queued.proposal() {
                        Proposal::Add(add) => add.key_package().leaf_node(),
                        Proposal::Update(update) => update.leaf_node(),
                        _ => continue,
                    };
                    verify_leaf_credential(leaf.credential(), leaf.signature_key().as_slice())?;
                }
                group
                    .merge_staged_commit(&self.provider, *staged_commit)
                    .map_err(|e| {
                        MlsError::OpenMls(format!("Merge staged commit error: {:?}", e))
                    })?;
            }
            _ => {
                return Err(MlsError::OpenMls("Expected Commit message".into()));
            }
        }

        Ok(group.epoch().as_u64())
    }

    /// Issue an empty (self-update) commit to advance the group's epoch.
    ///
    /// This is the PFS ratchet: it rotates the group secret without any
    /// membership change, so keys derived at the old epoch cannot decrypt
    /// anything sent after it. Returns the serialized Commit to broadcast.
    pub fn self_update(&mut self, group_id: &[u8]) -> Result<Vec<u8>, MlsError> {
        traced(
            tracing::info_span!(target: "aura::core", "mls.self_update", outcome = tracing::field::Empty),
            || self.self_update_inner(group_id),
        )
    }

    fn self_update_inner(&mut self, group_id: &[u8]) -> Result<Vec<u8>, MlsError> {
        // Retain the outgoing epoch's keys before the commit is merged.
        self.snapshot_epoch_keys(group_id);

        let identity = self.identity.as_ref().ok_or(MlsError::NoCredential)?;

        let group = self
            .groups
            .get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        let bundle = group
            .self_update(
                &self.provider,
                &identity.signer,
                LeafNodeParameters::default(),
            )
            .map_err(|e| MlsError::OpenMls(format!("Self update error: {:?}", e)))?;

        let commit_bytes = bundle
            .commit()
            .tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Commit serialize error: {:?}", e)))?;

        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("Merge commit error: {:?}", e)))?;

        Ok(commit_bytes)
    }

    /// Leave a group (self-remove) by proposing a self-removal
    /// Returns the serialized proposal message to broadcast
    pub fn leave_group(&mut self, group_id: &[u8]) -> Result<Vec<u8>, MlsError> {
        let identity = self.identity.as_ref().ok_or(MlsError::NoCredential)?;

        let group = self
            .groups
            .get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        // Create a self-remove proposal
        let leave_proposal = group
            .leave_group(&self.provider, &identity.signer)
            .map_err(|e| MlsError::OpenMls(format!("Leave group error: {:?}", e)))?;

        // Serialize the proposal
        let proposal_bytes = leave_proposal
            .tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Proposal serialize error: {:?}", e)))?;

        // Remove from our local groups
        self.groups.remove(group_id);
        self.forget_group_keys(group_id);

        Ok(proposal_bytes)
    }

    /// Remove a member from the group (admin operation)
    /// Returns the serialized Commit message to broadcast
    pub fn remove_member(
        &mut self,
        group_id: &[u8],
        member_index: u32,
    ) -> Result<Vec<u8>, MlsError> {
        // Retain the outgoing epoch's keys before the commit is merged.
        self.snapshot_epoch_keys(group_id);

        let identity = self.identity.as_ref().ok_or(MlsError::NoCredential)?;

        let group = self
            .groups
            .get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;

        // Create leaf node reference for the member to remove
        let leaf_index = LeafNodeIndex::new(member_index);

        // Remove the member
        let (commit_out, _welcome, _group_info) = group
            .remove_members(&self.provider, &identity.signer, &[leaf_index])
            .map_err(|e| MlsError::OpenMls(format!("Remove member error: {:?}", e)))?;

        // Merge the pending commit
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("Merge commit error: {:?}", e)))?;

        // Serialize the commit
        let commit_bytes = commit_out
            .tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Commit serialize error: {:?}", e)))?;

        Ok(commit_bytes)
    }

    /// Get serialized KeyPackage for sharing with others
    pub fn get_key_package_bytes(&self) -> Result<Vec<u8>, MlsError> {
        let kp_bundle = self.generate_key_package()?;
        let bytes = kp_bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("KeyPackage serialize error: {:?}", e)))?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client with a fresh identity; most tests only need distinct members.
    fn new_client(user_id: &str) -> Result<MlsClient, MlsError> {
        MlsClient::new(user_id, &IdentitySecret::generate())
    }

    #[test]
    fn test_client_creation() {
        let client = new_client("alice").expect("Failed to create client");
        assert!(client.groups.is_empty());
        assert!(client.identity.is_some());
    }

    #[test]
    fn test_key_package_generation() {
        let client = new_client("alice").expect("Failed to create client");
        let kp_bundle = client
            .generate_key_package()
            .expect("Failed to generate KeyPackage");
        // KeyPackage generated successfully
        assert!(kp_bundle.key_package().ciphersuite() == CIPHERSUITE);
    }

    #[test]
    fn test_group_creation() {
        let mut client = new_client("alice").expect("Failed to create client");
        let group_id = b"test-voice-channel-1";

        client
            .create_group(group_id)
            .expect("Failed to create group");
        assert!(client.is_member(group_id));

        let epoch = client.epoch(group_id).expect("Failed to get epoch");
        assert_eq!(epoch, 0); // Initial epoch is 0
    }

    #[test]
    fn test_dave_key_export() {
        let mut client = new_client("alice").expect("Failed to create client");
        let group_id = b"test-voice-channel-1";

        client
            .create_group(group_id)
            .expect("Failed to create group");

        let (key, epoch) = client
            .export_sender_key(group_id, 1)
            .expect("Failed to export key");
        assert_eq!(key.len(), DAVE_KEY_LEN);
        assert_eq!(epoch, 0);
    }

    #[test]
    fn test_add_member_and_welcome() {
        // Alice creates a group
        let mut alice = new_client("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-2";
        alice
            .create_group(group_id)
            .expect("Failed to create group");

        // Bob generates a KeyPackage
        let bob = new_client("bob").expect("Failed to create bob");
        let bob_kp_bytes = bob
            .get_key_package_bytes()
            .expect("Failed to get bob's KeyPackage");

        // Alice adds Bob
        let (commit_bytes, welcome_bytes) = alice
            .add_member(group_id, &bob_kp_bytes)
            .expect("Failed to add member");

        assert!(!commit_bytes.is_empty());
        assert!(!welcome_bytes.is_empty());

        // Alice's epoch should now be 1
        assert_eq!(alice.epoch(group_id).unwrap(), 1);
    }

    #[test]
    fn test_process_welcome() {
        // Alice creates a group
        let mut alice = new_client("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-3";
        alice
            .create_group(group_id)
            .expect("Failed to create group");

        // Bob generates a KeyPackage
        let mut bob = new_client("bob").expect("Failed to create bob");
        let bob_kp_bytes = bob
            .get_key_package_bytes()
            .expect("Failed to get bob's KeyPackage");

        // Alice adds Bob
        let (_commit_bytes, welcome_bytes) = alice
            .add_member(group_id, &bob_kp_bytes)
            .expect("Failed to add member");

        // Bob processes the Welcome and joins
        let joined_group_id = bob
            .process_welcome(&welcome_bytes)
            .expect("Failed to process welcome");

        assert_eq!(joined_group_id, group_id);
        assert!(bob.is_member(&joined_group_id));

        // Bob should be at same epoch as Alice
        assert_eq!(
            bob.epoch(&joined_group_id).unwrap(),
            alice.epoch(group_id).unwrap()
        );
    }

    #[test]
    fn test_dave_keys_match_after_join() {
        // Alice creates a group
        let mut alice = new_client("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-4";
        alice
            .create_group(group_id)
            .expect("Failed to create group");

        // Bob generates a KeyPackage and joins
        let mut bob = new_client("bob").expect("Failed to create bob");
        let bob_kp_bytes = bob
            .get_key_package_bytes()
            .expect("Failed to get bob's KeyPackage");

        let (_commit, welcome) = alice
            .add_member(group_id, &bob_kp_bytes)
            .expect("Failed to add member");

        bob.process_welcome(&welcome)
            .expect("Failed to process welcome");

        // Both should derive the same DAVE key when using the same sender_id
        let (alice_key, alice_epoch) = alice
            .export_sender_key(group_id, 0)
            .expect("Alice key export");
        let (bob_key, bob_epoch) = bob.export_sender_key(group_id, 0).expect("Bob key export");

        assert_eq!(alice_epoch, bob_epoch);
        assert_eq!(alice_key, bob_key);
    }

    /// A message encrypted at epoch N must stay decryptable after the group
    /// moves to N+1 — otherwise chat silently breaks whenever anyone joins.
    #[test]
    fn test_sender_key_survives_epoch_advance() {
        let mut alice = new_client("alice").expect("alice");
        let group_id = b"test-text-channel-epoch";
        alice.create_group(group_id).expect("create group");

        // Alice derives Bob's key at epoch 0 (as she would on receiving a
        // message from him).
        let bob_session = 42u32;
        let (key_at_0, epoch) = alice
            .export_sender_key(group_id, bob_session)
            .expect("export at epoch 0");
        assert_eq!(epoch, 0);

        // Charlie joins, advancing the epoch.
        let charlie = new_client("charlie").expect("charlie");
        let charlie_kp = charlie.get_key_package_bytes().expect("charlie KP");
        alice
            .add_member(group_id, &charlie_kp)
            .expect("add charlie");
        assert_eq!(alice.epoch(group_id).unwrap(), 1);

        // The epoch-0 key is still retrievable...
        let retained = alice
            .export_sender_key_at_epoch(group_id, bob_session, 0)
            .expect("retained epoch 0 key");
        assert_eq!(retained, key_at_0);

        // ...and is genuinely different from the epoch-1 key, so asking for the
        // wrong epoch really would have failed to decrypt.
        let key_at_1 = alice
            .export_sender_key_at_epoch(group_id, bob_session, 1)
            .expect("epoch 1 key");
        assert_ne!(retained, key_at_1);
    }

    /// Two peers must derive the same key for a given (sender, epoch) pair even
    /// after the epoch has moved on for both of them.
    #[test]
    fn test_retained_keys_agree_across_peers() {
        let mut alice = new_client("alice").expect("alice");
        let group_id = b"test-text-channel-agree";
        alice.create_group(group_id).expect("create group");

        let mut bob = new_client("bob").expect("bob");
        let bob_kp = bob.get_key_package_bytes().expect("bob KP");
        let (_commit, welcome) = alice.add_member(group_id, &bob_kp).expect("add bob");
        bob.process_welcome(&welcome).expect("bob joins");

        // Both are at epoch 1 and both derive Alice's sender key there.
        let sender = 7u32;
        let (alice_key, e1) = alice
            .export_sender_key(group_id, sender)
            .expect("alice key");
        let (bob_key, e2) = bob.export_sender_key(group_id, sender).expect("bob key");
        assert_eq!((e1, e2), (1, 1));
        assert_eq!(alice_key, bob_key);

        // Charlie joins: epoch 2 for both.
        let charlie = new_client("charlie").expect("charlie");
        let charlie_kp = charlie.get_key_package_bytes().expect("charlie KP");
        let (commit, _welcome) = alice
            .add_member(group_id, &charlie_kp)
            .expect("add charlie");
        bob.process_commit(group_id, &commit).expect("bob commits");
        assert_eq!(alice.epoch(group_id).unwrap(), 2);
        assert_eq!(bob.epoch(group_id).unwrap(), 2);

        // Both still agree on the epoch-1 key.
        let alice_retained = alice
            .export_sender_key_at_epoch(group_id, sender, 1)
            .expect("alice retained");
        let bob_retained = bob
            .export_sender_key_at_epoch(group_id, sender, 1)
            .expect("bob retained");
        assert_eq!(alice_retained, bob_retained);
        assert_eq!(alice_retained, alice_key);
    }

    /// `register_sender` warms a key so a member's *first* message survives an
    /// epoch change that happens before anyone has derived their key.
    #[test]
    fn test_register_sender_warms_key_before_epoch_change() {
        let mut alice = new_client("alice").expect("alice");
        let group_id = b"test-text-channel-warm";
        alice.create_group(group_id).expect("create group");

        let newcomer = 99u32;
        alice
            .register_sender(group_id, newcomer)
            .expect("register newcomer");

        let charlie = new_client("charlie").expect("charlie");
        let charlie_kp = charlie.get_key_package_bytes().expect("charlie KP");
        alice
            .add_member(group_id, &charlie_kp)
            .expect("add charlie");

        assert!(alice
            .export_sender_key_at_epoch(group_id, newcomer, 0)
            .is_ok());
    }

    /// An epoch that has aged out of the retention window reports
    /// `EpochUnavailable` rather than handing back a wrong key.
    #[test]
    fn test_stale_epoch_is_rejected() {
        let mut alice = new_client("alice").expect("alice");
        let group_id = b"test-text-channel-stale";
        alice.create_group(group_id).expect("create group");

        let sender = 3u32;
        alice.export_sender_key(group_id, sender).expect("epoch 0");

        // Advance well past the retention window.
        for _ in 0..EPOCH_KEY_RETENTION + 2 {
            let peer = new_client("peer").expect("peer");
            let kp = peer.get_key_package_bytes().expect("peer KP");
            alice.add_member(group_id, &kp).expect("add peer");
        }

        match alice.export_sender_key_at_epoch(group_id, sender, 0) {
            Err(MlsError::EpochUnavailable { epoch, .. }) => assert_eq!(epoch, 0),
            other => panic!("expected EpochUnavailable, got {:?}", other.map(|_| "key")),
        }
    }

    /// A self-update commit advances the epoch and rotates the group secret,
    /// while the pre-ratchet epoch's keys stay decryptable.
    #[test]
    fn test_self_update_ratchets_epoch() {
        let mut alice = new_client("alice").expect("alice");
        let group_id = b"test-text-channel-ratchet";
        alice.create_group(group_id).expect("create group");

        let mut bob = new_client("bob").expect("bob");
        let bob_kp = bob.get_key_package_bytes().expect("bob KP");
        let (_commit, welcome) = alice.add_member(group_id, &bob_kp).expect("add bob");
        bob.process_welcome(&welcome).expect("bob joins");

        let sender = 11u32;
        let (key_before, epoch_before) = alice
            .export_sender_key(group_id, sender)
            .expect("pre-ratchet");
        bob.export_sender_key(group_id, sender).expect("bob warms");

        // Alice ratchets; Bob applies the commit.
        let commit = alice.self_update(group_id).expect("self update");
        bob.process_commit(group_id, &commit).expect("bob applies");

        let epoch_after = alice.epoch(group_id).unwrap();
        assert_eq!(epoch_after, epoch_before + 1);
        assert_eq!(bob.epoch(group_id).unwrap(), epoch_after);

        // New epoch yields a different key, and both peers agree on it.
        let (alice_new, _) = alice
            .export_sender_key(group_id, sender)
            .expect("alice new");
        let (bob_new, _) = bob.export_sender_key(group_id, sender).expect("bob new");
        assert_ne!(alice_new, key_before, "ratchet must rotate the secret");
        assert_eq!(alice_new, bob_new);

        // The pre-ratchet epoch is still decryptable on both sides.
        assert_eq!(
            alice
                .export_sender_key_at_epoch(group_id, sender, epoch_before)
                .expect("alice retained"),
            key_before
        );
        assert_eq!(
            bob.export_sender_key_at_epoch(group_id, sender, epoch_before)
                .expect("bob retained"),
            key_before
        );
    }

    /// Leaving a channel must drop the retained keys with it, so a later
    /// membership of the same channel can never be served stale material.
    #[test]
    fn test_leaving_clears_retained_keys() {
        // Alice hosts; Bob joins and derives a key, then leaves.
        let mut alice = new_client("alice").expect("alice");
        let group_id = b"test-text-channel-leave";
        alice.create_group(group_id).expect("create group");

        let mut bob = new_client("bob").expect("bob");
        let bob_kp = bob.get_key_package_bytes().expect("bob KP");
        let (_commit, welcome) = alice.add_member(group_id, &bob_kp).expect("add bob");
        bob.process_welcome(&welcome).expect("bob joins");

        let sender = 5u32;
        bob.export_sender_key(group_id, sender).expect("bob key");
        assert!(bob.export_sender_key_at_epoch(group_id, sender, 1).is_ok());

        bob.leave_group(group_id).expect("bob leaves");

        // Group and its retained keys are both gone.
        assert!(!bob.is_member(group_id));
        assert!(bob.exported_keys.keys().all(|(gid, _)| gid != group_id));
        assert!(!bob.known_senders.contains_key(group_id.as_slice()));
    }

    #[test]
    fn test_three_party_group() {
        // Alice creates a group
        let mut alice = new_client("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-5";
        alice
            .create_group(group_id)
            .expect("Failed to create group");

        // Add Bob
        let mut bob = new_client("bob").expect("Failed to create bob");
        let bob_kp = bob.get_key_package_bytes().expect("Bob KP");
        let (_commit1, welcome1) = alice.add_member(group_id, &bob_kp).expect("Add bob");
        bob.process_welcome(&welcome1).expect("Bob joins");

        // Add Charlie
        let mut charlie = new_client("charlie").expect("Failed to create charlie");
        let charlie_kp = charlie.get_key_package_bytes().expect("Charlie KP");
        let (commit2, welcome2) = alice
            .add_member(group_id, &charlie_kp)
            .expect("Add charlie");
        charlie.process_welcome(&welcome2).expect("Charlie joins");

        // Bob processes the commit from Alice adding Charlie
        bob.process_commit(group_id, &commit2)
            .expect("Bob processes commit");

        // All three should have the same epoch and derive matching keys with same sender_id
        let (alice_key, epoch_a) = alice.export_sender_key(group_id, 0).expect("Alice key");
        let (bob_key, epoch_b) = bob.export_sender_key(group_id, 0).expect("Bob key");
        let (charlie_key, epoch_c) = charlie.export_sender_key(group_id, 0).expect("Charlie key");

        assert_eq!(epoch_a, epoch_b);
        assert_eq!(epoch_b, epoch_c);
        assert_eq!(alice_key, bob_key);
        assert_eq!(bob_key, charlie_key);
    }

    // ------------------------------------------------------------------
    // Hybrid post-quantum ciphersuite
    // ------------------------------------------------------------------

    /// The suite pre-X-Wing clients shipped with.
    const LEGACY_CIPHERSUITE: Ciphersuite =
        Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

    /// X-Wing public key: ML-KEM-768 encapsulation key (1184) + X25519 (32).
    const XWING_PUBLIC_KEY_LEN: usize = 1216;

    /// The KEM must be the X25519+ML-KEM-768 hybrid, the AEAD 256-bit so the
    /// MLS layer matches the data plane's PQ margin, and signatures ML-DSA-44
    /// (the scheme member credentials certify).
    #[test]
    fn test_ciphersuite_is_hybrid_post_quantum() {
        assert_eq!(
            CIPHERSUITE.hpke_kem_algorithm(),
            HpkeKemType::XWingKemDraft6
        );
        assert_eq!(CIPHERSUITE.aead_algorithm(), AeadType::ChaCha20Poly1305);
        assert_eq!(
            CIPHERSUITE.hpke_aead_algorithm(),
            HpkeAeadType::ChaCha20Poly1305
        );
        assert_eq!(CIPHERSUITE.hash_algorithm(), HashType::Sha2_384);
        assert_eq!(CIPHERSUITE.signature_algorithm(), SignatureScheme::MLDSA44);
        assert_eq!(
            CIPHERSUITE.signature_algorithm() as u16,
            MLS_SIGNATURE_SCHEME_MLDSA44
        );

        let provider = OpenMlsRustCrypto::default();
        assert!(provider.crypto().supports(CIPHERSUITE).is_ok());
        assert!(provider
            .crypto()
            .supported_ciphersuites()
            .contains(&CIPHERSUITE));
    }

    /// KeyPackages must actually carry X-Wing keys, not merely be labelled
    /// with the X-Wing suite. An X25519 key is 32 bytes; X-Wing is 1216.
    #[test]
    fn test_key_package_carries_xwing_keys() {
        let client = new_client("alice").expect("client");
        let bundle = client.generate_key_package().expect("KP");
        let kp = bundle.key_package();

        assert_eq!(kp.ciphersuite(), CIPHERSUITE);
        assert_eq!(kp.hpke_init_key().as_slice().len(), XWING_PUBLIC_KEY_LEN);
        // The leaf key's bytes are crate-private; its TLS encoding is a
        // 2-byte MLS varint length prefix followed by the key.
        use openmls::prelude::tls_codec::Size;
        assert_eq!(
            kp.leaf_node().encryption_key().tls_serialized_len(),
            2 + XWING_PUBLIC_KEY_LEN
        );

        // We advertise only the suite we accept — no classical fallback.
        let advertised: Vec<Ciphersuite> = kp
            .leaf_node()
            .capabilities()
            .ciphersuites()
            .iter()
            .map(|cs| Ciphersuite::try_from(*cs).expect("known suite"))
            .collect();
        assert_eq!(advertised, vec![CIPHERSUITE]);
    }

    /// Full lifecycle under X-Wing: create, join via Welcome, add a third
    /// member via Commit, then ratchet from a *non-founder* so every member
    /// has to decapsulate an X-Wing path secret from someone else. Keys must
    /// agree at every epoch and the old epoch must stay decryptable.
    #[test]
    fn test_pq_join_commit_and_ratchet() {
        let group_id = b"test-pq-lifecycle";
        let sender = 21u32;

        let mut alice = new_client("alice").expect("alice");
        alice.create_group(group_id).expect("create");

        let mut bob = new_client("bob").expect("bob");
        let (_c, welcome) = alice
            .add_member(group_id, &bob.get_key_package_bytes().unwrap())
            .expect("add bob");
        bob.process_welcome(&welcome).expect("bob joins");

        let mut charlie = new_client("charlie").expect("charlie");
        let (commit, welcome) = alice
            .add_member(group_id, &charlie.get_key_package_bytes().unwrap())
            .expect("add charlie");
        charlie.process_welcome(&welcome).expect("charlie joins");
        bob.process_commit(group_id, &commit)
            .expect("bob applies add");

        for c in [&alice, &bob, &charlie] {
            assert_eq!(c.groups[group_id.as_slice()].ciphersuite(), CIPHERSUITE);
            assert_eq!(c.epoch(group_id).unwrap(), 2);
        }

        let (k2_a, _) = alice.export_sender_key(group_id, sender).unwrap();
        let (k2_b, _) = bob.export_sender_key(group_id, sender).unwrap();
        let (k2_c, _) = charlie.export_sender_key(group_id, sender).unwrap();
        assert_eq!(k2_a, k2_b);
        assert_eq!(k2_b, k2_c);

        // Bob (not the founder) ratchets.
        let ratchet = bob.self_update(group_id).expect("bob ratchets");
        alice
            .process_commit(group_id, &ratchet)
            .expect("alice applies");
        charlie
            .process_commit(group_id, &ratchet)
            .expect("charlie applies");

        let (k3_a, e_a) = alice.export_sender_key(group_id, sender).unwrap();
        let (k3_b, e_b) = bob.export_sender_key(group_id, sender).unwrap();
        let (k3_c, e_c) = charlie.export_sender_key(group_id, sender).unwrap();
        assert_eq!((e_a, e_b, e_c), (3, 3, 3));
        assert_eq!(k3_a, k3_b);
        assert_eq!(k3_b, k3_c);
        assert_ne!(k3_a, k2_a, "ratchet must rotate the epoch secret");

        for c in [&mut alice, &mut bob, &mut charlie] {
            assert_eq!(
                c.export_sender_key_at_epoch(group_id, sender, 2).unwrap(),
                k2_a
            );
        }
    }

    /// A client still on the classical suite must get a clear mismatch error
    /// when it tries to join, and the founder's group must not change.
    #[test]
    fn test_rejects_legacy_key_package() {
        let group_id = b"test-pq-reject-legacy-kp";
        let mut alice = new_client("alice").expect("alice");
        alice.create_group(group_id).expect("create");

        // Built exactly as the pre-X-Wing client did.
        let legacy = OpenMlsRustCrypto::default();
        let signer = openmls_basic_credential::SignatureKeyPair::new(
            LEGACY_CIPHERSUITE.signature_algorithm(),
        )
        .unwrap();
        signer.store(legacy.storage()).unwrap();
        let legacy_kp = KeyPackage::builder()
            .build(
                LEGACY_CIPHERSUITE,
                &legacy,
                &signer,
                CredentialWithKey {
                    credential: BasicCredential::new(b"legacy".to_vec()).into(),
                    signature_key: signer.public().into(),
                },
            )
            .unwrap()
            .key_package()
            .tls_serialize_detached()
            .unwrap();

        match alice.add_member(group_id, &legacy_kp) {
            Err(MlsError::CiphersuiteMismatch { expected, actual }) => {
                assert_eq!(expected, CIPHERSUITE);
                assert_eq!(actual, LEGACY_CIPHERSUITE);
            }
            other => panic!("expected CiphersuiteMismatch, got {:?}", other.map(|_| ())),
        }
        assert_eq!(alice.epoch(group_id).unwrap(), 0, "group must be untouched");
        assert_eq!(alice.group_members(group_id).unwrap().len(), 1);
    }

    /// Downgrade guard: a Welcome into a classical-suite group is refused even
    /// when it is validly addressed to one of our own KeyPackages.
    #[test]
    fn test_rejects_classical_welcome() {
        // Our provider can still operate the legacy suite, so the Welcome
        // decrypts and our own ciphersuite check is what refuses it.
        const CLASSICAL: Ciphersuite = LEGACY_CIPHERSUITE;
        let group_id = b"test-pq-reject-classical-welcome";

        let mut bob = new_client("bob").expect("bob");
        // The classical suite signs with Ed25519, so this KeyPackage needs a
        // classical signer; built with Bob's provider so its private keys are
        // there when the Welcome arrives, as a real KP's would be.
        let bob_classical_signer =
            openmls_basic_credential::SignatureKeyPair::new(CLASSICAL.signature_algorithm())
                .unwrap();
        bob_classical_signer.store(bob.provider.storage()).unwrap();
        let bob_classical_kp = KeyPackage::builder()
            .build(
                CLASSICAL,
                &bob.provider,
                &bob_classical_signer,
                CredentialWithKey {
                    credential: BasicCredential::new(b"bob".to_vec()).into(),
                    signature_key: bob_classical_signer.public().into(),
                },
            )
            .unwrap()
            .key_package()
            .clone();

        // A peer that builds a classical group and invites Bob into it.
        let peer = OpenMlsRustCrypto::default();
        let peer_signer =
            openmls_basic_credential::SignatureKeyPair::new(CLASSICAL.signature_algorithm())
                .unwrap();
        peer_signer.store(peer.storage()).unwrap();
        let mut group = MlsGroup::new_with_group_id(
            &peer,
            &peer_signer,
            &MlsGroupCreateConfig::builder()
                .use_ratchet_tree_extension(true)
                .ciphersuite(CLASSICAL)
                .build(),
            GroupId::from_slice(group_id),
            CredentialWithKey {
                credential: BasicCredential::new(b"peer".to_vec()).into(),
                signature_key: peer_signer.public().into(),
            },
        )
        .unwrap();
        let (_commit, welcome, _) = group
            .add_members(&peer, &peer_signer, &[bob_classical_kp])
            .unwrap();
        group.merge_pending_commit(&peer).unwrap();
        let welcome = welcome.tls_serialize_detached().unwrap();

        match bob.process_welcome(&welcome) {
            Err(MlsError::CiphersuiteMismatch { expected, actual }) => {
                assert_eq!(expected, CIPHERSUITE);
                assert_eq!(actual, CLASSICAL);
            }
            other => panic!("expected CiphersuiteMismatch, got {:?}", other),
        }
        assert!(!bob.is_member(group_id));
        // Rejected before `into_group`, so nothing was persisted either.
        assert!(
            MlsGroup::load(bob.provider.storage(), &GroupId::from_slice(group_id))
                .unwrap()
                .is_none()
        );
    }

    // ------------------------------------------------------------------
    // Persistent, certified MLS keys
    // ------------------------------------------------------------------

    /// A signer for `identity`'s derived MLS key, stored in `provider`.
    fn raw_signer(
        identity: &IdentitySecret,
        provider: &OpenMlsRustCrypto,
    ) -> openmls_basic_credential::SignatureKeyPair {
        let k = identity.mls_signing_key(0);
        let signer = openmls_basic_credential::SignatureKeyPair::from_raw(
            CIPHERSUITE.signature_algorithm(),
            k.seed.to_vec(),
            k.public_key.clone(),
        );
        signer.store(provider.storage()).unwrap();
        signer
    }

    /// `claimed`'s genuine credential (its identity certifying its own MLS
    /// key), attached to a leaf that `signer` signs. That is someone
    /// presenting another user's certificate as their own.
    fn forged_credential(
        claimed: &IdentitySecret,
        signer: &openmls_basic_credential::SignatureKeyPair,
    ) -> CredentialWithKey {
        let claimed_key = claimed.mls_signing_key(0);
        let cred = MemberCredential::issue(
            claimed,
            "U_victim",
            MLS_SIGNATURE_SCHEME_MLDSA44,
            &claimed_key.public_key,
        )
        .unwrap();
        CredentialWithKey {
            credential: BasicCredential::new(cred.to_bytes()).into(),
            signature_key: signer.public().into(),
        }
    }

    fn forged_key_package(provider: &OpenMlsRustCrypto) -> KeyPackage {
        let signer = raw_signer(&IdentitySecret::generate(), provider);
        KeyPackage::builder()
            .leaf_node_capabilities(leaf_capabilities())
            .build(
                CIPHERSUITE,
                provider,
                &signer,
                forged_credential(&IdentitySecret::generate(), &signer),
            )
            .unwrap()
            .key_package()
            .clone()
    }

    fn group_config() -> MlsGroupCreateConfig {
        MlsGroupCreateConfig::builder()
            .use_ratchet_tree_extension(true)
            .ciphersuite(CIPHERSUITE)
            .capabilities(leaf_capabilities())
            .build()
    }

    /// The MLS key comes from the identity, so a member reconnecting with a
    /// new client presents the same key: trust pinned on it survives.
    #[test]
    fn test_mls_key_is_the_same_on_every_client_for_one_identity() {
        let id = IdentitySecret::generate();
        let a = MlsClient::new("U_a", &id).unwrap();
        let b = MlsClient::new("U_a", &id).unwrap();
        let ka = a.generate_key_package().unwrap();
        let kb = b.generate_key_package().unwrap();
        let key_a = ka.key_package().leaf_node().signature_key();
        assert_eq!(key_a, kb.key_package().leaf_node().signature_key());
        assert_eq!(key_a.as_slice().len(), 1312, "ML-DSA-44 public key");
        assert_eq!(key_a.as_slice(), id.mls_signing_key(0).public_key);
    }

    #[test]
    fn test_group_members_report_verified_identity_keys() {
        let alice_id = IdentitySecret::generate();
        let bob_id = IdentitySecret::generate();
        let mut alice = MlsClient::new("U_alice", &alice_id).unwrap();
        let mut bob = MlsClient::new("U_bob", &bob_id).unwrap();
        let gid = b"test-members-identity";
        alice.create_group(gid).unwrap();
        let kp = bob.get_key_package_bytes().unwrap();
        let (_commit, welcome) = alice.add_member(gid, &kp).unwrap();
        bob.process_welcome(&welcome).unwrap();

        let members = bob.group_members(gid).unwrap();
        assert_eq!(members.len(), 2);
        let a = members.iter().find(|m| m.identity == "U_alice").unwrap();
        let b = members.iter().find(|m| m.identity == "U_bob").unwrap();
        assert_eq!(a.identity_key, alice_id.public_key().as_bytes());
        assert_eq!(b.identity_key, bob_id.public_key().as_bytes());
        assert!(b.is_self && !a.is_self);
    }

    #[test]
    fn test_add_member_rejects_forged_credential() {
        let mut alice = new_client("U_alice").unwrap();
        let gid = b"test-forged-key-package";
        alice.create_group(gid).unwrap();
        let epoch = alice.epoch(gid).unwrap();

        let kp = forged_key_package(&OpenMlsRustCrypto::default())
            .tls_serialize_detached()
            .unwrap();
        assert!(matches!(
            alice.add_member(gid, &kp),
            Err(MlsError::InvalidCredential(_))
        ));
        assert_eq!(alice.epoch(gid).unwrap(), epoch);
    }

    /// A Welcome into a group whose founder signs with one key but carries
    /// another user's credential is refused, and nothing is stored.
    #[test]
    fn test_welcome_rejects_group_with_forged_member() {
        let gid = b"test-forged-welcome";
        let mut bob = new_client("U_bob").unwrap();
        let bob_kp = bob.generate_key_package().unwrap().key_package().clone();

        let provider = OpenMlsRustCrypto::default();
        let signer = raw_signer(&IdentitySecret::generate(), &provider);
        let mut group = MlsGroup::new_with_group_id(
            &provider,
            &signer,
            &group_config(),
            GroupId::from_slice(gid),
            forged_credential(&IdentitySecret::generate(), &signer),
        )
        .unwrap();
        let (_commit, welcome, _) = group.add_members(&provider, &signer, &[bob_kp]).unwrap();
        group.merge_pending_commit(&provider).unwrap();

        let welcome = welcome.tls_serialize_detached().unwrap();
        assert!(matches!(
            bob.process_welcome(&welcome),
            Err(MlsError::InvalidCredential(_))
        ));
        assert!(!bob.is_member(gid));
        assert!(
            MlsGroup::load(bob.provider.storage(), &GroupId::from_slice(gid))
                .unwrap()
                .is_none()
        );
    }

    /// A legitimate founder's commit that adds a forged member is refused,
    /// and the group stays at its epoch.
    #[test]
    fn test_commit_adding_forged_member_is_rejected() {
        let gid = b"test-forged-commit";
        let founder_id = IdentitySecret::generate();
        let provider = OpenMlsRustCrypto::default();
        let signer = raw_signer(&founder_id, &provider);
        let founder_cred = MemberCredential::issue(
            &founder_id,
            "U_founder",
            MLS_SIGNATURE_SCHEME_MLDSA44,
            signer.public(),
        )
        .unwrap();
        let mut group = MlsGroup::new_with_group_id(
            &provider,
            &signer,
            &group_config(),
            GroupId::from_slice(gid),
            CredentialWithKey {
                credential: BasicCredential::new(founder_cred.to_bytes()).into(),
                signature_key: signer.public().into(),
            },
        )
        .unwrap();

        let mut bob = new_client("U_bob").unwrap();
        let bob_kp = bob.generate_key_package().unwrap().key_package().clone();
        let (_commit, welcome, _) = group.add_members(&provider, &signer, &[bob_kp]).unwrap();
        group.merge_pending_commit(&provider).unwrap();
        bob.process_welcome(&welcome.tls_serialize_detached().unwrap())
            .unwrap();
        let epoch = bob.epoch(gid).unwrap();

        let forged = forged_key_package(&OpenMlsRustCrypto::default());
        let (commit, _welcome, _) = group.add_members(&provider, &signer, &[forged]).unwrap();
        let commit = commit.tls_serialize_detached().unwrap();
        assert!(matches!(
            bob.process_commit(gid, &commit),
            Err(MlsError::InvalidCredential(_))
        ));
        assert_eq!(bob.epoch(gid).unwrap(), epoch);
    }
}
