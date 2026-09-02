//! PDF-side overlay discovery and asset staging (`docs-src/internals.typ`,
//! 7.4).
//!
//! Everything here is the *impure* half of the overlay story: it turns link
//! annotations into declarations, and turns attachment bytes into files a
//! runtime worker can open. The meaning of a declaration is decided in
//! `pulpit_core::overlay`, which this module only calls into.
//!
//! The security posture is deliberately paranoid: a deck is untrusted input
//! that arrives by email, and a ZIP attachment inside it is untrusted input
//! twice over. Every limit is checked *before* the allocation or write it
//! bounds, and a failing asset is a diagnostic — the static PDF page still
//! opens.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pulpit_core::overlay::{
    is_media_uri, parse_overlay_uri, ExternalAssetRef, ManifestError, OverlayDeclaration,
    WebManifest, WEB_MANIFEST_NAME,
};
use pulpit_core::{LinkTarget, PageLink};

/// Turn one page's link annotations into overlay declarations.
///
/// Ordinary links are silently skipped: a deck full of `https://` links is
/// not a deck full of broken media. Only a URI that *announced itself* as
/// media and then failed to parse is worth a diagnostic, because that is a
/// producer mistake the author can fix.
pub fn declarations_from_links(links: &[PageLink]) -> (Vec<OverlayDeclaration>, Vec<String>) {
    let mut declarations = Vec::new();
    let mut diagnostics = Vec::new();
    for link in links {
        let LinkTarget::Uri(uri) = &link.target else {
            continue;
        };
        match parse_overlay_uri(uri, link.rect) {
            Ok(declaration) => declarations.push(declaration),
            Err(e) if is_media_uri(uri) => {
                diagnostics.push(format!("media link `{uri}` was ignored: {e}"))
            }
            Err(_) => {}
        }
    }
    (declarations, diagnostics)
}

// ---------------------------------------------------------------------------
// Bundled ZIP extraction
// ---------------------------------------------------------------------------

/// Every ceiling the specification requires an extraction to enforce.
///
/// The defaults are sized for real presentation bundles — a few megabytes of
/// HTML, script and imagery — with enough headroom that a legitimate deck
/// never meets them, and far too little for a decompression bomb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_compressed_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_files: usize,
    pub max_path_len: usize,
    pub max_depth: usize,
    /// Expanded-to-compressed ratio allowed for any single entry.
    pub max_ratio: u64,
    pub max_file_bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_compressed_bytes: 64 << 20,
            max_expanded_bytes: 256 << 20,
            max_files: 2_000,
            max_path_len: 512,
            max_depth: 16,
            max_ratio: 100,
            max_file_bytes: 32 << 20,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("the bundle is not a readable ZIP archive: {0}")]
    Archive(String),
    #[error("staging failed: {0}")]
    Io(String),
    #[error("the bundle entry `{entry}` is unsafe: {reason}")]
    UnsafeEntry { entry: String, reason: &'static str },
    #[error("the bundle exceeds its {0} limit")]
    LimitExceeded(&'static str),
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    #[error("`{0}` resolves outside the document directory")]
    Escapes(String),
}

/// A bundle unpacked beneath a staging root, with its manifest already
/// parsed and validated.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedBundle {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    pub manifest: WebManifest,
}

/// Names Windows resolves to devices regardless of directory. They can never
/// be part of a legitimate bundle, and writing one on a Windows host has
/// effects no filesystem containment check would catch.
const DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Unix mode bits: the file-type field and the symbolic-link type.
const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;

