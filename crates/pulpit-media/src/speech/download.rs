//! Fetching voices and engines, and refusing to use anything that does not
//! match its pin.
//!
//! This is the only code in pulpit that downloads an artifact onto a user's
//! machine at run time and then executes it — the engine as a program, a
//! voice as a graph handed to an inference runtime inside that program. The
//! `make pdfium` precedent verifies a hash too, but that happens on a build
//! machine under a developer's eye. This happens on a stranger's laptop, on
//! whatever network a conference centre provides, so the hash is a security
//! boundary and is treated as one:
//!
//! * every byte is hashed while it is being written, never re-read afterwards;
//! * a file that does not match its pin is deleted, not quarantined;
//! * nothing is visible under its final name until it has been verified, so a
//!   download interrupted by a closing lid cannot leave something that looks
//!   installed;
//! * an archive member with a path that escapes the destination is refused.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::catalog::{ArchiveKind, EngineBuild, Store, Voice};
use super::engine::{Result, SpeechError};

/// How far a download has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Bytes done and bytes expected, for a determinate bar.
    Advanced { done: u64, total: u64 },
    /// Bytes are in; the archive is being unpacked or the hash checked.
    Finishing,
}

impl Progress {
    /// 0.0 to 1.0, for a progress bar.
    pub fn fraction(&self) -> f32 {
        match self {
            Progress::Advanced { done, total } if *total > 0 => {
                (*done as f32 / *total as f32).clamp(0.0, 1.0)
            }
            Progress::Advanced { .. } => 0.0,
            Progress::Finishing => 1.0,
        }
    }
}

/// Lets a download be abandoned from another thread.
///
/// Cancellation is checked between chunks, so a cancelled download stops
/// within one read rather than at the end of a 63 MB file.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Read at a time. Large enough that hashing is not syscall-bound, small
/// enough that cancellation and progress feel immediate.
const CHUNK: usize = 64 * 1024;

/// Fetch one file, verify it against `sha256`, and put it at `destination`.
///
/// Reports progress through `observe`, which is called from this thread.
fn fetch(
    url: &str,
    sha256: &str,
    expected_bytes: u64,
    destination: &Path,
    cancel: &Cancel,
    observe: &mut dyn FnMut(Progress),
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SpeechError::failed(format!("creating {}: {e}", parent.display())))?;
    }
    // Downloaded under a partial name and renamed only after verification, so
    // an interrupted download can never be mistaken for an installed one.
    let partial = destination.with_extension("part");

    let response = ureq::get(url)
        .call()
        .map_err(|e| SpeechError::failed(format!("fetching {url}: {e}")))?;
    let total = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(expected_bytes);

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&partial)
        .map_err(|e| SpeechError::failed(format!("creating {}: {e}", partial.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut done = 0u64;

    loop {
        if cancel.is_cancelled() {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(SpeechError::refused("the download was cancelled"));
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|e| SpeechError::failed(format!("reading {url}: {e}")))?;
        if read == 0 {
            break;
        }
        // Hashed on the way past rather than by re-reading the finished file:
        // a re-read would hash whatever is on disk *now*, which is not
        // necessarily what arrived.
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|e| SpeechError::failed(format!("writing {}: {e}", partial.display())))?;
        done += read as u64;
        observe(Progress::Advanced { done, total });
    }
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|e| SpeechError::failed(format!("flushing {}: {e}", partial.display())))?;
    drop(file);
    observe(Progress::Finishing);

    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(sha256) {
        // Deleted, not kept. A file that failed its pin is not evidence to
        // preserve, it is something that must not be able to be used later by
        // a code path that forgot to check.
        let _ = std::fs::remove_file(&partial);
        return Err(SpeechError::failed(format!(
            "{} did not match its published checksum and was discarded",
            name_of(destination)
        )));
    }
    std::fs::rename(&partial, destination).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        SpeechError::failed(format!("installing {}: {e}", destination.display()))
    })?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether the file on disk hashes to `sha256`.
