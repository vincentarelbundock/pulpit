# Adversarial review: pulpit

**Reviewed at:** `302c75b` (main), 2026-08-18
**Scope:** ~140k LOC across six crates. Signing/verification stack, IPC and
shared-memory layer, PDFium FFI, display reconciler, platform boundary, app
entry points. Suite run (1845 pass / 1 fail), clippy clean, findings proven
with throwaway probes rather than asserted.

**Line numbers** refer to the committed tree as reviewed (`302c75b`..`70bd669`,
clean). Fixes for several findings were already landing in the working tree of
branch `review-fixes` while this was being written, so offsets will have moved.

**Verdict up front, because it matters for how you read the rest:** this is not
idiot code. `verify/cms_check.rs` is better CMS verification than most shipping
PDF viewers — it refuses to guess a primitive from the certificate, recovers the
*original* `signedAttrs` bytes instead of re-serialising, rejects duplicate
security-critical attributes, and honestly reports `NotVerified` with a test
asserting the word "valid" never appears unqualified in the UI.
`verify/objects.rs` is bounded in depth, entry count, stream bytes, and
object-stream count.

Which is exactly why the findings below are worth taking seriously: the failures
are **not** where the author was paying attention. They cluster in a second,
sloppier PDF parser sitting right next to the careful one, and at the OS
boundary where the discipline visibly stops.

---

## 1. CRITICAL — Memory-exhaustion DoS on every document open

`crates/pulpit-render/src/verify/mod.rs:437` and `:528`

```rust
for obj_num in first..(first + count) {   // first, count: u32, straight from the file
    obj_numbers.insert(obj_num);
    let _ = tokenizer.next_token();       // return value discarded
}
```

The xref subsection header's `count` is attacker-controlled and never validated
against how many entries actually follow. The loop inserts into a `HashSet`
regardless of whether the tokenizer has anything left.

**Reproduced.** A ~120-byte file:

```
%PDF-1.4
1 0 obj
<< /Type /Catalog >>
endobj
xref
0 4294967290
trailer
<< /Root 1 0 R /Size 1 >>
startxref
<off>
%%EOF
```

→ 643 MB RSS at 44 s, **1.65 GB at 64 s**, still climbing when killed.