/// Unpack a web bundle beneath `dest_root`.
///
/// Bundle-internal structure is *preserved* rather than replaced by generated
/// names: relative `<script src>` and `<img src>` references inside the HTML
/// only resolve if the tree the author built survives extraction. Safety
/// therefore comes from validating each entry name against the rules below
/// and from proving the joined path stays under the root, not from discarding
/// the name.
pub fn extract_bundle(
    zip_bytes: &[u8],
    dest_root: &Path,
    limits: &ExtractionLimits,
) -> Result<ExtractedBundle, ExtractionError> {
    if zip_bytes.len() as u64 > limits.max_compressed_bytes {
        return Err(ExtractionError::LimitExceeded("compressed size"));
    }
    std::fs::create_dir_all(dest_root).map_err(io)?;
    // Containment is checked against the *canonical* root, so a staging root
    // that is itself reached through a symlink cannot be used to argue that
    // an escaping entry is contained.
    let root = dest_root.canonicalize().map_err(io)?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| ExtractionError::Archive(e.to_string()))?;
    if archive.len() > limits.max_files {
        return Err(ExtractionError::LimitExceeded("file count"));
    }

    let mut expanded: u64 = 0;
    let mut files = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| ExtractionError::Archive(e.to_string()))?;
        let name = entry.name().to_string();
        let is_dir = entry.is_dir();
        let relative = safe_relative_path(&name, limits)?;

        if let Some(mode) = entry.unix_mode() {
            let kind = mode & S_IFMT;
            if kind == S_IFLNK {
                return Err(unsafe_entry(&name, "symbolic links are never staged"));
            }
            // A hard link has no ZIP representation of its own; anything that
            // is neither a regular file nor a directory is refused rather
            // than interpreted.
            if kind != 0 && kind != S_IFREG && kind != S_IFDIR {
                return Err(unsafe_entry(&name, "only files and directories are staged"));
            }
        }

        let destination = contained_path(&root, &relative, &name)?;
        if is_dir {
            std::fs::create_dir_all(&destination).map_err(io)?;
            continue;
        }
        // Two entries writing the same path would let the second silently
        // replace a file the manifest was already validated against.
        if !seen.insert(destination.clone()) {
            return Err(unsafe_entry(&name, "the bundle declares it twice"));
        }

        let declared = entry.size();
        if declared > limits.max_file_bytes {
            return Err(ExtractionError::LimitExceeded("per-file size"));
        }
        let compressed = entry.compressed_size();

        // The header is a claim, not a fact: read one byte past the ceiling
        // so an entry that lies about its size is caught rather than trusted,
        // and every bound below this point is computed from what was
        // actually read, not from the declared size — an archive can put
        // whatever it likes in its header.
        let mut bytes = Vec::with_capacity(declared.min(1 << 20) as usize);
        entry
            .take(limits.max_file_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| ExtractionError::Archive(e.to_string()))?;
        if bytes.len() as u64 > limits.max_file_bytes {
            return Err(ExtractionError::LimitExceeded("per-file size"));
        }
        let real = bytes.len() as u64;
        if compressed > 0 && real / compressed > limits.max_ratio {
            return Err(ExtractionError::LimitExceeded("compression ratio"));
        }
        expanded = expanded.saturating_add(real);
        if expanded > limits.max_expanded_bytes {
            return Err(ExtractionError::LimitExceeded("expanded size"));
        }

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&destination, &bytes).map_err(io)?;
        make_read_only(&destination)?;
        files.push(destination);
    }

    let manifest_path = root.join(WEB_MANIFEST_NAME);
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|_| ManifestError::Missing)?;
    let manifest: WebManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| ManifestError::Malformed(e.to_string()))?;
    manifest.validate()?;

    Ok(ExtractedBundle {
        root,
        files,
        manifest,
    })
}

fn io(error: std::io::Error) -> ExtractionError {
    ExtractionError::Io(error.to_string())
}

fn unsafe_entry(entry: &str, reason: &'static str) -> ExtractionError {
    ExtractionError::UnsafeEntry {
        entry: entry.to_string(),
        reason,
    }
}

/// Validate one ZIP entry name and return it as a relative path.
///
/// This is a *lexical* check on the name the archive declared, before it ever
/// touches the filesystem; [`contained_path`] then proves the joined result.
fn safe_relative_path(name: &str, limits: &ExtractionLimits) -> Result<PathBuf, ExtractionError> {
    if name.is_empty() {
        return Err(unsafe_entry(name, "the name is empty"));
    }
    if name.len() > limits.max_path_len {
        return Err(ExtractionError::LimitExceeded("path length"));
    }
    if name.contains('\0') {
        return Err(unsafe_entry(name, "the name contains a NUL"));
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(unsafe_entry(name, "the name is rooted"));
    }
    if name.as_bytes().get(1) == Some(&b':') {
        return Err(unsafe_entry(name, "the name carries a drive prefix"));
    }

    let mut path = PathBuf::new();
    let mut depth = 0usize;
    // A directory entry ends in a separator; the empty trailing segment is
    // structure, not a name.
    let segments = name
        .split(['/', '\\'])
        .filter(|segment| !(segment.is_empty() && name.ends_with(['/', '\\'])));
    for segment in segments {
        if segment.is_empty() {
            return Err(unsafe_entry(name, "the name has an empty component"));
        }
        if segment == "." || segment == ".." {
            return Err(unsafe_entry(name, "the name traverses"));
        }
        if segment != segment.trim() {
            return Err(unsafe_entry(name, "a component is padded with whitespace"));
        }
        let stem = segment.split('.').next().unwrap_or(segment);
        if DEVICE_NAMES
            .iter()
            .any(|device| stem.eq_ignore_ascii_case(device))
        {
            return Err(unsafe_entry(name, "a component names a system device"));
        }
        path.push(segment);
        depth += 1;
        if depth > limits.max_depth {
            return Err(ExtractionError::LimitExceeded("nesting depth"));
        }
    }
    if path.as_os_str().is_empty() {
        return Err(unsafe_entry(name, "the name is empty"));
    }
    Ok(path)
}

