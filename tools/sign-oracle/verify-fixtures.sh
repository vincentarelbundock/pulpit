#!/usr/bin/env bash
# Signature verification oracle: runs pyHanko CLI against signed PDF fixtures
#
# Usage: ./verify-fixtures.sh [FIXTURES_DIR]
#
# Exits with status 0 if all signatures validate, nonzero if any validation fails.
# Skips gracefully for optional local runs if pyHanko is unavailable. CI
# treats a missing oracle or fixture as a failed acceptance test.

set -e

FIXTURES_DIR="${1:-.}"
FAILED=0
PASSED=0
SKIPPED=0

# Check if pyHanko is available
if ! command -v pyhanko &> /dev/null; then
    if [ "${CI:-}" = "true" ]; then
        echo "error: pyHanko CLI not found in CI"
        exit 1
    fi
    echo "skipped: pyHanko CLI not found (install with: pip install -r requirements.txt)"
    exit 0
fi

# Check if the fixtures directory exists
if [ ! -d "$FIXTURES_DIR" ]; then
    if [ "${CI:-}" = "true" ]; then
        echo "error: fixtures directory '$FIXTURES_DIR' does not exist in CI"
        exit 1
    fi
    echo "skipped: fixtures directory '$FIXTURES_DIR' does not exist"
    exit 0
fi

# Find all PDFs in the fixtures directory
pdf_files=$(find "$FIXTURES_DIR" -type f -name "*.pdf" 2>/dev/null || true)

if [ -z "$pdf_files" ]; then
    if [ "${CI:-}" = "true" ]; then
        echo "error: no *.pdf files found in '$FIXTURES_DIR' in CI"
        exit 1
    fi
    echo "skipped: no *.pdf files found in '$FIXTURES_DIR'"
    exit 0
fi

echo "Validating signatures in: $FIXTURES_DIR"
echo

# Validate each PDF
for pdf_file in $pdf_files; do
    # Use pyHanko's sign validate subcommand to check signature validity.
    # pyHanko outputs a status line with format: <INTEGRITY>:<TRUST>,<MODIFICATION>
    # Examples: "INTACT:TRUSTED,UNTOUCHED", "INTACT:UNTRUSTED,UNTOUCHED"
    # See: pyHanko sign validate --help
    echo -n "Validating $(basename "$pdf_file")... "

    # Capture full output and exit code
    # Use 'set +e' temporarily to allow pyhanko to fail without exiting the script
    set +e
    output=$(pyhanko sign validate "$pdf_file" 2>&1)
    pyhanko_exit=$?
    set -e

    # Status line format: "Sig1:....:INTACT:UNTRUSTED,UNTOUCHED" or similar
    # Extract lines containing INTACT status
    if echo "$output" | grep -q "INTACT:"; then
        # INTACT means the signature is structurally valid and hasn't been tampered with.
        # TRUSTED/UNTRUSTED is a separate concern (self-signed certs are untrusted by default).
        # For testing purposes, INTACT is sufficient (even if the certificate is untrusted).
        echo "PASS"
        PASSED=$((PASSED + 1))
    else
        # Check if there's any INVALID/CORRUPT status
        if echo "$output" | grep -q "INVALID:\|CORRUPT:"; then
            echo "FAIL"
            echo "  Signature integrity check failed"
            echo "$output" | sed 's/^/    /'
            FAILED=$((FAILED + 1))
        else
            # No clear status line found; use the exit code
            if [ $pyhanko_exit -eq 0 ]; then
                echo "PASS (no INVALID status)"
                PASSED=$((PASSED + 1))
            else
                echo "FAIL (pyhanko exit code: $pyhanko_exit)"
                echo "  Output:"
                echo "$output" | sed 's/^/    /'
                FAILED=$((FAILED + 1))
            fi
        fi
    fi
done

echo
echo "Results: $PASSED passed, $FAILED failed"
exit "$FAILED"
