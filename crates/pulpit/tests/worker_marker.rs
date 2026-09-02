//! Every place that re-executes this binary must be bound by the fork-bomb
//! marker.
//!
//! `pulpit`, the renderer worker, the media worker, the document worker and
//! the typst worker are all roles of *one* executable, re-executed with a
//! flag. That is what makes the bound load-bearing rather than a formality:
//! the thing a worker would re-execute is itself, so a worker that spawns
//! workers grows exponentially and takes the machine down long before a
//! deadline or a restart budget can notice. Unbounded breadth is not a failure
//! any supervisor can catch.
//!
//! The marker is the only bound that holds, and it has already been forgotten
//! once: the typst worker set an environment variable that nothing ever read
//! and never touched the real one, so for as long as that code existed the
//! bound simply did not apply there. Nothing failed, because nothing checked.
//!
//! Checked by reading the source, because the alternative is spawning real
//! worker processes from a unit test. The needles are assembled at run time:
//! this file is not one of the ones scanned, but the habit is the one
//! `scroll.rs` established for the same reason, and a future move of this test
//! into a scanned file should not silently make it pass.

use std::path::Path;

/// Files that re-execute the current binary, and must therefore ask the guard
/// before doing it.
///
/// Paths are relative to this crate's `tests/` directory. A new worker role
/// belongs on this list; if it is not here, this test cannot protect it, which
/// is why the sweep below also fails on a file it does not know about.
const RE_EXECUTES: &[&str] = &[
    "../../pulpit-core/src/ipc/worker.rs",
    "../../pulpit-render/src/document/session.rs",
    "../src/typst_annotation.rs",
];

/// Files that call `current_exe()` to *locate* something beside the binary
/// rather than to run it. These are not spawns and are deliberately exempt.
const LOCATES_ONLY: &[&str] = &[
    "../../pulpit-render/src/pdf/pdfium.rs",
    "../../pulpit-media/src/worker/mpv.rs",
    // §76.2's end-to-end journal/undo test: finds the sibling `pulpit`
    // executable to hand to `DocumentSession::start`, which is what asks the
    // guard, exactly as `crates/pulpit/tests/document_worker.rs` does.
    "../src/reader_journal.rs",
];

fn crate_sources() -> Vec<std::path::PathBuf> {
    fn walk(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    for crate_name in [
        "pulpit",
        "pulpit-core",
        "pulpit-display",
        "pulpit-media",
        "pulpit-render",
    ] {
        walk(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../")
                .join(crate_name)
                .join("src")
                .as_path(),
            &mut found,
        );
    }
    found
}

#[test]
fn every_file_that_re_executes_this_binary_asks_the_guard_first() {
    let guard = format!("{}_guard", "spawn");
    let build = format!("{}(\"", "build");

    for relative in RE_EXECUTES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join(relative);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is on the list but unreadable: {e}", path.display()));
        assert!(
            source.contains(&guard) || source.contains(&build),
            "{relative} re-executes this binary without asking the guard, so a worker \
             spawned from it could spawn its own — the failure the marker exists to stop"
        );
    }
}

#[test]
fn no_file_re_executes_this_binary_without_being_on_the_list() {
    // The list above is only a guarantee if nothing can quietly join it. A new
    // worker role that re-executes the binary shows up here first.
    let needle = format!("current_{}()", "exe");
    let known: Vec<_> = RE_EXECUTES
        .iter()
        .chain(LOCATES_ONLY.iter())
        .map(|relative| {
            Path::new(relative)
                .file_name()
                .expect("every listed path names a file")
                .to_owned()
        })
        .collect();

    let mut unknown = Vec::new();
    for path in crate_sources() {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !source.contains(&needle) {
            continue;
        }
        let name = path.file_name().expect("a source file has a name");
        if !known.iter().any(|listed| listed == name) {
            unknown.push(path);
        }
    }

    assert!(
        unknown.is_empty(),
        "these re-execute the binary but are on neither list, so nothing checks that the \
         fork-bomb marker binds them: {unknown:#?}"
    );
}

#[test]
fn the_marker_has_exactly_one_definition() {
    // It used to have three, each with a comment saying the copies had to
    // agree, and nothing that made them.
    let definition = format!("const WORKER_{}: &str", "MARKER");
    let defining: Vec<_> = crate_sources()
        .into_iter()
        .filter(|path| {
            std::fs::read_to_string(path).is_ok_and(|source| source.contains(&definition))
        })
        .collect();

    assert_eq!(
        defining.len(),
        1,
        "the fork-bomb marker must be defined once; found {defining:#?}"
    );
}