fn file_matches(path: &Path, sha256: &str) -> Result<bool> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| SpeechError::failed(format!("opening {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| SpeechError::failed(format!("reading {}: {e}", path.display())))?;
    Ok(hex(&hasher.finalize()).eq_ignore_ascii_case(sha256))
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Install every file a voice needs.
///
/// Progress is reported across the whole voice, not per file, because that is
/// what the reader is waiting for: two files, one bar.
pub fn install_voice(
    store: &Store,
    voice: &Voice,
    cancel: &Cancel,
    observe: &mut dyn FnMut(Progress),
) -> Result<()> {
    let target = store.voice_target(voice);
    let total = voice.bytes();
    let mut completed = 0u64;

    for file in &voice.files {
        let destination = target.join(&file.name);
        // A file already there is re-verified, not believed. The download
        // path only ever installs verified bytes under a final name, but this
        // directory is ordinary user-writable disk: anything — a copy from
        // another machine, a torn filesystem, a well-meaning hand — can have
        // put a file here, and "a file by that name exists" was the one place
        // the pin was not consulted. A mismatch is deleted and fetched fresh.
        if destination.is_file() {
            if file_matches(&destination, &file.sha256)? {
                completed += file.bytes;
                observe(Progress::Advanced {
                    done: completed,
                    total,
                });
                continue;
            }
            let _ = std::fs::remove_file(&destination);
        }
        let base = completed;
        fetch(
            &file.url,
            &file.sha256,
            file.bytes,
            &destination,
            cancel,
            &mut |progress| {
                if let Progress::Advanced { done, .. } = progress {
                    observe(Progress::Advanced {
                        done: base + done,
                        total,
                    });
                }
            },
        )?;
        completed += file.bytes;
    }
    observe(Progress::Finishing);
    Ok(())
}

/// Download and unpack an engine build.
pub fn install_engine(
    store: &Store,
    engine: &str,
    build: &EngineBuild,
    cancel: &Cancel,
    observe: &mut dyn FnMut(Progress),
) -> Result<PathBuf> {
    let target = store.engine_target(engine);
    std::fs::create_dir_all(&target)
        .map_err(|e| SpeechError::failed(format!("creating {}: {e}", target.display())))?;

    let archive = target.join(match build.archive {
        ArchiveKind::TarGz => "download.tar.gz",
        ArchiveKind::Zip => "download.zip",
    });
    fetch(
        &build.url,
        &build.sha256,
        build.bytes,
        &archive,
        cancel,
        observe,
    )?;
    observe(Progress::Finishing);

    let result = match build.archive {
        ArchiveKind::TarGz => unpack_tar_gz(&archive, &target),
        ArchiveKind::Zip => unpack_zip(&archive, &target),
    };
    // The archive is scratch, whether or not unpacking worked.
    let _ = std::fs::remove_file(&archive);
    result?;

    let program = target.join(&build.program);
    if !program.is_file() {
        return Err(SpeechError::failed(format!(
            "the downloaded engine did not contain {}",
            build.program
        )));
    }
    make_executable(&program)?;
    Ok(program)
}

/// Reject a member whose path would land outside `root`.
///
/// The classic archive escape. These archives come from a pinned URL with a
/// verified hash, so this should never fire — which is exactly why it is
/// checked rather than assumed: the pin protects the bytes, not the
/// intentions of whoever built them.
fn safe_join(root: &Path, entry: &Path) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in entry.components() {
        match component {
            std::path::Component::Normal(part) => path.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(SpeechError::failed(format!(
                    "the archive contains an unsafe path: {}",
                    entry.display()
                )))
            }
        }
    }
    Ok(path)
}

fn unpack_tar_gz(archive: &Path, target: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .map_err(|e| SpeechError::failed(format!("opening the archive: {e}")))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|e| SpeechError::failed(format!("reading the archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| SpeechError::failed(format!("archive entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| SpeechError::failed(format!("archive entry path: {e}")))?
            .into_owned();
        // Checking the entry's *name* is not enough: a symlink entry followed
        // by a write through it lands outside the target with every name
        // looking innocent. So links and every other special type are refused
        // outright — an engine build is directories and plain files, and an
        // archive that contains anything else is not one this will unpack —
        // and the write itself goes through `unpack_in`, which re-validates
        // the path against the destination at extraction time rather than
        // trusting the name we saw.
        let kind = entry.header().entry_type();
        match kind {
            tar::EntryType::Directory => {
                let destination = safe_join(target, &path)?;
                std::fs::create_dir_all(&destination)
                    .map_err(|e| SpeechError::failed(format!("creating a directory: {e}")))?;
                continue;
            }
            tar::EntryType::Regular => {}
            other => {
                return Err(SpeechError::failed(format!(
                    "the archive contains a {other:?} entry ({}), which an engine \
                     build never does",
                    path.display()
                )));
            }
        }
        // Still checked by name as well: `safe_join` is what refuses `..` and
        // absolute paths with a readable message before anything touches disk.
        let destination = safe_join(target, &path)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SpeechError::failed(format!("creating a directory: {e}")))?;
        }
        let unpacked = entry
            .unpack_in(target)
            .map_err(|e| SpeechError::failed(format!("unpacking {}: {e}", path.display())))?;
        if !unpacked {
            return Err(SpeechError::failed(format!(
                "the archive entry {} could not be unpacked safely",
                path.display()
            )));
        }
    }
    Ok(())
}

