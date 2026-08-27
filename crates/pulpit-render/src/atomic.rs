//! Replacing a file's contents, once, for every writer in pulpit.
//!
//! Four writers had grown their own version of the same seven steps —
//! settings, the crash-recovery snapshot, the PDF export and the signer —
//! and they had drifted apart in exactly the way that matters. Three of them
//! opened the temporary file with `File::create`, which follows a symlink and
//! truncates whatever it lands on, and two of them named it predictably from
//! the destination or the pid. Only the signer's did neither. This module is
//! the signer's version, made available to the other three.
//!
//! The steps, in order, and why each one is there:
//!
//! 1. Create a temporary file **in the destination's own directory**, so the
//!    rename at the end is within one filesystem and therefore atomic.
//! 2. Create it with `O_CREAT|O_EXCL` under an unpredictable name. A name
//!    planted by somebody else is then a refusal, not a write through a
//!    symlink into a file we were never asked to touch.
//! 3. Write, and `fsync` the file — durability before visibility, so the
//!    rename can never expose contents still sitting in the page cache.
//! 4. Rename over the destination.
//! 5. `fsync` the containing directory, so the rename itself survives a
//!    crash.
//!
//! On any failure the temporary file is removed and the destination is left
//! exactly as it was.
//!
//! This module reads the clock and a hash seed to name the temporary file.
//! That is deliberate and is the whole of its impurity: it is a file-system
//! primitive, not a domain module.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// What permissions the finished file should carry.
///
/// This is a real decision, not a default to be picked absent-mindedly. A
/// file that holds the reader's own material is theirs alone; a file the
/// reader asked us to produce for them to go and use is theirs to share, and
/// silently narrowing it to `0o600` would be us overriding their umask.
///
/// On Windows neither variant sets anything: the file inherits its
/// directory's ACL. The `create_new` contract in step 2 is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// `0o600` from the instant the file exists — never widened after the
    /// fact, which would leave a window. For anything private: credentials,
    /// signed output, saved state.
    Private,
    /// Whatever the process umask says a new file gets, which is what
    /// `File::create` would have produced. For documents the reader asked us
    /// to write somewhere of their choosing.
    Inherited,
}

impl Visibility {
    #[cfg(unix)]
    fn mode(self) -> u32 {
        match self {
            // 0o666 is not a permission grant: the umask is applied to it by
            // the kernel, so this is precisely "an ordinary new file".
            Visibility::Inherited => 0o666,
            Visibility::Private => 0o600,
        }
    }
}

/// An I/O error together with the path it is about.
///
/// Every caller here has its own error type and all of them want to name the
/// file that failed, so the path travels with the error rather than being
/// reconstructed by guesswork at the catch site.
#[derive(Debug)]
pub struct AtomicError {
    pub path: PathBuf,
    pub source: std::io::Error,
}

impl std::fmt::Display for AtomicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for AtomicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<AtomicError> for std::io::Error {
    fn from(e: AtomicError) -> std::io::Error {
        std::io::Error::new(e.source.kind(), e.to_string())
    }
}

type Result<T> = std::result::Result<T, AtomicError>;

fn failure(path: impl Into<PathBuf>, source: std::io::Error) -> AtomicError {
    AtomicError {
        path: path.into(),
        source,
    }
}

/// The directory a path lives in, with the two spellings that are really
/// "here" — no parent at all, and an empty one — resolved to `.`.
pub fn parent_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => directory,
        _ => Path::new("."),
    }
}

/// A hidden name for the temporary file that another process cannot guess.
///
/// `tag` says which writer produced it, so a stray file left by a crash can
/// be traced back. Secrecy of the *contents* comes from `O_EXCL` in
/// [`open_exclusive`], not from the name; the name only has to be hard enough
/// to guess that planting a file at it in advance is not a strategy.
fn temporary_name(tag: &str) -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};

    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);

    // `RandomState` is seeded per process by the standard library; hashing
    // the ticket and the clock through it gives a name an attacker cannot
    // predict from the pid alone.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(ticket);
    hasher.write_u64(nanos);
    hasher.write_u32(std::process::id());
    format!(
        ".pulpit-{tag}-{}-{:016x}",
        std::process::id(),
        hasher.finish()
    )
}

/// Create `path`, and only create it: `O_CREAT|O_EXCL`, at `visibility`.
///
/// An existing file, or a symlink pointing anywhere at all, makes this fail
/// with `AlreadyExists` instead of opening — and nothing is truncated.
pub fn open_exclusive(path: &Path, visibility: Visibility) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(visibility.mode());
    }
    let _ = visibility;
    options.open(path)
}

/// Step 1 and step 2: the temporary file, in the destination's directory.
///
/// A taken name is answered by drawing another rather than by touching it.
/// Thirty-two consecutive collisions is not contention, it is a directory
/// that cannot be written to, so it gives up rather than spinning.
pub fn create_temporary(
    destination: &Path,
    tag: &str,
    visibility: Visibility,
) -> Result<(PathBuf, File)> {
    let directory = parent_directory(destination);

    let mut last_error = None;
    for _ in 0..32 {
        let path = directory.join(temporary_name(tag));
        match open_exclusive(&path, visibility) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(e),
            Err(e) => return Err(failure(path, e)),
        }
    }

    Err(failure(
        directory,
        last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no free temporary file name",
            )
        }),
    ))
}

