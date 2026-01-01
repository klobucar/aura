# Aura macOS Client Test Suite

## Overview

Comprehensive test suite for the Aura macOS client covering all core functionality including server management, profile management, identity operations, and connection retry logic.

## Test Coverage

### Unit Tests

#### ServerManagerTests (12 tests)
- ✅ CRUD operations (add, update, delete)
- ✅ Recent servers sorting and limiting
- ✅ Favorite servers filtering
- ✅ Persistence across app restarts

#### ProfileManagerTests (13 tests)
- ✅ CRUD operations (create, update, delete)
- ✅ Server linking (many-to-many relationships)
- ✅ Recent profiles sorting and limiting
- ✅ Biometric flag persistence
- ✅ Keychain coordination

#### UserIdentityTests (18 tests)
- ✅ Ed25519 keypair generation
- ✅ Data signing and signature verification
- ✅ Keychain save/load operations
- ✅ Profile import/export (JSON format)
- ✅ Round-trip import/export validation
- ✅ Invalid data handling
- ✅ Display name management

#### ConnectionRetryTests (8 tests)
- ✅ Retry state management
- ✅ Connection parameter preservation
- ✅ Disconnect cleanup
- ✅ Exponential backoff timing validation
- ✅ Authentication state tracking

### Integration Tests (6 tests)
- ✅ Profile-server linking workflows
- ✅ Multiple profiles with multiple servers
- ✅ Import/export with server links
- ✅ Keychain persistence across restarts
- ✅ Full connection flow simulation
- ✅ Data consistency validation

## Running Tests

### Quick Run
```bash
./run-tests.sh
```

### Manual Run
```bash
xcodebuild test \
    -project Aura.xcodeproj \
    -scheme Aura \
    -destination 'platform=macOS' \
    -enableCodeCoverage YES
```

### Run Specific Test Class
```bash
xcodebuild test \
    -project Aura.xcodeproj \
    -scheme Aura \
    -destination 'platform=macOS' \
    -only-testing:AuraTests/ServerManagerTests
```

### Run Specific Test
```bash
xcodebuild test \
    -project Aura.xcodeproj \
    -scheme Aura \
    -destination 'platform=macOS' \
    -only-testing:AuraTests/ServerManagerTests/testAddServer
```

## Test Reports

After running `./run-tests.sh`, reports are generated in the `build/` directory:
- `test-report.html` - Detailed test results
- `coverage.txt` - Code coverage summary

## CI/CD Integration

Add to your CI pipeline:

```yaml
# GitHub Actions example
- name: Run Tests
  run: |
    cd clients/macos
    ./run-tests.sh
```

## Test Data Cleanup

All tests use isolated storage keys and clean up after themselves:
- `TestAuraServerProfiles` - Test server data
- `TestAuraUserProfiles` - Test profile metadata
- Keychain entries are created and deleted per test

## Keychain Testing Notes

Some tests require keychain access:
- Non-biometric keychain tests run on all platforms
- Biometric tests require Secure Enclave (physical Mac or simulator with biometric support)
- Tests will skip biometric operations if Secure Enclave is unavailable

## Adding New Tests

1. Create test file in `AuraTests/`
2. Import `@testable import Aura`
3. Extend `XCTestCase` with `@MainActor` for async support
4. Follow naming convention: `{Feature}Tests.swift`
5. Add setup/teardown for test isolation
6. Use descriptive test names: `test{Action}{Expected}`

Example:
```swift
@MainActor
final class MyFeatureTests: XCTestCase {
    var feature: MyFeature!
    
    override func setUp() async throws {
        feature = MyFeature()
    }
    
    override func tearDown() async throws {
        feature = nil
    }
    
    func testFeatureDoesExpectedThing() {
        // Arrange
        let input = "test"
        
        // Act
        let result = feature.process(input)
        
        // Assert
        XCTAssertEqual(result, "expected")
    }
}
```

## Coverage Goals

- **Target**: 80%+ code coverage for new features
- **Critical paths**: 100% coverage for security-related code (keychain, crypto)
- **UI**: Integration tests for critical user flows

## Troubleshooting

### Tests fail with keychain errors
- Ensure you're running on macOS (not iOS simulator)
- Check keychain access permissions
- Clean test data: `defaults delete com.aura.Aura`

### Tests timeout
- Increase timeout in test settings
- Check for infinite loops or blocking operations

### Flaky tests
- Ensure proper test isolation (setup/teardown)
- Avoid timing-dependent assertions
- Use `XCTestExpectation` for async operations
