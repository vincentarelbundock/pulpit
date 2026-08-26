//! Comic archives: a `.cbz` or `.cbt` presented exactly as a directory of
//! images.
//!
//! `SPEC-reader-formats.md` §54. An image archive is Class A — a container
//! around content pulpit already renders — so it extends `SPEC-images.md`
//! almost unchanged: entries in natural sort order, one image per page, the
//! entry's name as page identity.
//!
//! Two things are different from a directory, and both simplify:
//!
//! * The archive **replaces the directory as the source**, so the document is
//!   one file again. Reload returns to `SourceStamp::File` and the digest of
//!   §42.3 is unnecessary — an archive is rewritten atomically or it is not
//!   rewritten (§54.2).
//! * Nothing is ever extracted to disk (§54.5). An entry is read into memory,
//!   bounded, decoded, and fed to the decoded-image cache like any other page.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;

use pulpit_core::document::MAX_TRACKED_PAGE_SIZES;

use crate::images::decode::ImageFailure;
use crate::images::table::{is_supported_image, ImageEntry, ListError};

/// The archive formats pulpit reads (§54.6).
///
/// Both are pure Rust and add no native dependency, which is the property
/// that puts them in scope at all.
pub const ARCHIVE_EXTENSIONS: &[&str] = &["cbz", "cbt"];

/// How the entries of one archive are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// `.cbz`, a zip.
    Zip,
    /// `.cbt`, a tar — plain, or gzipped, which is common enough in the wild
    /// that refusing it would read as "damaged" for a file that is not.
    Tar,
}

impl ArchiveKind {
    /// Which archive an extension names, if any. Extension alone, like every
    /// other listing decision (§41.1).
    pub fn of(path: &Path) -> Option<ArchiveKind> {
        let extension = path.extension().and_then(|e| e.to_str())?;
        if extension.eq_ignore_ascii_case("cbz") {
            Some(ArchiveKind::Zip)
        } else if extension.eq_ignore_ascii_case("cbt") {
            Some(ArchiveKind::Tar)
        } else {
            None
        }
    }
}

/// The largest an entry may claim to be, or turn out to be (§54.4).
///
/// Applied to the declared size *and* to the bytes actually produced, because
/// a zip's central directory is a claim rather than a fact and a bomb is
/// built out of exactly that gap.
pub const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

/// The largest an archive's entries may sum to, uncompressed (§54.4).
///
/// The bound that catches the classic overlapping-stream bomb: 42.zip is
/// 42 kB on disk and 4.5 petabytes expanded, and §47.2's pixel bound is
/// applied *after* decompression, far too late to help.
pub const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The largest number of pages an archive may hold, which is the same bound a
/// directory has (§40.5) and for the same reason.
pub const MAX_ENTRIES: usize = MAX_TRACKED_PAGE_SIZES;

/// List the image entries of an archive, in natural sort order.
///
/// Directory entries are **flattened rather than recursed into** (§54.3): a
/// `.cbz` with chapter subfolders is common, and its reading order is still
/// the sorted full path, so `chapter-2/page-01.jpg` keeps its whole name and
/// sorts on it.
pub fn list_archive(path: &Path, kind: ArchiveKind) -> Result<Vec<ImageEntry>, ListError> {
    let raw = match kind {
        ArchiveKind::Zip => list_zip(path),
        ArchiveKind::Tar => list_tar(path),
    }?;
    check_bounds(path, &raw)?;
    Ok(raw)
}

/// §54.4's three bounds, over what the archive *claims* about itself.
///
/// Applied to the listing, before a single entry is decompressed, which is
/// the whole point: a zip bomb reaches this code from an untrusted download,
/// and §47.2's pixel bound is applied after decompression, far too late.
/// The claims are checked again while reading, in [`read_bounded`], because a
/// central directory is a claim and a bomb is built out of that gap.
pub fn check_bounds(path: &Path, entries: &[ImageEntry]) -> Result<(), ListError> {
    if entries.len() > MAX_ENTRIES {
        return Err(ListError::TooManyImages {
            path: path.display().to_string(),
            count: entries.len(),
        });
    }
    let mut total: u64 = 0;
    for entry in entries {
        if entry.len > MAX_ENTRY_BYTES {
            return Err(ListError::EntryTooLarge {
                path: path.display().to_string(),
                entry: entry.name.to_string_lossy().into_owned(),
                bytes: entry.len,
            });
        }
        total = total.saturating_add(entry.len);
    }
    if total > MAX_TOTAL_BYTES {
        return Err(ListError::ArchiveTooLarge {
            path: path.display().to_string(),
            bytes: total,
        });
    }
    Ok(())
}

