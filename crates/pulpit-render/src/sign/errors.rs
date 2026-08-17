#![forbid(unsafe_code)]
//! Error types for the signing module. Per §33, every error type must state what
//! failed, that the source is unchanged, and what to do next.

use thiserror::Error;

/// Signing errors as specified in SPEC-signing.md §33.
#[derive(Debug, Error)]
pub enum SigningError {
    #[error("Could not load PKCS#12 credential: {0}")]
    KeyLoadFailed(String),

    #[error("Wrong passphrase for PKCS#12 credential")]
    WrongPassphrase,

    #[error("Unsupported key algorithm: {algorithm}")]
    UnsupportedKeyAlgorithm { algorithm: String },

    #[error(
        "Digest mechanism mismatch: algorithm implies {implied} but {requested} was requested"
    )]
    DigestMechanismMismatch { implied: String, requested: String },

    #[error("CMS construction failed: {0}")]
    CmsConstructionFailed(String),

    #[error("Signature too large: reserved {reserved} bytes but need {required}")]
    SignatureTooLarge { reserved: usize, required: usize },

    #[error("Invalid certificate: {0}")]
    InvalidCertificate(String),

    #[error("DER encoding failed: {0}")]
    DerEncodingFailed(String),

    #[error("Signature operation failed: {0}")]
    SignatureOperationFailed(String),
}

// DER error handling - handled by other error types