/// Step 3, through the handle that was created and never by reopening the
/// path: reopening would hand the window back to whoever can write the
/// directory.
pub fn write_and_sync(path: &Path, file: &mut File, bytes: &[u8]) -> Result<()> {
    let mut write = || -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(path);
        failure(path, e)
    })
}

/// Steps 4 and 5: rename into place, then fsync the directory that now holds
/// the new name.
///
/// The directory fsync is best-effort. A filesystem that refuses to open its
/// own directory for syncing is not a reason to report a write that
/// succeeded as a write that failed.
pub fn promote(temporary: &Path, destination: &Path) -> Result<()> {
    if let Err(e) = std::fs::rename(temporary, destination) {
        let _ = std::fs::remove_file(temporary);
        return Err(failure(destination, e));
    }
    if let Ok(handle) = File::open(parent_directory(destination)) {
        let _ = handle.sync_all();
    }
    Ok(())
}

/// All five steps, for the callers that have nothing to do between writing
/// the bytes and promoting them.
///
/// The signer is the one caller that does: it reads the candidate back off
/// the disk and runs its verification gate before promoting, so it drives
/// [`create_temporary`], [`write_and_sync`] and [`promote`] itself.
pub fn replace(destination: &Path, tag: &str, visibility: Visibility, bytes: &[u8]) -> Result<()> {
    let (temporary, mut file) = create_temporary(destination, tag, visibility)?;
    write_and_sync(&temporary, &mut file, bytes)?;
    drop(file);
    promote(&temporary, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replacement_is_complete_or_absent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let destination = directory.path().join("state.json");

        replace(&destination, "test", Visibility::Private, b"first").expect("write");
        assert_eq!(std::fs::read(&destination).expect("read back"), b"first");

        replace(&destination, "test", Visibility::Private, b"second").expect("overwrite");
        assert_eq!(std::fs::read(&destination).expect("read back"), b"second");

        // Nothing is left behind: the temporary is renamed, not copied.
        let leftovers: Vec<_> = std::fs::read_dir(directory.path())
            .expect("list")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|name| name != "state.json")
            .collect();
        assert!(leftovers.is_empty(), "stray files: {leftovers:?}");
    }

    #[test]
    fn a_planted_file_at_the_temporary_name_is_not_truncated() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let planted = directory.path().join(".pulpit-test-planted");
        std::fs::write(&planted, b"victim contents").expect("plant a file");

        let error = open_exclusive(&planted, Visibility::Private)
            .expect_err("O_EXCL refuses an existing name");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&planted).expect("read back"),
            b"victim contents",
            "the planted file must not have been truncated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_at_the_temporary_name_is_not_followed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"victim contents").expect("write the victim");
        let planted = directory.path().join(".pulpit-test-planted");
        std::os::unix::fs::symlink(&victim, &planted).expect("plant a symlink");

        let error =
            open_exclusive(&planted, Visibility::Private).expect_err("O_EXCL refuses a symlink");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&victim).expect("read back"),
            b"victim contents",
            "the symlink's target must not have been written through"
        );
    }

    #[test]
    fn two_temporaries_beside_one_destination_do_not_collide() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let destination = directory.path().join("signed.pdf");

        let (first, _first) =
            create_temporary(&destination, "test", Visibility::Private).expect("create one");
        let (second, _second) =
            create_temporary(&destination, "test", Visibility::Private).expect("create another");

        assert_ne!(first, second, "two temporaries must not share a name");
        assert!(first.exists() && second.exists());
        assert_eq!(first.parent(), Some(directory.path()));

        // The name must not be derivable from the pid and a counter alone.
        let predictable = format!(".pulpit-test-{}-0", std::process::id());
        assert_ne!(first.file_name().unwrap().to_string_lossy(), predictable);
    }

    #[cfg(unix)]
    #[test]
    fn visibility_decides_the_mode_of_the_finished_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let mode_of =
            |path: &Path| std::fs::metadata(path).expect("stat").permissions().mode() & 0o777;

        let private = directory.path().join("private");
        replace(&private, "test", Visibility::Private, b"x").expect("write");
        assert_eq!(mode_of(&private), 0o600, "private material is owner-only");

        // What the umask would have given an ordinary `File::create`, and
        // therefore what this must give too: an exported document is the
        // reader's to share.
        let reference = directory.path().join("reference");
        std::fs::write(&reference, b"x").expect("write");
        let inherited = directory.path().join("inherited");
        replace(&inherited, "test", Visibility::Inherited, b"x").expect("write");
        assert_eq!(mode_of(&inherited), mode_of(&reference));
    }

    #[test]
    fn a_destination_with_no_directory_component_lands_beside_itself() {
        assert_eq!(parent_directory(Path::new("bare.pdf")), Path::new("."));
        assert_eq!(
            parent_directory(Path::new("/tmp/deck.pdf")),
            Path::new("/tmp")
        );
    }
}