fn open_archive(path: &Path) -> Result<std::fs::File, ListError> {
    std::fs::File::open(path).map_err(|e| ListError::Unreadable {
        path: path.display().to_string(),
        reason: e.to_string(),
    })
}

fn broken(path: &Path, reason: impl std::fmt::Display) -> ListError {
    ListError::Unreadable {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

fn list_zip(path: &Path) -> Result<Vec<ImageEntry>, ListError> {
    let file = open_archive(path)?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| broken(path, e))?;
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|e| broken(path, e))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if !is_image_entry(&name) {
            continue;
        }
        entries.push(ImageEntry {
            name: OsString::from(name),
            // The declared uncompressed size. Bounded here so a bomb is
            // refused before anything is decompressed, and bounded *again*
            // while reading, because this number is only a claim.
            len: entry.size(),
            // An archive's mtime is the archive's; a per-entry timestamp adds
            // nothing, because the source is one file that is replaced whole
            // (§54.2). Left absent rather than invented.
            modified: None,
        });
    }
    Ok(entries)
}

fn list_tar(path: &Path) -> Result<Vec<ImageEntry>, ListError> {
    let mut entries = Vec::new();
    with_tar(path, |archive| {
        for entry in archive.entries().map_err(|e| broken(path, e))? {
            let entry = entry.map_err(|e| broken(path, e))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let name = match entry.path() {
                Ok(name) => name.to_string_lossy().into_owned(),
                Err(_) => continue,
            };
            if !is_image_entry(&name) {
                continue;
            }
            entries.push(ImageEntry {
                name: OsString::from(name),
                len: entry.header().size().unwrap_or(0),
                modified: None,
            });
        }
        Ok(())
    })?;
    Ok(entries)
}

/// Measure every entry's pixel dimensions in **one pass** over the archive.
///
/// Not an optimisation so much as the difference between opening a comic and
/// not: reading entries one at a time means re-reading a zip's central
/// directory, or re-scanning a whole tar, per page — quadratic in the page
/// count, on the path that runs before the first frame appears.
///
/// A page that will not decode is simply absent from the map. It stays in the
/// page table and fails its own render (§49.1); it does not disappear.
pub fn measure_entries(
    path: &Path,
    kind: ArchiveKind,
) -> std::collections::HashMap<OsString, (u32, u32)> {
    let mut measured = std::collections::HashMap::new();
    let mut record = |name: String, bytes: &[u8]| {
        let label = format!("{}!{name}", path.display());
        if let Ok(size) = crate::images::decode::dimensions_of(bytes, &label) {
            measured.insert(OsString::from(name), size);
        }
    };
    match kind {
        ArchiveKind::Zip => {
            if let Ok(file) = std::fs::File::open(path) {
                if let Ok(mut archive) = zip::ZipArchive::new(std::io::BufReader::new(file)) {
                    for index in 0..archive.len() {
                        let Ok(mut entry) = archive.by_index(index) else {
                            continue;
                        };
                        let name = entry.name().to_string();
                        if entry.is_dir() || !is_image_entry(&name) {
                            continue;
                        }
                        if let Ok(bytes) = read_bounded(&mut entry) {
                            record(name, &bytes);
                        }
                    }
                }
            }
        }
        ArchiveKind::Tar => {
            let _ = with_tar(path, |archive| {
                let Ok(entries) = archive.entries() else {
                    return Ok(());
                };
                for entry in entries {
                    let Ok(mut entry) = entry else { continue };
                    if !entry.header().entry_type().is_file() {
                        continue;
                    }
                    let Ok(name) = entry.path().map(|p| p.to_string_lossy().into_owned()) else {
                        continue;
                    };
                    if !is_image_entry(&name) {
                        continue;
                    }
                    if let Ok(bytes) = read_bounded(&mut entry) {
                        record(name, &bytes);
                    }
                }
                Ok(())
            });
        }
    }
    measured
}

