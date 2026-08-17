# Reference citations for signing feature

This document records external references used during the implementation of the
cryptographic signing feature (SPEC-signing.md).

## pyHanko

**Repository:** <https://github.com/MatthiasValvekens/pyHanko>

**Version:** 0.36.2

**Pinned commit (release tag):** `0945f9bc64ef6ef386500943ee2b5941b5f142cd` (v0.36.2)

**Reference role:** pyHanko (MIT, Matthias Valvekens) is a reference implementation
of PAdES (PDF Advanced Electronic Signatures). Sections of SPEC-signing.md cite
specific lines in pyHanko source as the basis for PDF signing decisions and algorithms.
The porting policy (SPEC-signing §35.0) permits deriving algorithms and decisions from
pyHanko while re-implementing them in Rust. Ported code lives in:

- `crates/pulpit-render/src/sign/` — CMS and signing logic
- `crates/pulpit-render/src/pdfwrite/` — incremental PDF update writer
- `crates/pulpit-render/src/verify/` — signature discovery, coverage, and integrity verification

## certomancer

**Repository:** <https://github.com/MatthiasValvekens/certomancer>

**Version:** 0.16.0

**Pinned commit (release tag):** `8f5c58c566daefbdf91b8f2085b69b791930343a` (v0.16.0)

**Reference role:** certomancer is used for test PKI only — generating test credentials
and running as a local timestamp authority (TSA) in CI. It is not linked into the
binary and plays no role in production signing.
