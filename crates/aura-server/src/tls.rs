//! TLS 1.3 crypto configuration for the QUIC transport.
//!
//! Everything uses aws-lc-rs and offers the hybrid post-quantum key exchange
//! `X25519MLKEM768` (draft-ietf-tls-ecdhe-mlkem): the handshake secret then
//! stays confidential unless *both* X25519 and ML-KEM-768 are broken, so
//! recorded sessions resist "harvest now, decrypt later". Classical groups
//! stay enabled so clients whose TLS stack lacks ML-KEM can still connect.
//!
//! **Who picks the group:** rustls servers take the first group in the
//! *client's* preference list that they support. The server cannot force PQ;
//! it can only make sure it is available. Any client that lists
//! `X25519MLKEM768` first (macOS 26+ Network.framework, OpenSSL 3.5+, rustls
//! with aws-lc-rs) gets a hybrid handshake.

use rustls::crypto::aws_lc_rs::{self, kx_group};
use rustls::crypto::{CryptoProvider, SupportedKxGroup};
use rustls::server::WantsServerCert;
use rustls::{ConfigBuilder, ServerConfig};
use std::sync::Arc;

/// ALPN protocol identifier for Aura's QUIC transport.
pub const ALPN: &[u8] = b"aura-dave";

/// Key exchange groups, most preferred first.
///
/// The order is spelled out here rather than taken from rustls's defaults,
/// which only put the hybrid group first when the `prefer-post-quantum` cargo
/// feature happens to be unified into the build.
pub static KX_GROUPS: &[&dyn SupportedKxGroup] = &[
    kx_group::X25519MLKEM768,
    kx_group::X25519,
    kx_group::SECP256R1,
    kx_group::SECP384R1,
];

/// aws-lc-rs provider with [`KX_GROUPS`].
pub fn crypto_provider() -> CryptoProvider {
    CryptoProvider {
        kx_groups: KX_GROUPS.to_vec(),
        ..aws_lc_rs::default_provider()
    }
}

/// Install [`crypto_provider`] as the process-wide default, so anything
/// building rustls configs implicitly (e.g. rustls-acme's ACME client) uses it.
/// A no-op if a default is already installed.
pub fn install_default_provider() {
    let _ = crypto_provider().install_default();
}

/// A `ServerConfig` builder using [`crypto_provider`], restricted to TLS 1.3
/// (the only version QUIC permits), with no client authentication. The caller
/// supplies the certificate.
pub fn server_config_builder() -> Result<ConfigBuilder<ServerConfig, WantsServerCert>, rustls::Error>
{
    Ok(
        ServerConfig::builder_with_provider(Arc::new(crypto_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
    use rustls::{ClientConfig, ClientConnection, NamedGroup, RootCertStore, ServerConnection};

    fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        (
            CertificateDer::from(cert.cert.der().to_vec()),
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der())),
        )
    }

    fn client_offering(
        groups: &[&'static dyn SupportedKxGroup],
        root: CertificateDer<'static>,
    ) -> ClientConfig {
        let mut roots = RootCertStore::empty();
        roots.add(root).unwrap();
        let provider = CryptoProvider {
            kx_groups: groups.to_vec(),
            ..aws_lc_rs::default_provider()
        };
        ClientConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth()
    }

    /// Run a full in-memory TLS 1.3 handshake against the production server
    /// config and return the key exchange group the server negotiated.
    fn negotiate(client_groups: &[&'static dyn SupportedKxGroup]) -> NamedGroup {
        let (cert, key) = self_signed();
        let server_config = server_config_builder()
            .unwrap()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        let mut server = ServerConnection::new(Arc::new(server_config)).unwrap();
        let mut client = ClientConnection::new(
            Arc::new(client_offering(client_groups, cert)),
            ServerName::try_from("localhost").unwrap(),
        )
        .unwrap();

        while client.is_handshaking() || server.is_handshaking() {
            let mut buf = Vec::new();
            while client.wants_write() {
                client.write_tls(&mut buf).unwrap();
            }
            let mut rd = &buf[..];
            while !rd.is_empty() {
                server.read_tls(&mut rd).unwrap();
                server.process_new_packets().unwrap();
            }

            let mut buf = Vec::new();
            while server.wants_write() {
                server.write_tls(&mut buf).unwrap();
            }
            let mut rd = &buf[..];
            while !rd.is_empty() {
                client.read_tls(&mut rd).unwrap();
                client.process_new_packets().unwrap();
            }
        }

        let server_group = server.negotiated_key_exchange_group().unwrap().name();
        assert_eq!(
            client.negotiated_key_exchange_group().unwrap().name(),
            server_group
        );
        server_group
    }

    #[test]
    fn hybrid_pq_group_is_listed_first() {
        assert_eq!(KX_GROUPS[0].name(), NamedGroup::X25519MLKEM768);
        assert!(crypto_provider()
            .kx_groups
            .iter()
            .any(|g| g.name() == NamedGroup::X25519),);
    }

    /// A modern client (PQ preferred, classical available) gets the hybrid.
    #[test]
    fn pq_capable_client_negotiates_x25519mlkem768() {
        let group = negotiate(&[kx_group::X25519MLKEM768, kx_group::X25519]);
        assert_eq!(group, NamedGroup::X25519MLKEM768);
    }

    /// A client without ML-KEM still connects, over classical X25519.
    #[test]
    fn classical_only_client_falls_back_to_x25519() {
        let group = negotiate(&[kx_group::X25519]);
        assert_eq!(group, NamedGroup::X25519);
    }

    /// A client that only offers the hybrid group must not be refused.
    #[test]
    fn pq_only_client_is_accepted() {
        let group = negotiate(&[kx_group::X25519MLKEM768]);
        assert_eq!(group, NamedGroup::X25519MLKEM768);
    }
}