/// Read one archive entry into memory, bounded (§54.4, §54.5).
pub fn read_entry(
    path: &Path,
    kind: ArchiveKind,
    name: &std::ffi::OsStr,
) -> Result<Vec<u8>, ImageFailure> {
    let wanted = name.to_string_lossy().into_owned();
    let label = format!("{}!{wanted}", path.display());
    let found = match kind {
        ArchiveKind::Zip => read_zip_entry(path, &wanted),
        ArchiveKind::Tar => read_tar_entry(path, &wanted),
    }
    .map_err(|reason| ImageFailure::Unreadable {
        path: label.clone(),
        reason,
    })?;
    found.ok_or(ImageFailure::Unreadable {
        path: label,
        reason: "no such entry in the archive".into(),
    })
}

fn read_zip_entry(path: &Path, wanted: &str) -> Result<Option<Vec<u8>>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let entry = match archive.by_name(wanted) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    read_bounded(entry).map(Some)
}

fn read_tar_entry(path: &Path, wanted: &str) -> Result<Option<Vec<u8>>, String> {
    let mut found = None;
    with_tar(path, |archive| {
        for entry in archive.entries().map_err(|e| broken(path, e))? {
            let mut entry = entry.map_err(|e| broken(path, e))?;
            let name = match entry.path() {
                Ok(name) => name.to_string_lossy().into_owned(),
                Err(_) => continue,
            };
            if name != wanted {
                continue;
            }
            found = Some(
                read_bounded(&mut entry).map_err(|reason| ListError::Unreadable {
                    path: path.display().to_string(),
                    reason,
                })?,
            );
            break;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    Ok(found)
}

fn read_bounded(source: impl Read) -> Result<Vec<u8>, String> {
    read_within(source, MAX_ENTRY_BYTES)
}

/// Read a stream, refusing at `limit` rather than allocating past it.
///
/// One byte past the bound is read on purpose: it is the difference between
/// "exactly at the limit" and "the declared size was a lie", and only the
/// second is a refusal. This is what catches an entry whose central-directory
/// size does not match what it actually expands to.
fn read_within(source: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut limited = source.take(limit + 1);
    limited.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("the entry expands past the {limit}-byte limit"));
    }
    Ok(bytes)
}

/// Run `body` over a tar, transparently ungzipping one that is compressed.
///
/// `.cbt` is nominally a plain tar, but gzipped ones exist and reporting one
/// as damaged would be the §61.2 mistake against a file that is perfectly
/// good. The magic is two bytes and `flate2` is already in the dependency
/// graph, so this costs nothing.
fn with_tar<F>(path: &Path, body: F) -> Result<(), ListError>
where
    F: FnOnce(&mut TarArchive<'_>) -> Result<(), ListError>,
{
    let file = open_archive(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut magic = [0u8; 2];
    let gzipped = match std::io::Read::read_exact(&mut reader, &mut magic) {
        Ok(()) => magic == [0x1f, 0x8b],
        // Shorter than two bytes: not a tar, and the reader below says so.
        Err(_) => false,
    };
    std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0)).map_err(|e| broken(path, e))?;
    if gzipped {
        let mut archive =
            tar::Archive::new(Box::new(flate2::read::GzDecoder::new(reader)) as Box<dyn Read>);
        body(&mut archive)
    } else {
        let mut archive = tar::Archive::new(Box::new(reader) as Box<dyn Read>);
        body(&mut archive)
    }
}

type TarArchive<'a> = tar::Archive<Box<dyn Read + 'a>>;

/// Is this entry name one of the page formats (§54.3, §41.2)?
///
/// The same extension set a directory is listed by, read from the same one
/// definition: an archive is a directory that happens to be one file.
fn is_image_entry(name: &str) -> bool {
    // Entries whose name has no final component — a bare directory marker
    // that did not set the directory flag — are not pages.
    let Some(last) = name.rsplit(['/', '\\']).next() else {
        return false;
    };
    // Nothing outside the archive is being opened, so a `..` in a name is not
    // a traversal risk here; it is simply part of the entry's identity. But a
    // dotfile a packer left behind (`__MACOSX/._page-01.jpg`) is not a page.
    if last.starts_with("._") {
        return false;
    }
    is_supported_image(Path::new(last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(width, height, image::Rgba([3, 4, 5, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn write_cbz(path: &Path, names: &[&str]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for name in names {
            writer.start_file(*name, options).unwrap();
            writer.write_all(&png(12, 8)).unwrap();
        }
        writer.finish().unwrap();
    }

    fn write_cbt(path: &Path, names: &[&str]) {
        let file = std::fs::File::create(path).unwrap();
        let mut builder = tar::Builder::new(file);
        for name in names {
            let bytes = png(12, 8);
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, bytes.as_slice())
                .unwrap();
        }
        builder.finish().unwrap();
    }

    fn names(entries: &[ImageEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_cbz_lists_its_image_entries_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        write_cbz(
            &path,
            &["page-01.png", "ComicInfo.xml", "cover.jpeg", "notes.txt"],
        );
        let entries = list_archive(&path, ArchiveKind::Zip).unwrap();
        let mut listed = names(&entries);
        listed.sort();
        assert_eq!(listed, ["cover.jpeg", "page-01.png"]);
    }

    #[test]
    fn a_cbt_lists_its_image_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbt");
        write_cbt(&path, &["page-01.png", "ComicInfo.xml"]);
        let entries = list_archive(&path, ArchiveKind::Tar).unwrap();
        assert_eq!(names(&entries), ["page-01.png"]);
    }

    /// §54.3: subfolders are flattened, not recursed into, and the reading
    /// order is the sorted full path.
    #[test]
    fn chapter_subfolders_are_flattened_and_keep_their_whole_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        write_cbz(
            &path,
            &[
                "chapter-2/page-01.png",
                "chapter-1/page-02.png",
                "chapter-1/page-10.png",
                "chapter-1/page-01.png",
            ],
        );
        let mut entries = list_archive(&path, ArchiveKind::Zip).unwrap();
        entries.sort_by(|a, b| crate::images::table::page_order(&a.name, &b.name));
        assert_eq!(
            names(&entries),
            [
                "chapter-1/page-01.png",
                "chapter-1/page-02.png",
                "chapter-1/page-10.png",
                "chapter-2/page-01.png",
            ],
            "natural sort over the full path, so page-2 precedes page-10 \
             inside a chapter and chapter-1 precedes chapter-2"
        );
    }

    #[test]
    fn a_macos_resource_fork_is_not_a_page() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        write_cbz(&path, &["page-01.png", "__MACOSX/._page-01.png"]);
        assert_eq!(
            list_archive(&path, ArchiveKind::Zip).unwrap().len(),
            1,
            "the resource fork is not a second copy of page one"
        );
    }

    #[test]
    fn an_entry_is_read_back_out_of_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        for (name, kind) in [
            ("comic.cbz", ArchiveKind::Zip),
            ("comic.cbt", ArchiveKind::Tar),
        ] {
            let path = dir.path().join(name);
            match kind {
                ArchiveKind::Zip => write_cbz(&path, &["a/page-01.png"]),
                ArchiveKind::Tar => write_cbt(&path, &["a/page-01.png"]),
            }
            let bytes = read_entry(&path, kind, std::ffi::OsStr::new("a/page-01.png")).unwrap();
            assert_eq!(bytes, png(12, 8), "{name}");
            assert!(
                read_entry(&path, kind, std::ffi::OsStr::new("nope.png")).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_gzipped_cbt_is_read_rather_than_called_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.tar");
        write_cbt(&plain, &["page-01.png"]);

        let path = dir.path().join("comic.cbt");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(&std::fs::read(&plain).unwrap()).unwrap();
        encoder.finish().unwrap();

        assert_eq!(
            names(&list_archive(&path, ArchiveKind::Tar).unwrap()),
            ["page-01.png"]
        );
        assert_eq!(
            read_entry(&path, ArchiveKind::Tar, std::ffi::OsStr::new("page-01.png")).unwrap(),
            png(12, 8)
        );
    }

    // §54.4, §63.1: each of the three bounds refuses rather than allocating.
    // Checked over the *claimed* sizes, which is what makes them cheap enough
    // to apply before anything is decompressed — and what lets them be tested
    // without building a real petabyte.
    fn claiming(name: &str, len: u64) -> ImageEntry {
        ImageEntry {
            name: OsString::from(name),
            len,
            modified: None,
        }
    }

    #[test]
    fn an_archive_claiming_too_many_pages_is_refused() {
        let entries: Vec<ImageEntry> = (0..MAX_ENTRIES + 1)
            .map(|index| claiming(&format!("page-{index:05}.png"), 1))
            .collect();
        assert!(matches!(
            check_bounds(Path::new("/comics/huge.cbz"), &entries),
            Err(ListError::TooManyImages { count, .. }) if count == MAX_ENTRIES + 1
        ));
        assert!(check_bounds(Path::new("/comics/ok.cbz"), &entries[..MAX_ENTRIES]).is_ok());
    }

    #[test]
    fn an_archive_with_one_enormous_entry_is_refused_and_names_it() {
        let entries = [
            claiming("page-01.png", 1024),
            claiming("bomb.png", MAX_ENTRY_BYTES + 1),
        ];
        let error = check_bounds(Path::new("/comics/bomb.cbz"), &entries).unwrap_err();
        assert!(
            error.to_string().contains("bomb.png"),
            "the offending entry is named: {error}"
        );
    }

    #[test]
    fn an_archive_that_expands_past_the_total_bound_is_refused() {
        let each = MAX_ENTRY_BYTES;
        let count = (MAX_TOTAL_BYTES / each) as usize + 1;
        let entries: Vec<ImageEntry> = (0..count)
            .map(|index| claiming(&format!("page-{index:05}.png"), each))
            .collect();
        assert!(matches!(
            check_bounds(Path::new("/comics/bomb.cbz"), &entries),
            Err(ListError::ArchiveTooLarge { .. })
        ));
    }

    /// The declared size is a claim, and a bomb is built out of the gap
    /// between the claim and what the entry actually produces. The read is
    /// bounded too, so an entry that lies stops at the limit instead of
    /// filling memory.
    #[test]
    fn an_entry_that_expands_past_its_bound_stops_rather_than_filling_memory() {
        let honest = read_within(std::io::repeat(0u8).take(64), 128).unwrap();
        assert_eq!(honest.len(), 64);
        assert_eq!(
            read_within(std::io::repeat(0u8).take(128), 128)
                .unwrap()
                .len(),
            128
        );

        let error = read_within(std::io::repeat(0u8), 128).unwrap_err();
        assert!(error.contains("128"), "{error}");
    }

    #[test]
    fn a_corrupt_archive_is_unreadable_rather_than_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.cbz");
        std::fs::write(&path, b"this is not a zip").unwrap();
        assert!(matches!(
            list_archive(&path, ArchiveKind::Zip),
            Err(ListError::Unreadable { .. })
        ));
    }

    /// §54.7's message lives in the one refusal table now
    /// (`crate::formats`). What is still this module's claim is that a comic
    /// format pulpit refuses is not mistaken for an archive it reads.
    #[test]
    fn a_refused_comic_format_is_not_an_archive_kind() {
        for name in ["/comics/book.cbr", "/comics/book.cb7"] {
            assert_eq!(ArchiveKind::of(Path::new(name)), None);
            assert!(crate::formats::unsupported_format(Path::new(name)).is_some());
        }
    }

    #[test]
    fn the_archive_kinds_are_decided_by_extension_alone() {
        assert_eq!(ArchiveKind::of(Path::new("a.cbz")), Some(ArchiveKind::Zip));
        assert_eq!(ArchiveKind::of(Path::new("a.CBT")), Some(ArchiveKind::Tar));
        assert_eq!(ArchiveKind::of(Path::new("a.cbr")), None);
        assert_eq!(ArchiveKind::of(Path::new("a.pdf")), None);
        for extension in ARCHIVE_EXTENSIONS {
            assert!(ArchiveKind::of(Path::new(&format!("a.{extension}"))).is_some());
        }
    }
}
