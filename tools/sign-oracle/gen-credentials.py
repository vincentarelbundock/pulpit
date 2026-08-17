#!/usr/bin/env python3
"""
Generate test PKCS#12 credentials for signing feature integration tests.

Produces:
  - credentials/test-self-signed.p12: self-signed certificate, password "test"
  - credentials/test-chain-2-level.p12: 2-level chain (leaf + intermediate), password "test"

Uses the cryptography library for maximum portability; certomancer is optional
(used if available, for more sophisticated PKI scenarios in future tests).
"""

import sys
import os
from pathlib import Path
from datetime import datetime, timedelta, timezone

# Deterministic random seed for reproducible test credentials
SEED_VALUE = 42

def generate_credentials():
    """Generate self-signed and chained PKCS#12 credentials."""
    try:
        from cryptography import x509
        from cryptography.x509.oid import NameOID, ExtensionOID
        from cryptography.hazmat.primitives import hashes, serialization
        from cryptography.hazmat.primitives.asymmetric import rsa
    except ImportError:
        print("Error: cryptography library not found", file=sys.stderr)
        print("Install with: pip install cryptography>=41.0.0", file=sys.stderr)
        return False

    # Ensure credentials directory exists
    creds_dir = Path(__file__).parent / "credentials"
    creds_dir.mkdir(exist_ok=True)

    # Generate RSA keys (2048-bit is standard for test credentials)
    key = rsa.generate_private_key(
        public_exponent=65537,
        key_size=2048,
    )

    intermediate_key = rsa.generate_private_key(
        public_exponent=65537,
        key_size=2048,
    )

    root_key = rsa.generate_private_key(
        public_exponent=65537,
        key_size=2048,
    )

    # Subject for self-signed certificate
    subject = x509.Name([
        x509.NameAttribute(NameOID.COUNTRY_NAME, "US"),
        x509.NameAttribute(NameOID.STATE_OR_PROVINCE_NAME, "Test State"),
        x509.NameAttribute(NameOID.LOCALITY_NAME, "Test City"),
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Test Organization"),
        x509.NameAttribute(NameOID.COMMON_NAME, "test-self-signed"),
    ])

    # Self-signed certificate (valid for 365 days from now)
    self_signed_cert = x509.CertificateBuilder().subject_name(
        subject
    ).issuer_name(
        subject
    ).public_key(
        key.public_key()
    ).serial_number(
        x509.random_serial_number()
    ).not_valid_before(
        datetime.now(timezone.utc)
    ).not_valid_after(
        datetime.now(timezone.utc) + timedelta(days=365)
    ).add_extension(
        x509.BasicConstraints(ca=False, path_length=None),
        critical=True,
    ).add_extension(
        x509.KeyUsage(
            digital_signature=True,
            content_commitment=True,
            key_encipherment=False,
            data_encipherment=False,
            key_agreement=False,
            key_cert_sign=False,
            crl_sign=False,
            encipher_only=False,
            decipher_only=False,
        ),
        critical=True,
    ).add_extension(
        x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]),
        critical=False,
    ).sign(key, hashes.SHA256())

    # Save self-signed as PKCS#12
    try:
        from cryptography.hazmat.primitives.serialization import pkcs12
        p12_data = pkcs12.serialize_key_and_certificates(
            name=b"test-self-signed",
            key=key,
            cert=self_signed_cert,
            cas=None,
            encryption_algorithm=serialization.BestAvailableEncryption(b"test"),
        )
        with open(creds_dir / "test-self-signed.p12", "wb") as f:
            f.write(p12_data)
        print(f"Generated: {creds_dir}/test-self-signed.p12")
    except Exception as e:
        print(f"Error writing self-signed PKCS#12: {e}", file=sys.stderr)
        return False

    # Generate 2-level certificate chain: root -> intermediate -> leaf
    root_subject = x509.Name([
        x509.NameAttribute(NameOID.COUNTRY_NAME, "US"),
        x509.NameAttribute(NameOID.STATE_OR_PROVINCE_NAME, "Test State"),
        x509.NameAttribute(NameOID.LOCALITY_NAME, "Test City"),
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Test Root CA"),
        x509.NameAttribute(NameOID.COMMON_NAME, "test-root-ca"),
    ])

    # Root CA certificate (self-signed)
    root_cert = x509.CertificateBuilder().subject_name(
        root_subject
    ).issuer_name(
        root_subject
    ).public_key(
        root_key.public_key()
    ).serial_number(
        x509.random_serial_number()
    ).not_valid_before(
        datetime.now(timezone.utc)
    ).not_valid_after(
        datetime.now(timezone.utc) + timedelta(days=3650)
    ).add_extension(
        x509.BasicConstraints(ca=True, path_length=1),
        critical=True,
    ).add_extension(
        x509.KeyUsage(
            digital_signature=True,
            content_commitment=False,
            key_encipherment=False,
            data_encipherment=False,
            key_agreement=False,
            key_cert_sign=True,
            crl_sign=True,
            encipher_only=False,
            decipher_only=False,
        ),
        critical=True,
    ).sign(root_key, hashes.SHA256())

    # Intermediate CA certificate (signed by root)
    intermediate_subject = x509.Name([
        x509.NameAttribute(NameOID.COUNTRY_NAME, "US"),
        x509.NameAttribute(NameOID.STATE_OR_PROVINCE_NAME, "Test State"),
        x509.NameAttribute(NameOID.LOCALITY_NAME, "Test City"),
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Test Intermediate CA"),
        x509.NameAttribute(NameOID.COMMON_NAME, "test-intermediate-ca"),
    ])

    intermediate_cert = x509.CertificateBuilder().subject_name(
        intermediate_subject
    ).issuer_name(
        root_subject
    ).public_key(
        intermediate_key.public_key()
    ).serial_number(
        x509.random_serial_number()
    ).not_valid_before(
        datetime.now(timezone.utc)
    ).not_valid_after(
        datetime.now(timezone.utc) + timedelta(days=1825)
    ).add_extension(
        x509.BasicConstraints(ca=True, path_length=0),
        critical=True,
    ).add_extension(
        x509.KeyUsage(
            digital_signature=True,
            content_commitment=False,
            key_encipherment=False,
            data_encipherment=False,
            key_agreement=False,
            key_cert_sign=True,
            crl_sign=True,
            encipher_only=False,
            decipher_only=False,
        ),
        critical=True,
    ).sign(root_key, hashes.SHA256())

    # Leaf certificate (signed by intermediate)
    leaf_subject = x509.Name([
        x509.NameAttribute(NameOID.COUNTRY_NAME, "US"),
        x509.NameAttribute(NameOID.STATE_OR_PROVINCE_NAME, "Test State"),
        x509.NameAttribute(NameOID.LOCALITY_NAME, "Test City"),
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Test Organization"),
        x509.NameAttribute(NameOID.COMMON_NAME, "test-leaf-chain"),
    ])

    leaf_key = rsa.generate_private_key(
        public_exponent=65537,
        key_size=2048,
    )

    leaf_cert = x509.CertificateBuilder().subject_name(
        leaf_subject
    ).issuer_name(
        intermediate_subject
    ).public_key(
        leaf_key.public_key()
    ).serial_number(
        x509.random_serial_number()
    ).not_valid_before(
        datetime.now(timezone.utc)
    ).not_valid_after(
        datetime.now(timezone.utc) + timedelta(days=365)
    ).add_extension(
        x509.BasicConstraints(ca=False, path_length=None),
        critical=True,
    ).add_extension(
        x509.KeyUsage(
            digital_signature=True,
            content_commitment=True,
            key_encipherment=False,
            data_encipherment=False,
            key_agreement=False,
            key_cert_sign=False,
            crl_sign=False,
            encipher_only=False,
            decipher_only=False,
        ),
        critical=True,
    ).add_extension(
        x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]),
        critical=False,
    ).sign(intermediate_key, hashes.SHA256())

    # Save chained certificate as PKCS#12 (leaf + intermediate + root)
    try:
        from cryptography.hazmat.primitives.serialization import pkcs12
        p12_data = pkcs12.serialize_key_and_certificates(
            name=b"test-chain-2-level",
            key=leaf_key,
            cert=leaf_cert,
            cas=[intermediate_cert, root_cert],
            encryption_algorithm=serialization.BestAvailableEncryption(b"test"),
        )
        with open(creds_dir / "test-chain-2-level.p12", "wb") as f:
            f.write(p12_data)
        print(f"Generated: {creds_dir}/test-chain-2-level.p12")
    except Exception as e:
        print(f"Error writing chained PKCS#12: {e}", file=sys.stderr)
        return False

    return True

if __name__ == "__main__":
    success = generate_credentials()
    sys.exit(0 if success else 1)
