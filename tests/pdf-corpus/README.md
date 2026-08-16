# PDF interoperability corpus

This is a small, redistributable corpus for exercising PDF rendering and
document handling. It intentionally covers interactive and awkward features
rather than ordinary slide decks:

| Directory | Coverage |
| --- | --- |
| `acroform/` | buttons, combo boxes, list boxes, multiline text, mixed fields |
| `xfa/` | simple XFA and multipage XFA content |
| `annotations/` | general, ink, and file-attachment annotations |
| `signatures/` | signature dictionaries and multiple signatures |
| `encrypted/` | Standard Security Handler revisions 2 and 6 |
| `malformed/` | deliberately invalid annotation data |
| `javascript/` | annotation JavaScript action |

## Provenance and licence

Every PDF is an unmodified fixture from the PDFium repository at revision
`1348385bb7c8dbc0d667d4b00f038a1d4684a196`, retrieved on 2026-08-16 from
`testing/resources/`. PDFium's source headers identify the work as governed by
the BSD-style licence in its repository-level `LICENSE`; that file also
contains the Apache-2.0 text. The complete upstream file is reproduced
verbatim as `LICENSES/PDFIUM-LICENSE.txt`, without trying to narrow its terms.

`MANIFEST.toml` records the upstream path, purpose, and SHA-256 digest of each
file. Verify the archive after copying or downloading it with:

```sh
sha256sum --check SHA256SUMS
```

The PDFs are test inputs, not trusted documents. In particular, this corpus
contains JavaScript, an embedded-file annotation, encryption, signatures, and
malformed input. Open it only in the same sandboxed or supervised environment
used for untrusted PDFs.

## Selection policy

Only files whose redistribution is covered by an explicit upstream licence
are included. Samples from PDF.js issues, iText examples, government sites,
and corpus indexes were excluded because repository presence or public
downloadability alone does not establish a redistribution grant.
