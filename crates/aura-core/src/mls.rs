//! MLS Encryption Client for DAVE Protocol
//!
//! Provides MLS group management for E2EE voice and text channels.
//! Uses OpenMLS with Ed25519 signatures and Curve25519 key exchange.

use openmls::prelude::*;
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
    // TODO: These methods require more complex message serialization
    // They are stubbed for now and will be implemented in a follow-up PR.
    // ============================================================================
    
    /// Process a Welcome message to join a group
    pub fn process_welcome(&mut self, _welcome_bytes: &[u8]) -> Result<Vec<u8>, MlsError> {
        // TODO: Implement Welcome processing
        Err(MlsError::OpenMls("Welcome processing not yet implemented".into()))
    }
    
    /// Add a member to a group (returns Commit and Welcome messages)
    pub fn add_member(
        &mut self,
        _group_id: &[u8],
        _key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), MlsError> {
        // TODO: Implement member addition with proper serialization
        Err(MlsError::OpenMls("Add member not yet implemented".into()))
    }
    
    /// Process a Commit message from another member
    pub fn process_commit(&mut self, _group_id: &[u8], _commit_bytes: &[u8]) -> Result<u64, MlsError> {
        // TODO: Implement Commit processing
        Err(MlsError::OpenMls("Commit processing not yet implemented".into()))
    }
    
    /// Leave a group
    pub fn leave_group(&mut self, _group_id: &[u8]) -> Result<Vec<u8>, MlsError> {
        // TODO: Implement leave with proper serialization
        Err(MlsError::OpenMls("Leave group not yet implemented".into()))
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
}
