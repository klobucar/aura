use aura_server::config::Config;
use aura_server::connection::QuicServer;
use aura_server::db::Database;
use aura_server::state::ServerState;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_acme_certificate_acquisition_with_pebble() {
    // Only run this test if Pebble is detected (e.g. port 14000 is open)
    let pebble_addr = "127.0.0.1:14000";
    if tokio::net::TcpStream::connect(pebble_addr).await.is_err() {
        eprintln!("Skipping ACME test: Pebble not found at {}. Run 'docker-compose -f docker-compose.test.yml up -d' first.", pebble_addr);
        return;
    }

    // Initialize rustls crypto provider (required for rustls 0.23+)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Create a temporary database and config
    let db = Arc::new(Database::open_in_memory().unwrap());
    let mut config = Config::default();

    // Configure for ACME testing
    config.server.acme_domain = Some("localhost".to_string());
    config.server.acme_directory_url = Some("https://localhost:14000/dir".to_string());
    config.server.acme_contact = Some("test@aura.local".to_string());
    config.server.acme_cache_path = Some(std::path::PathBuf::from("target/test_acme_cache"));
    config.server.bind_address = "127.0.0.1:8443".to_string();

    // Ensure cache directory is clean
    if config.server.acme_cache_path.as_ref().unwrap().exists() {
        let _ = std::fs::remove_dir_all(config.server.acme_cache_path.as_ref().unwrap());
    }

    // 2. Wrap state
    let state = Arc::new(ServerState::new(Arc::clone(&db), config.clone()));

    // 3. Start the server
    // Note: This spawns a background task for ACME and binds the UDP socket.
    // It also binds TCP/443 for ALPN challenges.
    // IMPORTANT: This requires permission to bind to privileged ports
    // OR we should remap it for testing.
    // In this test environment, we might need to run as sudo or change the port.
    // However, Pebble is configured to talk to port 443 in our pebble-config.json.

    let bind_addr: std::net::SocketAddr = "127.0.0.1:8443".parse().unwrap();
    let _server = match QuicServer::new(bind_addr, Arc::clone(&state)) {
        Ok(s) => s,
        Err(e) => {
            // If we can't bind 443 (privileged), we might fail here.
            panic!(
                "Failed to create QuicServer (check if you have permission to bind port 443): {}",
                e
            );
        }
    };

    // 4. Wait for certificate acquisition
    // Pebble with PEBBLE_VA_ALWAYS_VALID=1 should issue a cert almost immediately.
    let mut success = false;
    for _ in 0..60 {
        // 60 seconds timeout
        sleep(Duration::from_secs(1)).await;

        // We can check if a cert file was created in the cache
        // or just try to connect to the server via QUIC.
        if config.server.acme_cache_path.as_ref().unwrap().exists() {
            // Check for files in cache
            let entries =
                std::fs::read_dir(config.server.acme_cache_path.as_ref().unwrap()).unwrap();
            if entries.count() > 0 {
                success = true;
                break;
            }
        }
    }

    assert!(success, "ACME certificate was not acquired within timeout");
    println!("✓ ACME certificate acquired successfully using Pebble");
}
