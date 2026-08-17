#!/usr/bin/env bash
# Signature verification oracle: runs pyHanko CLI against signed PDF fixtures
#
# Usage: ./verify-fixtures.sh [FIXTURES_DIR]
#
# Exits with status 0 if all signatures validate, nonzero if any validation fails.
# Skips gracefully if pyHanko is unavailable.

set -e

FIXTURES_DIR="${1:-.}"
FAILED=0
PASSED=0
SKIPPED=0

# Check if pyHanko is available
if ! command -v pyhanko &> /dev/null; then
    echo "skipped: pyHanko CLI not found (install with: pip install -r requirements.txt)"
    exit 0
fi

# Check if the fixtures directory exists
if [ ! -d "$FIXTURES_DIR" ]; then
    echo "skipped: fixtures directory '$FIXTURES_DIR' does not exist"
    exit 0
fi

# Find all PDFs in the fixtures directory
pdf_files=$(find "$FIXTURES_DIR" -type f -name "*.pdf" 2>/dev/null || true)

if [ -z "$pdf_files" ]; then
    echo "skipped: no *.pdf files found in '$FIXTURES_DIR'"
    exit 0
fi

echo "Validating signatures in: $FIXTURES_DIR"
echo

# Validate each PDF
for pdf_file in $pdf_files; do
    # Use pyHanko's sign validate subcommand
    # See: pyHanko sign validate --help
    echo -n "Validating $(basename "$pdf_file")... "

    if pyhanko sign validate "$pdf_file" 2>&1 | tail -1 | grep -q "signature is valid\|valid digital signature"; then
        echo "PASS"
        ((PASSED++))
    else
        # Check the actual exit code and output more carefully
        if pyhanko sign validate "$pdf_file" > /dev/null 2>&1; then
            echo "PASS"
            ((PASSED++))
        else
            echo "FAIL"
            echo "  Error details:"
            pyhanko sign validate "$pdf_file" 2>&1 | sed 's/^/    /'
            ((FAILED++))
        fi
    fi
done

echo
echo "Results: $PASSED passed, $FAILED failed"
exit "$FAILED"
