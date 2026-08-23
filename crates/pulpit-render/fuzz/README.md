# Fuzzing Suite for PDF Signature Verification

This directory contains libFuzzer targets for the attacker-reachable parsers in the PDF signature verification module (`src/verify/`).

## Targets

Four targets fuzz the critical paths:

- **fuzz_revision_map**: Tests `RevisionMap::build()` on arbitrary bytes. Verifies the xref chain parser never panics.
- **fuzz_discover**: Tests discovery of signatures via `discover_signatures()` after building the revision map.
- **fuzz_verify_full**: Tests the main entry point `verify_signatures()`, covering coverage classification, integrity checks, and CMS parsing.
- **fuzz_cms**: Tests CMS parsing directly via `check_signature()` with doctored contents blobs.

## Running

Each target has a small committed seed corpus in `fuzz/seeds/<target>/`.
`fuzz/corpus/`
is cargo-fuzz's generated working set and remains ignored. Pass the committed
directory explicitly when starting a fresh run:

```bash
cargo fuzz run <target> fuzz/seeds/<target> -- -max_total_time=60
```

Run all targets for 60 seconds each (development, lightweight fuzzing):

```bash
make fuzz-sign
```

For overnight or CI runs with longer budgets, increase `-max_total_time`:

```bash
cargo fuzz run fuzz_revision_map -- -max_total_time=3600  # 1 hour
```

## Notes

- Fuzzing requires **nightly Rust** (`rustup default nightly`).
- The fuzz crate is excluded from the main workspace (`Cargo.toml`) so `cargo test --workspace` is unaffected.
- libFuzzer output during a run: `cov` = code coverage edges hit, `ft` = feature set, `corp` = corpus size.
- If a crash is found, it is minimized by cargo-fuzz and saved to `artifacts/`.

## Coverage

All paths are attacker-reachable on any opened PDF file, so these targets directly test defense against malformed or crafted input.