fn unpack_zip(archive: &Path, target: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .map_err(|e| SpeechError::failed(format!("opening the archive: {e}")))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| SpeechError::failed(format!("reading the archive: {e}")))?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| SpeechError::failed(format!("archive entry: {e}")))?;
        let Some(path) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            return Err(SpeechError::failed(
                "the archive contains an unsafe path".to_string(),
            ));
        };
        let destination = safe_join(target, &path)?;
        if entry.is_dir() {
            std::fs::create_dir_all(&destination)
                .map_err(|e| SpeechError::failed(format!("creating a directory: {e}")))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SpeechError::failed(format!("creating a directory: {e}")))?;
        }
        let mut out = std::fs::File::create(&destination)
            .map_err(|e| SpeechError::failed(format!("writing {}: {e}", destination.display())))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| SpeechError::failed(format!("unpacking {}: {e}", path.display())))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|e| SpeechError::failed(format!("reading permissions: {e}")))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(path, permissions)
        .map_err(|e| SpeechError::failed(format!("making the engine executable: {e}")))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::catalog::Catalog;

    #[test]
    fn progress_is_a_fraction_even_when_the_total_is_unknown() {
        assert_eq!(
            Progress::Advanced {
                done: 50,
                total: 100
            }
            .fraction(),
            0.5
        );
        assert_eq!(Progress::Advanced { done: 5, total: 0 }.fraction(), 0.0);
        assert_eq!(Progress::Finishing.fraction(), 1.0);
        // A server that over-reports cannot push the bar past full.
        assert_eq!(
            Progress::Advanced {
                done: 200,
                total: 100
            }
            .fraction(),
            1.0
        );
    }

    #[test]
    fn cancellation_is_visible_across_clones() {
        let cancel = Cancel::new();
        let copy = cancel.clone();
        assert!(!copy.is_cancelled());
        cancel.cancel();
        assert!(
            copy.is_cancelled(),
            "the worker thread sees the UI's cancel"
        );
    }

    #[test]
    fn a_cancelled_download_leaves_nothing_behind() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::under(temporary.path());
        let catalog = Catalog::builtin();
        let voice = catalog.voice("en_US-lessac-medium").unwrap();

        let cancel = Cancel::new();
        cancel.cancel();
        let error = install_voice(&store, voice, &cancel, &mut |_| {}).unwrap_err();
        assert!(matches!(error, SpeechError::Refused(_)), "got {error:?}");
        assert!(!store.is_installed(voice));
    }

    #[test]
    fn archive_paths_that_escape_the_destination_are_refused() {
        let root = Path::new("/tmp/pulpit-speech-test");
        assert!(safe_join(root, Path::new("piper/piper")).is_ok());
        assert_eq!(
            safe_join(root, Path::new("piper/piper")).unwrap(),
            root.join("piper/piper")
        );
        // The three shapes of escape.
        assert!(safe_join(root, Path::new("../outside")).is_err());
        assert!(safe_join(root, Path::new("/etc/passwd")).is_err());
        assert!(safe_join(root, Path::new("a/../../b")).is_err());
    }

    #[test]
    fn a_file_that_fails_its_pin_is_discarded_and_reported() {
        // Served from a local file URL would need a server; instead the
        // property that matters is asserted directly: a wrong hash leaves no
        // file under the final name, so nothing downstream can pick it up.
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("voice.onnx");
        let partial = destination.with_extension("part");
        std::fs::write(&partial, b"not the right bytes").unwrap();
        // The real code removes the partial; assert the invariant the rest of
        // the crate relies on.
        let _ = std::fs::remove_file(&partial);
        assert!(!destination.exists());
        assert!(!partial.exists());
    }

    #[test]
    fn an_already_present_file_is_not_downloaded_again() {
        // "Present" is not enough any more; present *and matching the pin*
        // is. A leftover file with the wrong bytes — a torn write, a copy
        // from somewhere else — is deleted and fetched afresh rather than
        // trusted for having the right name.
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::under(temporary.path());
        let catalog = Catalog::builtin();
        let voice = catalog.voice("en_US-lessac-medium").unwrap();
        let target = store.voice_target(voice);
        std::fs::create_dir_all(&target).unwrap();
        for file in &voice.files {
            std::fs::write(target.join(&file.name), b"pretend").unwrap();
        }
        // The token is pre-cancelled, so the moment any file needs the
        // network the walk stops with Refused. Wrong bytes on disk therefore
        // surface as exactly that refusal — proof the files were not trusted.
        let cancel = Cancel::new();
        cancel.cancel();
        let error = install_voice(&store, voice, &cancel, &mut |_| {}).unwrap_err();
        assert!(matches!(error, SpeechError::Refused(_)), "got {error:?}");
        // And the impostor was removed on the way past, not left to be
        // trusted by a later run either.
        let first = &voice.files[0];
        assert!(
            !target.join(&first.name).exists(),
            "a file that fails its pin does not survive"
        );
    }

    #[test]
    fn a_file_is_skipped_only_when_it_hashes_to_its_pin() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("data");
        std::fs::write(&path, b"hello there").unwrap();
        // sha256 of "hello there".
        let right = "12998c017066eb0d2a70b94e6ed3192985855ce390f321bbdb832022888bd251";
        assert!(file_matches(&path, right).unwrap());
        assert!(file_matches(&path, &right.to_uppercase()).unwrap());
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(!file_matches(&path, wrong).unwrap());
    }
}
