//! MLS Encryption Client for DAVE Protocol
//!
//! Provides MLS group management for E2EE voice and text channels.
//! Uses OpenMLS with Ed25519 signatures and Curve25519 key exchange.

use openmls::prelude::*;
use openmls::prelude::tls_codec::{Deserialize, Serialize};
use openmls_rust_crypto::OpenMlsRustCrypto;
use std::collections::HashMap;

/// DAVE key derivation label
pub const DAVE_KEY_LABEL: &str = "aura-dave-key";

/// Key length for DAVE encryption (XChaCha20-Poly1305)
pub const DAVE_KEY_LEN: usize = 32;

/// Ciphersuite for all MLS operations
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

#[derive(Debug, thiserror::Error)]
pub enum MlsError {
    #[error("OpenMLS error: {0}")]
    OpenMls(String),
    #[error("Group not found: {0}")]
    GroupNotFound(String),
    #[error("No credential available")]
    NoCredential,
    #[error("Serialization error: {0}")]
    Serialization(String),
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
}

impl MlsClient {
    /// Create a new MLS client with a fresh Ed25519 identity
    pub fn new(identity_name: &str) -> Result<Self, MlsError> {
        let provider = OpenMlsRustCrypto::default();
        
        // Generate signature keypair using the basic credential crate
        let signer = openmls_basic_credential::SignatureKeyPair::new(CIPHERSUITE.signature_algorithm())
            .map_err(|e| MlsError::OpenMls(format!("KeyGen error: {:?}", e)))?;
        
        // Store the keypair in the provider's key store
        signer.store(provider.storage())
            .map_err(|e| MlsError::OpenMls(format!("KeyStore error: {:?}", e)))?;
        
        // Create basic credential with identity
        let credential = BasicCredential::new(identity_name.as_bytes().to_vec());
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
        })
    }
    
    /// Generate a KeyPackage for joining groups
    pub fn generate_key_package(&self) -> Result<KeyPackageBundle, MlsError> {
        let identity = self.identity.as_ref()
            .ok_or(MlsError::NoCredential)?;
        
        let key_package_bundle = KeyPackage::builder()
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
        let identity = self.identity.as_ref()
            .ok_or(MlsError::NoCredential)?;
        
        let group_config = MlsGroupCreateConfig::builder()
            .use_ratchet_tree_extension(true)
            .ciphersuite(CIPHERSUITE)
            .build();
        
        let group = MlsGroup::new_with_group_id(
            &self.provider,
            &identity.signer,
            &group_config,
            GroupId::from_slice(group_id),
            identity.credential_with_key.clone(),
        )
        .map_err(|e| MlsError::OpenMls(format!("Group creation error: {:?}", e)))?;
        
        self.groups.insert(group_id.to_vec(), group);
        Ok(())
    }
    
    /// Export a DAVE encryption key for the current epoch
    pub fn export_dave_key(&self, group_id: &[u8]) -> Result<([u8; DAVE_KEY_LEN], u64), MlsError> {
        let group = self.groups.get(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;
        
        let epoch = group.epoch().as_u64();
        let context = epoch.to_le_bytes();
        
        let secret = group.export_secret(
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
    
    /// Get the current epoch for a group
    pub fn epoch(&self, group_id: &[u8]) -> Result<u64, MlsError> {
        let group = self.groups.get(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;
        
        Ok(group.epoch().as_u64())
    }
    
    /// Check if we are a member of a group
    pub fn is_member(&self, group_id: &[u8]) -> bool {
        self.groups.contains_key(group_id)
    }
    
    // ============================================================================
    // MLS Group Management - Full Implementation
    // ============================================================================
    
    /// Process a Welcome message to join a group
    /// Returns the group_id of the joined group
    pub fn process_welcome(&mut self, welcome_bytes: &[u8]) -> Result<Vec<u8>, MlsError> {
        // Deserialize the Welcome message using tls_codec
        let mls_message_in = MlsMessageIn::tls_deserialize_exact(welcome_bytes)
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
        
        // Process the Welcome and create the group
        let group = StagedWelcome::new_from_welcome(
            &self.provider,
            &group_config,
            welcome,
            None, // No ratchet tree provided separately
        )
        .map_err(|e| MlsError::OpenMls(format!("Welcome processing error: {:?}", e)))?
        .into_group(&self.provider)
        .map_err(|e| MlsError::OpenMls(format!("Group creation from welcome error: {:?}", e)))?;
        
        let group_id = group.group_id().as_slice().to_vec();
        self.groups.insert(group_id.clone(), group);
        
        Ok(group_id)
    }
    
    /// Add a member to a group (returns serialized Commit and Welcome messages)
    pub fn add_member(
        &mut self,
        group_id: &[u8],
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), MlsError> {
        let identity = self.identity.as_ref()
            .ok_or(MlsError::NoCredential)?;
        
        let group = self.groups.get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;
        
        // Deserialize the KeyPackage
        let key_package_in = KeyPackageIn::tls_deserialize_exact(key_package_bytes)
            .map_err(|e| MlsError::Serialization(format!("KeyPackage deserialize error: {:?}", e)))?;
        
        // Validate the KeyPackage
        let key_package = key_package_in.validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::OpenMls(format!("KeyPackage validation error: {:?}", e)))?;
        
        // Add the member and get (commit, welcome, group_info)
        let (commit_out, welcome, _group_info) = group.add_members(
            &self.provider,
            &identity.signer,
            &[key_package],
        )
        .map_err(|e| MlsError::OpenMls(format!("Add member error: {:?}", e)))?;
        
        // Merge the pending commit
        group.merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("Merge commit error: {:?}", e)))?;
        
        // Serialize the commit
        let commit_bytes = commit_out.tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Commit serialize error: {:?}", e)))?;
        
        // Serialize the welcome
        let welcome_bytes = welcome.tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Welcome serialize error: {:?}", e)))?;
        
        Ok((commit_bytes, welcome_bytes))
    }
    
    /// Process a Commit message from another member
    /// Returns the new epoch number
    pub fn process_commit(&mut self, group_id: &[u8], commit_bytes: &[u8]) -> Result<u64, MlsError> {
        let group = self.groups.get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;
        
        // Deserialize the commit message
        let message_in = MlsMessageIn::tls_deserialize_exact(commit_bytes)
            .map_err(|e| MlsError::Serialization(format!("Commit deserialize error: {:?}", e)))?;
        
        // Process the incoming message
        let protocol_message = message_in.try_into_protocol_message()
            .map_err(|e| MlsError::OpenMls(format!("Protocol message error: {:?}", e)))?;
        
        let processed = group.process_message(&self.provider, protocol_message)
            .map_err(|e| MlsError::OpenMls(format!("Process message error: {:?}", e)))?;
        
        // Handle the processed message content
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                group.merge_staged_commit(&self.provider, *staged_commit)
                    .map_err(|e| MlsError::OpenMls(format!("Merge staged commit error: {:?}", e)))?;
            }
            _ => {
                return Err(MlsError::OpenMls("Expected Commit message".into()));
            }
        }
        
        Ok(group.epoch().as_u64())
    }
    
    /// Leave a group (self-remove) by proposing a self-removal
    /// Returns the serialized proposal message to broadcast
    pub fn leave_group(&mut self, group_id: &[u8]) -> Result<Vec<u8>, MlsError> {
        let identity = self.identity.as_ref()
            .ok_or(MlsError::NoCredential)?;
        
        let group = self.groups.get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;
        
        // Create a self-remove proposal
        let leave_proposal = group.leave_group(&self.provider, &identity.signer)
            .map_err(|e| MlsError::OpenMls(format!("Leave group error: {:?}", e)))?;
        
        // Serialize the proposal
        let proposal_bytes = leave_proposal.tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Proposal serialize error: {:?}", e)))?;
        
        // Remove from our local groups
        self.groups.remove(group_id);
        
        Ok(proposal_bytes)
    }
    
    /// Remove a member from the group (admin operation)
    /// Returns the serialized Commit message to broadcast
    pub fn remove_member(&mut self, group_id: &[u8], member_index: u32) -> Result<Vec<u8>, MlsError> {
        let identity = self.identity.as_ref()
            .ok_or(MlsError::NoCredential)?;
        
        let group = self.groups.get_mut(group_id)
            .ok_or_else(|| MlsError::GroupNotFound(format!("{:02x?}", group_id)))?;
        
        // Create leaf node reference for the member to remove
        let leaf_index = LeafNodeIndex::new(member_index);
        
        // Remove the member
        let (commit_out, _welcome, _group_info) = group.remove_members(
            &self.provider,
            &identity.signer,
            &[leaf_index],
        )
        .map_err(|e| MlsError::OpenMls(format!("Remove member error: {:?}", e)))?;
        
        // Merge the pending commit
        group.merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("Merge commit error: {:?}", e)))?;
        
        // Serialize the commit
        let commit_bytes = commit_out.tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("Commit serialize error: {:?}", e)))?;
        
        Ok(commit_bytes)
    }
    
    /// Get serialized KeyPackage for sharing with others
    pub fn get_key_package_bytes(&self) -> Result<Vec<u8>, MlsError> {
        let kp_bundle = self.generate_key_package()?;
        let bytes = kp_bundle.key_package().tls_serialize_detached()
            .map_err(|e| MlsError::Serialization(format!("KeyPackage serialize error: {:?}", e)))?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_client_creation() {
        let client = MlsClient::new("alice").expect("Failed to create client");
        assert!(client.groups.is_empty());
        assert!(client.identity.is_some());
    }
    
    #[test]
    fn test_key_package_generation() {
        let client = MlsClient::new("alice").expect("Failed to create client");
        let kp_bundle = client.generate_key_package().expect("Failed to generate KeyPackage");
        // KeyPackage generated successfully
        assert!(kp_bundle.key_package().ciphersuite() == CIPHERSUITE);
    }
    
    #[test]
    fn test_group_creation() {
        let mut client = MlsClient::new("alice").expect("Failed to create client");
        let group_id = b"test-voice-channel-1";
        
        client.create_group(group_id).expect("Failed to create group");
        assert!(client.is_member(group_id));
        
        let epoch = client.epoch(group_id).expect("Failed to get epoch");
        assert_eq!(epoch, 0); // Initial epoch is 0
    }
    
    #[test]
    fn test_dave_key_export() {
        let mut client = MlsClient::new("alice").expect("Failed to create client");
        let group_id = b"test-voice-channel-1";
        
        client.create_group(group_id).expect("Failed to create group");
        
        let (key, epoch) = client.export_dave_key(group_id).expect("Failed to export key");
        assert_eq!(key.len(), DAVE_KEY_LEN);
        assert_eq!(epoch, 0);
    }
    
    #[test]
    fn test_add_member_and_welcome() {
        // Alice creates a group
        let mut alice = MlsClient::new("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-2";
        alice.create_group(group_id).expect("Failed to create group");
        
        // Bob generates a KeyPackage
        let bob = MlsClient::new("bob").expect("Failed to create bob");
        let bob_kp_bytes = bob.get_key_package_bytes().expect("Failed to get bob's KeyPackage");
        
        // Alice adds Bob
        let (commit_bytes, welcome_bytes) = alice.add_member(group_id, &bob_kp_bytes)
            .expect("Failed to add member");
        
        assert!(!commit_bytes.is_empty());
        assert!(!welcome_bytes.is_empty());
        
        // Alice's epoch should now be 1
        assert_eq!(alice.epoch(group_id).unwrap(), 1);
    }
    
    #[test]
    fn test_process_welcome() {
        // Alice creates a group
        let mut alice = MlsClient::new("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-3";
        alice.create_group(group_id).expect("Failed to create group");
        
        // Bob generates a KeyPackage
        let mut bob = MlsClient::new("bob").expect("Failed to create bob");
        let bob_kp_bytes = bob.get_key_package_bytes().expect("Failed to get bob's KeyPackage");
        
        // Alice adds Bob
        let (_commit_bytes, welcome_bytes) = alice.add_member(group_id, &bob_kp_bytes)
            .expect("Failed to add member");
        
        // Bob processes the Welcome and joins
        let joined_group_id = bob.process_welcome(&welcome_bytes)
            .expect("Failed to process welcome");
        
        assert_eq!(joined_group_id, group_id);
        assert!(bob.is_member(&joined_group_id));
        
        // Bob should be at same epoch as Alice
        assert_eq!(bob.epoch(&joined_group_id).unwrap(), alice.epoch(group_id).unwrap());
    }
    
    #[test]
    fn test_dave_keys_match_after_join() {
        // Alice creates a group
        let mut alice = MlsClient::new("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-4";
        alice.create_group(group_id).expect("Failed to create group");
        
        // Bob generates a KeyPackage and joins
        let mut bob = MlsClient::new("bob").expect("Failed to create bob");
        let bob_kp_bytes = bob.get_key_package_bytes().expect("Failed to get bob's KeyPackage");
        
        let (_commit, welcome) = alice.add_member(group_id, &bob_kp_bytes)
            .expect("Failed to add member");
        
        bob.process_welcome(&welcome).expect("Failed to process welcome");
        
        // Both should derive the same DAVE key
        let (alice_key, alice_epoch) = alice.export_dave_key(group_id).expect("Alice key export");
        let (bob_key, bob_epoch) = bob.export_dave_key(group_id).expect("Bob key export");
        
        assert_eq!(alice_epoch, bob_epoch);
        assert_eq!(alice_key, bob_key);
    }
    
    #[test]
    fn test_three_party_group() {
        // Alice creates a group
        let mut alice = MlsClient::new("alice").expect("Failed to create alice");
        let group_id = b"test-voice-channel-5";
        alice.create_group(group_id).expect("Failed to create group");
        
        // Add Bob
        let mut bob = MlsClient::new("bob").expect("Failed to create bob");
        let bob_kp = bob.get_key_package_bytes().expect("Bob KP");
        let (_commit1, welcome1) = alice.add_member(group_id, &bob_kp).expect("Add bob");
        bob.process_welcome(&welcome1).expect("Bob joins");
        
        // Add Charlie
        let mut charlie = MlsClient::new("charlie").expect("Failed to create charlie");
        let charlie_kp = charlie.get_key_package_bytes().expect("Charlie KP");
        let (commit2, welcome2) = alice.add_member(group_id, &charlie_kp).expect("Add charlie");
        charlie.process_welcome(&welcome2).expect("Charlie joins");
        
        // Bob processes the commit from Alice adding Charlie
        bob.process_commit(group_id, &commit2).expect("Bob processes commit");
        
        // All three should have the same epoch and DAVE key
        let (alice_key, epoch_a) = alice.export_dave_key(group_id).expect("Alice key");
        let (bob_key, epoch_b) = bob.export_dave_key(group_id).expect("Bob key");
        let (charlie_key, epoch_c) = charlie.export_dave_key(group_id).expect("Charlie key");
        
        assert_eq!(epoch_a, epoch_b);
        assert_eq!(epoch_b, epoch_c);
        assert_eq!(alice_key, bob_key);
        assert_eq!(bob_key, charlie_key);
    }
}
