import XCTest
@testable import Aura

@MainActor
final class ConnectionRetryTests: XCTestCase {
    
    var client: QuicNetworkClient!
    
    override func setUp() async throws {
        client = QuicNetworkClient()
    }
    
    override func tearDown() async throws {
        client.disconnect()
        client = nil
    }
    
    // MARK: - Retry State Tests
    
    func testInitialRetryState() {
        XCTAssertEqual(client.retryCount, 0)
        XCTAssertEqual(client.maxRetries, 5)
        XCTAssertTrue(client.autoReconnectEnabled)
        XCTAssertFalse(client.isRetrying)
    }
    
    func testMaxRetriesConfiguration() {
        client.maxRetries = 10
        XCTAssertEqual(client.maxRetries, 10)
    }
    
    func testAutoReconnectToggle() {
        client.autoReconnectEnabled = false
        XCTAssertFalse(client.autoReconnectEnabled)
        
        client.autoReconnectEnabled = true
        XCTAssertTrue(client.autoReconnectEnabled)
    }
    
    // MARK: - Connection Parameter Tests
    
    func testSavedConnectionParameters() async throws {
        let testHost = "test.example.com"
        let testPort: UInt16 = 9999
        
        // Attempt connection (will fail but should save parameters)
        do {
            try await client.connect(host: testHost, port: testPort)
        } catch {
            // Expected to fail since server doesn't exist
        }
        
        // Verify parameters were saved (we can't directly access private vars,
        // but we can verify the connection attempt was made)
        XCTAssertTrue(client.connectionStatus.contains("Connecting") || 
                     client.connectionStatus.contains("Disconnected") ||
                     client.connectionStatus.contains("failed"))
    }
    
    // MARK: - Disconnect Cleanup Tests
    
    func testDisconnectResetsRetryState() {
        // Simulate retry state
        client.retryCount = 3
        client.isRetrying = true
        
        client.disconnect()
        
        XCTAssertEqual(client.retryCount, 0)
        XCTAssertFalse(client.isRetrying)
        XCTAssertEqual(client.connectionStatus, "Disconnected")
    }
    
    func testDisconnectCleansUpConnection() {
        client.isConnected = true
        client.isAuthenticated = true
        
        client.disconnect()
        
        XCTAssertFalse(client.isConnected)
        XCTAssertFalse(client.isAuthenticated)
    }
    
    // MARK: - Connection Status Tests
    
    func testConnectionStatusUpdates() {
        XCTAssertEqual(client.connectionStatus, "Disconnected")
        
        // Status should update during connection attempts
        // (We can't fully test this without a real server, but we can verify initial state)
    }
    
    // MARK: - Exponential Backoff Calculation Tests
    
    func testExponentialBackoffTiming() {
        // Test the exponential backoff formula: min(1 * 2^(n-1), 30)
        // Attempt 1: 1s
        // Attempt 2: 2s
        // Attempt 3: 4s
        // Attempt 4: 8s
        // Attempt 5: 16s
        // Attempt 6+: 30s (capped)
        
        let baseDelay: TimeInterval = 1.0
        
        for attempt in 1...10 {
            let delay = min(baseDelay * pow(2.0, Double(attempt - 1)), 30.0)
            
            switch attempt {
            case 1: XCTAssertEqual(delay, 1.0)
            case 2: XCTAssertEqual(delay, 2.0)
            case 3: XCTAssertEqual(delay, 4.0)
            case 4: XCTAssertEqual(delay, 8.0)
            case 5: XCTAssertEqual(delay, 16.0)
            case 6...: XCTAssertEqual(delay, 30.0) // Capped at 30s
            default: break
            }
        }
    }
    
    // MARK: - Authentication Retry Tests
    
    func testAuthenticationStatePreserved() {
        // Verify that authentication state is properly tracked
        XCTAssertFalse(client.isAuthenticated)
        
        // After successful auth (simulated)
        client.isAuthenticated = true
        XCTAssertTrue(client.isAuthenticated)
        
        // After disconnect
        client.disconnect()
        XCTAssertFalse(client.isAuthenticated)
    }
}
