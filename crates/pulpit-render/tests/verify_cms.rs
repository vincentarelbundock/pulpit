//! Cryptographic verification of a real signed PDF, per SPEC-signing.md §28.3,
//! plus the §34.2 broken-CMS cases.

mod signing_fixture;

use pulpit_render::verify::{
    self, IdentityAssurance, PadesProfile, SignatureCoverage, SignatureVerification,
};
use signing_fixture::{any_test_credential, build_signed_pdf, MOD_DATE_UNIX};

fn single_status(bytes: &[u8]) -> Box<verify::SignatureStatus> {
    let mut results = verify::verify_signatures(bytes).expect("verification ran");
    assert_eq!(results.len(), 1, "expected exactly one signature");
    match results.remove(0) {
        SignatureVerification::Checked(status) => status,
        SignatureVerification::Broken { field_name, reason } => {
            panic!("signature {field_name} unexpectedly broken: {reason}")
        }
    }
}

#[test]
fn signed_fixture_is_intact_and_valid() {
    let cred = any_test_credential();
    let fixture = build_signed_pdf(&cred);

    let status = single_status(&fixture.bytes);

    assert_eq!(status.coverage, SignatureCoverage::EntireFile);
    assert!(status.intact, "message digest should match the byte range");
    assert!(
        status.valid,
        "signature should verify against the embedded cert"
    );
    assert!(!status.later_revisions);
    assert_eq!(status.field_name, "Sig1");
    assert_eq!(status.profile, Some(PadesProfile::AdbePkcs7Detached));
    assert_eq!(
        status.identity,
        IdentityAssurance::NotVerified {
            reason: "pulpit does not perform certificate path validation"
        },
        "§20.3: identity is never established in this release"
    );
    // The claimed time comes from /M, not from the signing-time attribute,
    // which the fixture deliberately sets to a different instant.
    assert_eq!(status.claimed_time, Some(MOD_DATE_UNIX));
    assert_eq!(status.attested_time, None, "timestamps are deferred to B-T");
    assert!(
        status.algorithm_findings.is_empty(),
        "SHA-256 draws no findings, got {:?}",
        status.algorithm_findings
    );
    assert_eq!(status.digest_algorithm, "SHA-256");
    assert!(!status.cert_chain.is_empty());
    assert_eq!(status.signer_subject, status.signer_cert.subject);
    assert_eq!(status.signer_cert.sha256_fingerprint.len(), 64);
}

#[test]
fn flipping_a_covered_byte_breaks_intact() {
    let cred = any_test_credential();
    let mut fixture = build_signed_pdf(&cred);

    // A byte well inside the first digested span: the page dictionary.
    let target = fixture
        .bytes
        .windows(9)
        .position(|w| w == b"/MediaBox")
        .expect("fixture contains /MediaBox")
        + 1;
    assert!(
        (target as u64) < fixture.sig_start,
        "target must be digested"
    );
    fixture.bytes[target] ^= 0x01;

    let status = single_status(&fixture.bytes);
    assert!(!status.intact, "a modified covered byte must break intact");
}

#[test]
fn flipping_a_signature_byte_breaks_valid_but_not_intact() {
    let cred = any_test_credential();
    let mut fixture = build_signed_pdf(&cred);

    // The last byte of the DER is inside the SignerInfo's `signature` OCTET
    // STRING for every CMS this fixture produces: `signature` is the final
    // field of the final SignerInfo, and no unsigned attributes follow it.
    // Each DER byte occupies two hex digits in the reservation.
    let last_der_byte = fixture.cms_len - 1;
    let hex_pos = fixture.sig_start as usize + 1 + last_der_byte * 2;
    fixture.bytes[hex_pos] = if fixture.bytes[hex_pos] == b'0' {
        b'1'
    } else {
        b'0'
    };

    let status = single_status(&fixture.bytes);
    assert!(
        !status.valid,
        "a corrupted signature must not verify against the certificate"
    );
    // The message-digest attribute is independent of the raw signature bytes,
    // so the document is still reported intact: this is exactly the
    // `intact && !valid` diagnosis §28.3 insists on keeping distinguishable.
    assert!(
        status.intact,
        "corrupting the signature must not change the document digest verdict"
    );
}

#[test]
fn flipping_reservation_padding_changes_nothing() {
    let cred = any_test_credential();
    let mut fixture = build_signed_pdf(&cred);

    // First padding hex digit past the DER (§23.4). Confirm it is padding.
    let pad = fixture.sig_start as usize + 1 + fixture.cms_len * 2;
    assert!(
        pad < fixture.sig_end as usize - 1,
        "reservation has padding"
    );
    assert_eq!(
        fixture.bytes[pad], b'0',
        "byte past the DER must be padding"
    );
    fixture.bytes[pad] = b'7';

    let status = single_status(&fixture.bytes);
    assert!(status.intact, "padding lies outside the digested spans");
    assert!(status.valid, "padding lies outside the DER");
}

// --- §34.2 broken-CMS cases ----------------------------------------------