This is not confined to signing. `crates/pulpit/src/app.rs:5127` calls
`verify_signatures` on **every document open**, on the `document-open` thread
**inside the main process**. Rule 3 ("the audience frame is never worse than it
was; rendering happens in supervised child processes") does not protect this —
the OOM killer takes the presenter mid-talk. Emailing someone a deck is a remote
trigger.

`first + count` and `first + i` also overflow: panic in debug, wrap in release.

The part that should sting: `crates/pulpit-render/fuzz/fuzz_targets/fuzz_revision_map.rs`
exists and its comment reads *"must return Ok or typed Err, never panic."* There
is no `corpus/` directory. libFuzzer would have found this in under a second.
**The fuzz target was written and never run.** That is worse than not having one
— it is a green checkmark over an untested claim.

**Fix:** bound `count` by the remaining file size (a classic entry is 20 bytes; a
stream row is `sum(W)`), and use `checked_add`. Then actually run
`cargo fuzz run fuzz_revision_map` and commit a corpus.

---

## 2. CRITICAL — Rendered slides are world-readable in `/dev/shm`, and they leak

`crates/pulpit-render/src/shm.rs:49-63`

```rust
let file = OpenOptions::new().read(true).write(true)
    .create(true).truncate(true)      // NOT create_new
    .open(&path)?;                    // no .mode(0o600)
```

**Observed on the development machine during this review:**

```
201 files, 2.0 GB resident in tmpfs
mode -rw-r--r--, in /dev/shm (drwxrwxrwt)
oldest: Aug 14 — four days old
sampled contents: ~99% non-zero over 2 MB — real pixel data
```

Three separate problems in one function:

- **Disclosure.** Every rendered frame is readable by any local user, during the
  talk and for days afterwards.
- **Leak.** `Drop` unlinks, but `Drop` does not run when a worker is killed or the
  app crashes. 2 GB of RAM, unbounded across runs.
  `regions_grow_and_are_unlinked_on_drop` passes and asserts *"regions do not leak
  into /dev/shm"* — it tests the one path that was never in doubt.
- **Substitution.** Names are `pulpit-<pid>-<n>`, fully predictable. `create(true)`
  opens a pre-planted file instead of refusing it; the sticky bit stops deletion,
  not creation. A local attacker plants the region, then reads *and writes* the
  buffer the audience window renders from. `AttachedRegion::open` maps
  read-**write**; `read_only()` exists and is dead code.

Compare `sign/apply.rs:1093-1110`, where the same author writes `O_EXCL` +
`0o600` + a `RandomState`-derived name and documents exactly why. The discipline
is present in the codebase; it just did not reach this file.

**Fix:** `create_new(true)` + `.mode(0o600)`; unlink immediately after mapping
(POSIX shm semantics — the mapping survives, the name does not) and pass the fd,
or at minimum sweep stale `pulpit-*` at startup. Use `read_only()` on the reading
side.

---

## 3. HIGH — Instant panic on a non-UTF-8 filename

`crates/pulpit/src/main.rs:53` — `std::env::args()` panics on non-Unicode
arguments.

**Reproduced against `target/release/pulpit`:**

```
$ pulpit "$(printf '/tmp/caf\xff-deck.pdf')"
thread 'main' panicked at library/std/src/env.rs:876:51:
called `Result::unwrap()` on an `Err` value: "/tmp/caf\xFF-deck.pdf"
```

On Linux a filename is arbitrary bytes. A Latin-1 name from a file manager, a USB
stick, or a colleague's export kills the app before it draws a pixel.

Fixing `args()` alone is not enough — the same assumption is baked in downstream:

- `crates/pulpit-render/src/document/session.rs:64` —
  `format!("{flag}={}", source.display())`. `Path::display()` is **lossy**; the
  worker receives a mangled path.
- `crates/pulpit-media/src/runtime/chromium.rs:332` — `--user-data-dir={}` with
  `.display()`. Same.

Use `args_os()` and build child arguments as `OsString`.

Adjacent, same lines: `main.rs:65` and `:76` use
`trim_start_matches("--document-worker=")`, which strips *all* repeated leading
occurrences. `strip_prefix` is what is meant.

---

## 4. HIGH — Unbounded recursion in the signing path's parser

`crates/pulpit-render/src/sign/apply.rs:1219`

`parse_value` recurses on `<<` and `[` with **no depth limit**. It is reached from
`existing_field_names` / `find_field_object` — the *first* thing `plan_revision`
does — over field dictionaries taken verbatim from the document you were handed.
Deeply nested arrays → stack overflow → SIGSEGV.

`verify/objects.rs:46` defines `MAX_VALUE_DEPTH: usize = 64` and threads `depth`
through every call. The sign path has its own parser with none of that. Two
parsers, one lesson learned.

(Not executed — it needs a loaded credential — but the code is unambiguous.)

---

## 5. HIGH — Signing silently corrupts non-ASCII text

`crates/pulpit-render/src/sign/apply.rs:1257` — `String::from_utf8_lossy(token)`
is applied to every value token before re-emission.

**Reproduced:**

```
source token : (Caf\xE9)                 <- PDFDocEncoded "Café"
re-emitted   : <FEFF004300610066FFFD>    <- "Caf\u{FFFD}"
```

The é is gone. This hits the catalog, AcroForm, page node, and field
dictionaries — every object the signing revision re-emits. The §32 gate does
**not** catch it: it verifies signature validity and prefix identity, not content
fidelity. So you sign a document, quietly damage its text, then certify the
damaged version.

Related, same root cause: `find_field_object` (`:1367`) compares
`field_name.as_slice() == name.as_bytes()` — raw PDF bytes against a lossy-UTF-8
name from `find_field_tree`. **A signature field with a non-ASCII name can never
be located, so it can never be signed.**

---

## 6. HIGH — A malformed signature vanishes instead of reading "broken"

`crates/pulpit-render/src/verify/mod.rs:646`

```rust
if let Ok(Some(sig_report)) = extract_signature_field(bytes, field, revisions) {
```

Both `Err` and `Ok(None)` are silently dropped. A `/ByteRange` with anything other
than 4 elements makes `extract_sig_dict_info` return `Err`, and the field
disappears from the report entirely.

The UI is otherwise scrupulously honest — `IDENTITY_DISCLOSURE`,
`SignedIdentityNotVerified`, a test asserting "valid"/"verified" never appear
unqualified. This is the one hole in it: tamper with a signature the right way and
the document renders as **unsigned** rather than **broken**. That is the attack
every signature UI is supposed to defend against, and it is the only place the
presentation layer is not defending it.

Make undecodable-but-present signature fields produce
`SignatureVerification::Broken`, never absence.

---

## Medium

| # | Finding | Location |
|---|---|---|
| 7 | **`parse_xref_stream_entries` is a stub.** Never decodes the stream; `_w_widths` is computed then discarded; a stream with no `/Index` (the common case for PDF 1.5+) yields an *empty* object set. So `last_changed_revision` returns `None` for most modern PDFs, `sig_dict_rev` falls back to `0`, and coverage silently degrades. The comment says "deferred" — it is shipped. | `verify/mod.rs:453-529` |
| 8 | **`find_prev` stops at the first `>>`.** A trailer with a nested dictionary before `/Prev` truncates the revision chain to one revision. | `verify/mod.rs:341-397` |
| 9 | **`RevisionInfo.eof` is documented as "byte just past %%EOF" for that revision** but is assigned `file_size` for every revision. Currently unread — a trap set for the next person. | `verify/mod.rs:130,186` |
| 10 | **Single-instance lock is a read-then-create race.** Two simultaneous launches both read no pid and both `Acquired` — the exact failure the module exists to prevent. Pid reuse also makes it refuse to start, the outcome its own doc comment calls "a worse failure". Use `flock`. | `platform/instance.rs:44` |
| 11 | **`unguessable_token()`'s comment is false and its fallback is predictable.** It says "128 bits from the OS, via `getrandom`" — it opens `/dev/urandom` as a file and **discards the read result**. The "never fall back to something predictable silently" fallback is 4 bytes of `subsec_nanos` + 4 of pid + 8 zero bytes ≈ 30 bits, behind a `tracing::warn!`. `getrandom` is already in `Cargo.lock` three times over, so the "no dependency available" premise is wrong. | `runtime/chromium.rs:1020` |
| 12 | **CDP pipes created without `O_CLOEXEC`** (`libc_pipe` = bare `pipe(2)`), so they leak into every subsequently spawned child — render workers, document workers, `systemd-inhibit`, `kill`. A leaked write end means the browser never sees EOF and does not exit. Also racy against concurrent spawns. Use `pipe2(O_CLOEXEC)`. Separately, the `dup2` comment "clears CLOEXEC on the duplicates" is wrong when `oldfd == newfd` — dup2 is a no-op there. | `runtime/chromium.rs:607, 341` |
| 13 | **`make test` is red and hides 1500 tests.** `wayland.rs:369` guards only on `WAYLAND_DISPLAY`, not on whether libwayland loads. Without `--no-fail-fast`, `cargo test --workspace` stops at **322 of 1846**. The Makefile target says "no display required"; CLAUDE.md repeats it. Guard on `WaylandBackend::connect()` succeeding, and add `--no-fail-fast`. | `pulpit-display/src/wayland.rs:369`, `Makefile:114` |
| 14 | **Asset origin serves one connection at a time with no write timeout.** Any local process can stall media 5 s per connection, or indefinitely by connecting and never reading. | `runtime/chromium.rs:700-712` |
| 15 | **Layout store claims "same atomic dance as the settings store" — it does not fsync.** The settings store fsyncs the file *and* the directory (`settings/store.rs:118-125`); the layout store does `fs::write` + `rename`. A comment asserting a durability property the code does not have. | `layout/store.rs:413-416` |

---

## Low / notes

- `extract_rsa_bits(...).unwrap_or(2048)` — an unparseable key is silently
  reported as 2048-bit. Fine for display; a hazard if key strength ever becomes a
  gate. (`sign/credential.rs:316`)
- `PdfObject::Name` is emitted without `#` escaping. Not reachable today because
  `PdfTokenizer` does not decode escapes — one parser improvement away from
  producing corrupt dictionaries. (`pdfwrite/mod.rs:290`)
- `environment<'a>()` mints a `&mut FormEnvironment` with an unbounded lifetime.
  Every call site checked correctly ends the borrow before re-entering PDFium, but
  nothing enforces it and the `# Safety` block does not mention aliasing.
  (`document/form.rs:497`)
- `find_token_in_slice` re-searches for the `/Contents` bytes instead of using the
  tokenizer's known position — a decoy hex string earlier in the dictionary
  redirects the extent. (`verify/mod.rs:1188`)
- `extract_docmdp_level` re-tokenizes the whole dictionary per token;
  `parse_xref_extent`'s stream branch tokenizes to EOF per revision (×1024);
  `find_object`/`find_object_offset` build a fresh `ObjectResolver` — a full xref
  parse — per call, per field. All quadratic.
- No minimum RSA modulus; SHA-1 is a `finding`, not a refusal. Defensible given no
  path validation, worth stating explicitly.
- PKCS#12 error classification via `format!("{:?}", e).contains("decrypt")` — a
  substring match on a Debug format. Breaks silently on an upstream rename.
  (`sign/credential.rs:120`)
- `CString::new(...).expect(...)` throughout `worker/mpv.rs` — a NUL in a
  PDF-derived media option panics the worker.
- Three versions each of `getrandom` and `rand` in the lock file.

---

## What is actually good

Stated plainly, because the evidence supports it:

- `cms_check.rs` refuses to derive the signature primitive from the certificate,
  cross-checks `SignedData.digestAlgorithms`, rejects duplicated
  `content-type`/`message-digest`/`signing-time`, validates
  `cms-algorithm-protection` and `signing-certificate-v2`, and recovers raw
  `signedAttrs` by walking the DER rather than re-encoding — with a comment
  explaining exactly why re-encoding would be wrong. That is a level of care most
  PDF signature implementations do not reach.
- The §32 gate reads the candidate **back from disk** and refuses to promote on any
  failure. The tamper hook exists solely to prove the gate is load-bearing.
- `verify/objects.rs` bounds depth, entries, stream bytes, and object streams; the
  repair scan steps over stream bodies so a decoy `5 0 obj` cannot be selected.
- The signature UI is honest, and there is a test enforcing the honesty.
- `reconcile` is pure, idempotent, and capability-driven; `LayoutId::from_name`
  sanitises correctly; the `Outcome` convention holds (every `-> bool` found is a
  predicate, not an operation).
- Clippy clean at `-D warnings`. 1845 tests. PDFium present, so the meaningful
  rendering tests actually ran.

The through-line of every finding above: **the careful implementation and the
careless one are usually sitting in the same crate.** `verify/objects.rs` vs
`verify/mod.rs`. `sign/apply.rs`'s temp file vs `shm.rs`'s. The fix for most of
this is to delete the second implementation and route through the first.

---

## Suggested order

1. **#1 and #2** — one is remotely triggerable and takes down a live
   presentation; the other leaks 2 GB and the slide contents.
2. **#3** — a one-line `args_os()` change with a two-line follow-through.
3. **#4–#6** — these gate the signing feature.
4. **#13** — until this is fixed, the suite is not proving what it claims to prove.
