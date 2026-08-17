# Reference citations for signing feature

This document records external references used during the implementation of the
cryptographic signing feature (SPEC-signing.md).

## pyHanko

**Repository:** <https://github.com/MatthiasValvekens/pyHanko>

**Pinned commit:** `50eb14218bdb731c62bb136d784a3c581794944a`

**Version:** TODO: determine from git tag at pinned commit

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

**Pinned commit:** TODO: pin alongside pyHanko

**Reference role:** certomancer is used for test PKI only — generating test credentials
and running as a local timestamp authority (TSA) in CI. It is not linked into the
binary and plays no role in production signing.
