#!/bin/bash

# Aura macOS Client Test Runner
# Run all tests and generate coverage report

set -e

echo "🧪 Running Aura macOS Client Tests..."
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Navigate to project directory
cd "$(dirname "$0")"

# Clean build folder
echo "🧹 Cleaning build folder..."
xcodebuild clean -project Aura.xcodeproj -scheme Aura -quiet

# Run tests
echo ""
echo "🚀 Running unit tests..."
xcodebuild test \
    -project Aura.xcodeproj \
    -scheme Aura \
    -destination 'platform=macOS' \
    -enableCodeCoverage YES \
    -quiet \
    | xcpretty --color --report html --output build/test-report.html

# Check test result
if [ ${PIPESTATUS[0]} -eq 0 ]; then
    echo ""
    echo -e "${GREEN}✅ All tests passed!${NC}"
    echo ""
    
    # Generate coverage report
    echo "📊 Generating coverage report..."
    xcrun xccov view --report --only-targets \
        $(find ~/Library/Developer/Xcode/DerivedData -name "*.xcresult" | head -1)/*/action.xccovreport \
        > build/coverage.txt
    
    echo ""
    echo "Coverage Summary:"
    cat build/coverage.txt
    echo ""
    echo -e "${GREEN}Test report: build/test-report.html${NC}"
    echo -e "${GREEN}Coverage report: build/coverage.txt${NC}"
else
    echo ""
    echo -e "${RED}❌ Tests failed!${NC}"
    echo ""
    echo "Check build/test-report.html for details"
    exit 1
fi