/// Join a validated relative path onto the root and prove the result is still
/// inside it. The lexical check above already excludes traversal; this is the
/// belt to its braces, because the cost of being wrong is a write outside the
/// staging directory.
fn contained_path(root: &Path, relative: &Path, name: &str) -> Result<PathBuf, ExtractionError> {
    let joined = root.join(relative);
    let mut normalised = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                if !normalised.pop() {
                    return Err(unsafe_entry(name, "the name escapes the staging root"));
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }
    if !normalised.starts_with(root) {
        return Err(unsafe_entry(name, "the name escapes the staging root"));
    }
    Ok(normalised)
}

/// Staged assets are read-only to runtime workers: a worker that can rewrite
/// its own bundle can rewrite what the next presentation shows.
fn make_read_only(path: &Path) -> Result<(), ExtractionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).map_err(io)?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(path).map_err(io)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions).map_err(io)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// External assets
// ---------------------------------------------------------------------------

/// Resolve a document-relative asset reference to a real path.
///
/// Canonicalisation is what makes this trustworthy: a symbolic link inside
/// the document directory pointing at `/etc/shadow` passes every lexical
/// check and fails this one.
pub fn resolve_external(
    document_dir: &Path,
    asset: &ExternalAssetRef,
) -> Result<PathBuf, ExtractionError> {
    if !asset.is_containable() {
        return Err(ExtractionError::Escapes(asset.path.clone()));
    }
    let base = document_dir.canonicalize().map_err(io)?;
    let resolved = base
        .join(&asset.path)
        .canonicalize()
        .map_err(|_| ExtractionError::Escapes(asset.path.clone()))?;
    if !resolved.starts_with(&base) {
        return Err(ExtractionError::Escapes(asset.path.clone()));
    }
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Staging root
// ---------------------------------------------------------------------------

/// One private, randomly named directory holding every staged asset of a
/// document generation (`docs-src/internals.typ`).
///
/// The directory is removed when the root is dropped — that is, when the
/// generation is retired. Cleanup failure is a diagnostic and never
/// propagates: an audience is never shown an error because a temporary
/// directory outlived its welcome.
#[derive(Debug)]
pub struct StagingRoot {
    root: PathBuf,
    next_asset: AtomicU64,
}

impl StagingRoot {
    /// Create a staging root under the system temporary directory.
    pub fn create() -> std::io::Result<Self> {
        Self::create_in(&std::env::temp_dir())
    }

    pub fn create_in(parent: &Path) -> std::io::Result<Self> {
        // `create_dir` fails rather than reuses if the name is taken, so a
        // collision can never hand two generations the same directory.
        for _ in 0..8 {
            let root = parent.join(format!("pulpit-stage-{:016x}", random_token()));
            match std::fs::create_dir(&root) {
                Ok(()) => {
                    restrict_to_owner(&root)?;
                    return Ok(Self {
                        root,
                        next_asset: AtomicU64::new(0),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other(
            "could not create a private staging directory",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// A fresh subdirectory for one asset. The name is generated, never taken
    /// from the PDF: attachment names are untrusted strings.
    pub fn asset_dir(&self) -> std::io::Result<PathBuf> {
        let index = self.next_asset.fetch_add(1, Ordering::Relaxed);
        let directory = self.root.join(format!("asset-{index:06}"));
        std::fs::create_dir_all(&directory)?;
        Ok(directory)
    }
}

impl Drop for StagingRoot {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.root) {
            tracing::warn!(path = %self.root.display(), error = %e, "staging cleanup failed");
        }
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// A name no other process can guess in advance. This is obscurity on top of
/// the 0o700 mode, not instead of it.
fn random_token() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let marker = 0u8;
    let stack = &marker as *const u8 as u64;
    let mut value = nanos ^ stack.rotate_left(17) ^ (std::process::id() as u64).wrapping_mul(31);
    value = value.wrapping_add(counter.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    // A cheap avalanche, so adjacent nanosecond readings do not produce
    // adjacent names.
    value ^= value >> 33;
    value = value.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    value ^= value >> 29;
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::notes::Region;
    use pulpit_core::overlay::{ContentKind, WEB_MANIFEST_VERSION};
    use std::io::Write;

    fn link(uri: &str) -> PageLink {
        PageLink {
            rect: Region::new(0.1, 0.2, 0.3, 0.4),
            target: LinkTarget::Uri(uri.into()),
        }
    }

    #[test]
    fn a_media_link_becomes_a_declaration_over_its_own_rectangle() {
        let (declarations, diagnostics) =
            declarations_from_links(&[link("pulpit://video/clip?autoplay")]);
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].region, Region::new(0.1, 0.2, 0.3, 0.4));
        assert_eq!(declarations[0].content.kind(), ContentKind::Video);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn an_ordinary_link_is_skipped_without_a_diagnostic() {
        let (declarations, diagnostics) = declarations_from_links(&[
            link("https://example.com"),
            PageLink {
                rect: Region::FULL,
                target: LinkTarget::Page {
                    page: 3,
                    zoom: None,
                },
            },
        ]);
        assert!(declarations.is_empty());
        assert!(
            diagnostics.is_empty(),
            "a deck full of web links is not a deck full of broken media"
        );
    }

    #[test]
    fn a_malformed_media_uri_produces_a_diagnostic_the_author_can_act_on() {
        let (declarations, diagnostics) = declarations_from_links(&[
            link("pulpit://hologram/x"),
            link("pulpit://video-external/../../etc/passwd"),
        ]);
        assert!(declarations.is_empty());
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].contains("pulpit://hologram/x"));
        assert!(diagnostics[1].contains("escapes"));
    }

    // -- extraction ---------------------------------------------------------

    fn manifest_json() -> String {
        format!("{{\"version\": {WEB_MANIFEST_VERSION}, \"entrypoint\": \"index.html\"}}")
    }

    /// Build a ZIP from `(name, contents)` pairs, with no validation of its
    /// own: the point of these tests is to hand the extractor archives no
    /// honest producer would write.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn good_bundle() -> Vec<u8> {
        zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            ("index.html", b"<p>hello</p>"),
            ("assets/app.js", b"console.log(1)"),
        ])
    }

    fn stage() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("bundle");
        (directory, root)
    }

    /// Extract into a staging root of its own, the way the application does:
    /// one generation, one private directory, never a reused one.
    fn extract_fresh(
        archive: &[u8],
        limits: &ExtractionLimits,
    ) -> (tempfile::TempDir, Result<ExtractedBundle, ExtractionError>) {
        let (guard, root) = stage();
        let result = extract_bundle(archive, &root, limits);
        (guard, result)
    }

    #[test]
    fn a_well_formed_bundle_keeps_its_internal_structure() {
        let (_guard, root) = stage();
        let bundle = extract_bundle(&good_bundle(), &root, &ExtractionLimits::default()).unwrap();
        assert_eq!(bundle.manifest.entrypoint, "index.html");
        assert_eq!(bundle.files.len(), 3);
        assert!(
            bundle.root.join("assets/app.js").exists(),
            "relative HTML only resolves if the tree survives extraction"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_files_are_read_only_to_runtime_workers() {
        use std::os::unix::fs::PermissionsExt;
        let (_guard, root) = stage();
        let bundle = extract_bundle(&good_bundle(), &root, &ExtractionLimits::default()).unwrap();
        for file in &bundle.files {
            let mode = std::fs::metadata(file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o444, "{} is writable", file.display());
        }
    }

    #[test]
    fn a_traversing_entry_never_reaches_the_filesystem() {
        for name in ["../escaped.html", "a/../../escaped.html", "./x.html"] {
            let archive = zip_of(&[
                (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
                (name, b"pwned"),
            ]);
            let (guard, result) = extract_fresh(&archive, &ExtractionLimits::default());
            assert!(
                matches!(result, Err(ExtractionError::UnsafeEntry { .. })),
                "{name} should be refused"
            );
            assert!(!guard.path().join("escaped.html").exists());
        }
    }

    #[test]
    fn absolute_rooted_and_drive_prefixed_entries_are_refused() {
        for name in ["/etc/passwd", "\\windows\\system32\\x", "C:/windows/x"] {
            let archive = zip_of(&[
                (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
                (name, b"x"),
            ]);
            assert!(
                matches!(
                    extract_fresh(&archive, &ExtractionLimits::default()).1,
                    Err(ExtractionError::UnsafeEntry { .. })
                ),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn windows_device_names_are_refused_on_every_platform() {
        for name in ["CON", "nul.html", "a/LPT9.txt", "com1"] {
            let archive = zip_of(&[
                (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
                (name, b"x"),
            ]);
            assert!(
                matches!(
                    extract_fresh(&archive, &ExtractionLimits::default()).1,
                    Err(ExtractionError::UnsafeEntry { .. })
                ),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn a_symlink_entry_is_never_staged() {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        writer.start_file(WEB_MANIFEST_NAME, options).unwrap();
        writer.write_all(manifest_json().as_bytes()).unwrap();
        writer
            .add_symlink("shortcut", "/etc/passwd", options)
            .unwrap();
        let archive = writer.finish().unwrap().into_inner();

        let (_guard, root) = stage();
        assert!(matches!(
            extract_bundle(&archive, &root, &ExtractionLimits::default()),
            Err(ExtractionError::UnsafeEntry { .. })
        ));
    }

    #[test]
    fn a_compression_bomb_is_refused_before_it_is_written() {
        let (_guard, root) = stage();
        // A megabyte of zeroes deflates to almost nothing: exactly the shape
        // of a bomb, and exactly what the ratio limit exists for.
        let archive = zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            ("payload.bin", &vec![0u8; 1 << 20]),
        ]);
        assert!(matches!(
            extract_bundle(&archive, &root, &ExtractionLimits::default()),
            Err(ExtractionError::LimitExceeded("compression ratio"))
        ));
    }

    /// Build a two-entry ZIP — the manifest, then `name`/`contents` — whose
    /// *headers* claim the second entry is empty, in both the local file
    /// header and the central directory record, while the bytes actually
    /// written are `contents`. §76.9: `extract_bundle` must not trust that
    /// claim for its expansion bound.
    fn zip_with_lying_size(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut archive = zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            (name, contents),
        ]);
        let true_size = (contents.len() as u32).to_le_bytes();
        // Local file header: signature, then the uncompressed-size field 18
        // bytes in (4: version, 2: flags, 2: method, 2: time, 2: date, 4:
        // crc32, 4: compressed size = 18, then 4 bytes uncompressed size).
        const LOCAL_SIG: &[u8] = b"PK\x03\x04";
        const CENTRAL_SIG: &[u8] = b"PK\x01\x02";
        let local_offsets: Vec<usize> = find_all(&archive, LOCAL_SIG);
        let central_offsets: Vec<usize> = find_all(&archive, CENTRAL_SIG);
        assert_eq!(local_offsets.len(), 2, "manifest + payload local headers");
        assert_eq!(
            central_offsets.len(),
            2,
            "manifest + payload central records"
        );
        let local = local_offsets[1];
        let central = central_offsets[1];
        // Local file header: 4 (signature) + 2 (version needed) + 2 (flags)
        // + 2 (method) + 2 (time) + 2 (date) + 4 (crc32) + 4 (compressed
        // size) = 22 bytes before the uncompressed-size field.
        let local_size_at = local + 4 + 2 + 2 + 2 + 2 + 2 + 4 + 4;
        // Central directory record has one extra field ("version made by")
        // ahead of "version needed", so its uncompressed-size field sits 2
        // bytes further in: 24 bytes after the signature.
        let central_size_at = central + 4 + 2 + 2 + 2 + 2 + 2 + 2 + 4 + 4;
        assert_eq!(
            &archive[local_size_at..local_size_at + 4],
            &true_size[..],
            "test wrote its payload where it expected to find it"
        );
        assert_eq!(
            &archive[central_size_at..central_size_at + 4],
            &true_size[..],
            "test wrote its payload where it expected to find it"
        );
        archive[local_size_at..local_size_at + 4].copy_from_slice(&[0, 0, 0, 0]);
        archive[central_size_at..central_size_at + 4].copy_from_slice(&[0, 0, 0, 0]);
        archive
    }

    fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
        let mut offsets = Vec::new();
        let mut start = 0;
        while let Some(found) = haystack[start..]
            .windows(needle.len())
            .position(|window| window == needle)
        {
            offsets.push(start + found);
            start += found + 1;
        }
        offsets
    }

    #[test]
    fn an_archive_lying_about_zero_bytes_is_still_bounded_by_real_expansion() {
        let (_guard, root) = stage();
        // Every header on this entry claims 0 bytes; the bytes actually in
        // the archive are well over the expansion bound. §76.9 requires the
        // *real* size to be what accumulates into `expanded`.
        let archive = zip_with_lying_size("payload.bin", &vec![7u8; 1 << 20]);
        let limits = ExtractionLimits {
            max_expanded_bytes: 1 << 10,
            max_ratio: u64::MAX,
            ..Default::default()
        };
        assert!(
            matches!(
                extract_bundle(&archive, &root, &limits),
                Err(ExtractionError::LimitExceeded("expanded size"))
            ),
            "a header claiming 0 bytes must not buy unlimited real expansion"
        );
    }

    #[test]
    fn every_size_limit_refuses_rather_than_truncates() {
        let archive = zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            ("big.bin", b"0123456789"),
        ]);
        let tiny = ExtractionLimits {
            max_file_bytes: 4,
            max_ratio: u64::MAX,
            ..Default::default()
        };
        assert!(matches!(
            extract_fresh(&archive, &tiny).1,
            Err(ExtractionError::LimitExceeded("per-file size"))
        ));

        let squeezed = ExtractionLimits {
            max_expanded_bytes: 8,
            max_ratio: u64::MAX,
            ..Default::default()
        };
        assert!(matches!(
            extract_fresh(&archive, &squeezed).1,
            Err(ExtractionError::LimitExceeded("expanded size"))
        ));

        let narrow = ExtractionLimits {
            max_compressed_bytes: 4,
            ..Default::default()
        };
        assert!(matches!(
            extract_fresh(&archive, &narrow).1,
            Err(ExtractionError::LimitExceeded("compressed size"))
        ));
    }

    #[test]
    fn too_many_files_are_refused_before_any_are_written() {
        let (_guard, root) = stage();
        let names: Vec<String> = (0..40).map(|i| format!("f{i}.txt")).collect();
        let manifest = manifest_json();
        let mut entries: Vec<(&str, &[u8])> = vec![(WEB_MANIFEST_NAME, manifest.as_bytes())];
        entries.extend(names.iter().map(|name| (name.as_str(), b"x".as_slice())));
        let archive = zip_of(&entries);
        let limits = ExtractionLimits {
            max_files: 10,
            ..Default::default()
        };
        assert!(matches!(
            extract_bundle(&archive, &root, &limits),
            Err(ExtractionError::LimitExceeded("file count"))
        ));
        assert!(!root.join("f0.txt").exists());
    }

    #[test]
    fn deep_and_long_names_are_refused() {
        let deep = (0..40).map(|_| "d").collect::<Vec<_>>().join("/") + "/x.txt";
        let archive = zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            (deep.as_str(), b"x"),
        ]);
        assert!(matches!(
            extract_fresh(&archive, &ExtractionLimits::default()).1,
            Err(ExtractionError::LimitExceeded("nesting depth"))
        ));

        let long = "a".repeat(600) + ".txt";
        let archive = zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            (long.as_str(), b"x"),
        ]);
        assert!(matches!(
            extract_fresh(&archive, &ExtractionLimits::default()).1,
            Err(ExtractionError::LimitExceeded("path length"))
        ));
    }

    #[test]
    fn a_bundle_without_a_manifest_is_not_a_web_bundle() {
        let (_guard, root) = stage();
        let archive = zip_of(&[("index.html", b"<p>hi</p>")]);
        assert!(matches!(
            extract_bundle(&archive, &root, &ExtractionLimits::default()),
            Err(ExtractionError::Manifest(ManifestError::Missing))
        ));
    }

    #[test]
    fn an_unreadable_or_future_manifest_is_rejected_not_guessed_at() {
        let (_guard, root) = stage();
        let archive = zip_of(&[(WEB_MANIFEST_NAME, b"{ not json")]);
        assert!(matches!(
            extract_bundle(&archive, &root, &ExtractionLimits::default()),
            Err(ExtractionError::Manifest(ManifestError::Malformed(_)))
        ));

        let (_guard, root) = stage();
        let future = format!("{{\"version\": {}}}", WEB_MANIFEST_VERSION + 1);
        let archive = zip_of(&[(WEB_MANIFEST_NAME, future.as_bytes())]);
        assert!(matches!(
            extract_bundle(&archive, &root, &ExtractionLimits::default()),
            Err(ExtractionError::Manifest(ManifestError::UnknownVersion(_)))
        ));

        let (_guard, root) = stage();
        let escaping = format!(
            "{{\"version\": {WEB_MANIFEST_VERSION}, \"entrypoint\": \"../../etc/passwd\"}}"
        );
        let archive = zip_of(&[(WEB_MANIFEST_NAME, escaping.as_bytes())]);
        assert!(matches!(
            extract_bundle(&archive, &root, &ExtractionLimits::default()),
            Err(ExtractionError::Manifest(ManifestError::UnsafeEntrypoint(
                _
            )))
        ));
    }

    #[test]
    fn two_entries_that_land_on_one_path_are_refused() {
        // Distinct ZIP names, one destination: the separator forms differ, so
        // nothing catches this but the containment bookkeeping. Letting the
        // second win would replace a file the manifest was validated against.
        let archive = zip_of(&[
            (WEB_MANIFEST_NAME, manifest_json().as_bytes()),
            ("assets/app.js", b"first"),
            ("assets\\app.js", b"second"),
        ]);
        assert!(matches!(
            extract_fresh(&archive, &ExtractionLimits::default()).1,
            Err(ExtractionError::UnsafeEntry { .. })
        ));
    }

    // -- external assets ----------------------------------------------------

    #[test]
    fn an_external_asset_inside_the_document_directory_resolves() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("media")).unwrap();
        std::fs::write(directory.path().join("media/clip.mp4"), b"data").unwrap();
        let resolved =
            resolve_external(directory.path(), &ExternalAssetRef::new("media/clip.mp4")).unwrap();
        assert!(resolved.ends_with("media/clip.mp4"));
    }

    #[test]
    fn a_traversing_external_path_is_refused_lexically() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            resolve_external(directory.path(), &ExternalAssetRef::new("../secret.mp4")),
            Err(ExtractionError::Escapes(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_document_directory_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.mp4"), b"data").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.mp4"),
            directory.path().join("clip.mp4"),
        )
        .unwrap();
        assert!(
            matches!(
                resolve_external(directory.path(), &ExternalAssetRef::new("clip.mp4")),
                Err(ExtractionError::Escapes(_))
            ),
            "a symlink passes every lexical check and must fail this one"
        );
    }

    // -- staging root -------------------------------------------------------

    #[test]
    fn a_staging_root_is_private_and_disappears_with_its_generation() {
        let parent = tempfile::tempdir().unwrap();
        let path = {
            let staging = StagingRoot::create_in(parent.path()).unwrap();
            let first = staging.asset_dir().unwrap();
            let second = staging.asset_dir().unwrap();
            assert_ne!(first, second, "each asset gets its own subdirectory");
            assert!(first.starts_with(staging.path()));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(staging.path())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o700);
            }
            staging.path().to_path_buf()
        };
        assert!(!path.exists(), "the root is removed when it is dropped");
    }

    #[test]
    fn two_staging_roots_never_share_a_directory() {
        let parent = tempfile::tempdir().unwrap();
        let first = StagingRoot::create_in(parent.path()).unwrap();
        let second = StagingRoot::create_in(parent.path()).unwrap();
        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn a_staged_bundle_is_removed_with_its_staging_root() {
        let parent = tempfile::tempdir().unwrap();
        let staged = {
            let staging = StagingRoot::create_in(parent.path()).unwrap();
            let directory = staging.asset_dir().unwrap();
            let bundle =
                extract_bundle(&good_bundle(), &directory, &ExtractionLimits::default()).unwrap();
            assert!(bundle.root.join("index.html").exists());
            staging.path().to_path_buf()
        };
        assert!(
            !staged.exists(),
            "read-only staged files must not block cleanup"
        );
    }
}