mod broken_cms {
    use super::*;
    use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
    use cms::content_info::{CmsVersion, ContentInfo};
    use cms::signed_data::{
        CertificateSet, DigestAlgorithmIdentifiers, EncapsulatedContentInfo, SignedData,
        SignerIdentifier, SignerInfo, SignerInfos,
    };
    use der::asn1::{Any, AnyRef, ObjectIdentifier, OctetString, SetOfVec};
    use der::{Decode, Encode};
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::Certificate;

    fn oid(s: &str) -> ObjectIdentifier {
        ObjectIdentifier::new(s).unwrap()
    }

    fn test_certificate() -> Certificate {
        let mut params = rcgen::CertificateParams::new(vec!["broken-cms-test".to_string()]);
        params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
        let cert = rcgen::Certificate::from_params(params).unwrap();
        Certificate::from_der(&cert.serialize_der().unwrap()).unwrap()
    }

    fn signer_info(cert: &Certificate, serial_override: Option<SerialNumber>) -> SignerInfo {
        let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer: cert.tbs_certificate.issuer.clone(),
            serial_number: serial_override
                .unwrap_or_else(|| cert.tbs_certificate.serial_number.clone()),
        });
        SignerInfo {
            version: CmsVersion::V1,
            sid,
            digest_alg: spki::AlgorithmIdentifierOwned {
                oid: oid("2.16.840.1.101.3.4.2.1"),
                parameters: None,
            },
            signed_attrs: None,
            signature_algorithm: spki::AlgorithmIdentifierOwned {
                oid: oid("1.2.840.10045.4.3.2"),
                parameters: None,
            },
            signature: OctetString::new(vec![0u8; 8]).unwrap(),
            unsigned_attrs: None,
        }
    }

    /// Assemble a `ContentInfo(SignedData)` from the given signer infos.
    fn build_cms(cert: &Certificate, signers: Vec<SignerInfo>) -> Vec<u8> {
        let certificates =
            CertificateSet::try_from(vec![CertificateChoices::Certificate(cert.clone())]).unwrap();
        let mut digest_algorithms: DigestAlgorithmIdentifiers = SetOfVec::new();
        digest_algorithms
            .insert(spki::AlgorithmIdentifierOwned {
                oid: oid("2.16.840.1.101.3.4.2.1"),
                parameters: None,
            })
            .unwrap();
        let signed_data = SignedData {
            version: CmsVersion::V1,
            digest_algorithms,
            encap_content_info: EncapsulatedContentInfo {
                econtent_type: oid("1.2.840.113549.1.7.1"),
                econtent: None,
            },
            certificates: Some(certificates),
            crls: None,
            signer_infos: SignerInfos::try_from(signers).unwrap(),
        };
        let der_bytes = signed_data.to_der().unwrap();
        let content_info = ContentInfo {
            content_type: oid("1.2.840.113549.1.7.2"),
            content: Any::from(AnyRef::try_from(der_bytes.as_slice()).unwrap()),
        };
        content_info.to_der().unwrap()
    }

    /// Replace the fixture's CMS with `cms_der`, keeping the reservation and
    /// its padding intact, and verify the result.
    fn verify_with_cms(cms_der: &[u8]) -> SignatureVerification {
        let cred = any_test_credential();
        let mut fixture = build_signed_pdf(&cred);
        let interior = fixture.sig_end as usize - 1 - (fixture.sig_start as usize + 1);
        assert!(cms_der.len() * 2 <= interior, "replacement CMS must fit");
        for i in 0..interior {
            fixture.bytes[fixture.sig_start as usize + 1 + i] = b'0';
        }
        for (i, byte) in cms_der.iter().enumerate() {
            let hex = format!("{:02X}", byte);
            let pos = fixture.sig_start as usize + 1 + i * 2;
            fixture.bytes[pos] = hex.as_bytes()[0];
            fixture.bytes[pos + 1] = hex.as_bytes()[1];
        }
        let mut results = verify::verify_signatures(&fixture.bytes).expect("verification ran");
        assert_eq!(results.len(), 1);
        results.remove(0)
    }

    #[test]
    fn two_signer_infos_is_broken() {
        let cert = test_certificate();
        let cms_der = build_cms(
            &cert,
            vec![signer_info(&cert, None), {
                // A second, distinguishable SignerInfo: same certificate, a
                // different serial, so the SET OF holds two distinct elements.
                let mut other = signer_info(&cert, Some(SerialNumber::new(&[0x02, 0x02]).unwrap()));
                other.signature = OctetString::new(vec![1u8; 8]).unwrap();
                other
            }],
        );

        match verify_with_cms(&cms_der) {
            SignatureVerification::Broken { reason, .. } => {
                assert_eq!(reason, "unsupported signature structure: 2 signers")
            }
            other => panic!("expected broken, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_sid_is_broken() {
        let cert = test_certificate();
        // A serial number no embedded certificate carries.
        let cms_der = build_cms(
            &cert,
            vec![signer_info(
                &cert,
                Some(SerialNumber::new(&[0x7f, 0x7f, 0x7f]).unwrap()),
            )],
        );

        match verify_with_cms(&cms_der) {
            SignatureVerification::Broken { reason, .. } => {
                assert_eq!(reason, "signer certificate not present in the signature")
            }
            other => panic!("expected broken, got {other:?}"),
        }
    }
}
