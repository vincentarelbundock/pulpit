# Sign Oracle — PDF signature validation harness

A CI integration test harness for the cryptographic signing feature.
Validates that pulpit's signed PDF output is verifiable by pyHanko,
the reference implementation of PDF Advanced Electronic Signatures (PAdES).

## Contents

- `requirements.txt` — pinned Python dependencies; pyHanko itself is pinned
  to an exact source commit so specification citations remain reproducible
- `gen-credentials.py` — generates test PKCS#12 credentials
- `verify-fixtures.sh` — validates signed PDFs with pyHanko CLI
- `credentials/` — generated test certificates (created by setup)
- `fixtures/` — signed PDF test files (created by tests, validated by CI)

## Setup

Create a Python venv and install dependencies:

```bash
python3 -m venv .venv-sign-oracle
source .venv-sign-oracle/bin/activate  # or on Windows: .venv-sign-oracle\Scripts\activate
pip install -r requirements.txt
```

Or use the Makefile convenience target:

```bash
make sign-oracle-setup
```

## Generate test credentials

Run the credential generator to create test PKCS#12 files with password "test":

```bash
python3 gen-credentials.py
```

This creates:

- `credentials/test-self-signed.p12` — a self-signed certificate
- `credentials/test-chain-2-level.p12` — a 2-level certificate chain (leaf + intermediate + root)

Both are deterministic and intended for fixture generation; do not commit the
generated files to the repository.

## Validate signed PDFs

Run pyHanko's signature validator against all PDFs in a directory:

```bash
./verify-fixtures.sh /path/to/fixtures
```

The script:
- Skips gracefully if pyHanko is unavailable (exit 0)
- Skips gracefully if the directory does not exist (exit 0)
- Fails instead of skipping either condition when `CI=true`
- Exits nonzero if any signature validation fails
- Runs in CI as part of signing feature integration tests

Or use the Makefile target to validate fixtures in the default location:

```bash
make sign-oracle
```

## Workflow

### In development

1. Set up the venv: `make sign-oracle-setup`
2. Generate credentials: `python3 tools/sign-oracle/gen-credentials.py`
3. Use the credentials to produce signed PDF fixtures in your signing code
4. Validate with: `make sign-oracle` (validates `tools/sign-oracle/fixtures/`)

### In CI

The `make sign-oracle` target is expected to run after fixture generation:

```bash
make sign-oracle-setup       # runs once: create venv, pip install
python3 tools/sign-oracle/gen-credentials.py
<test runner produces signed PDFs into tools/sign-oracle/fixtures/>
make sign-oracle             # validate all fixtures
```

## References

- **pyHanko**: <https://github.com/MatthiasValvekens/pyHanko> — PAdES reference implementation
- **certomancer**: <https://github.com/MatthiasValvekens/certomancer> — test PKI generation
- **SPEC-signing.md**: `SPEC-signing.md` §34.2 (integration test oracle requirement)
