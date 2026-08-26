//! Where an installed djvulibre might be (`SPEC-reader-formats.md` §55.3).
//!
//! §55.4 makes DjVu a capability of the **machine** rather than of the build,
//! and that only holds if a djvulibre the machine has is a djvulibre pulpit
//! finds. Handing two bare names to the platform loader is not enough for
//! that: it is the whole answer on Debian and Fedora, and no answer at all on
//! Nix, Guix, Homebrew or a MacPorts prefix, where the library is installed,
//! working, and simply not on any path the loader consults.
//!
//! So the search goes, most specific first:
//!
//! 1. `PULPIT_DJVU_PATH`, which names the library or a directory holding it.
//! 2. The bare names, through the platform loader — the machine's own answer,
//!    and the right one wherever it exists.
//! 3. **Beside djvulibre's own tools.** `ddjvu` and `djvused` ship with the
//!    library and are on `PATH` wherever somebody installed it, so the tool is
//!    a signpost to the library even when nothing else is. Three things are
//!    read off it: the `lib` directory beside its `bin`, its own directory
//!    (which is where a Windows DLL sits), and — the case that matters on Nix
//!    — the run-time search path recorded *inside* the executable.
//! 4. The handful of prefixes a package manager uses.
//!
//! Step 3 is what makes this reliable rather than merely broad. On NixOS,
//! `djvused` resolves to `/nix/store/…-djvulibre-3.5.29-bin/bin/djvused`,
//! whose `../lib` does not exist — the library is a *separate* store path with
//! a different hash, which nothing about the tool's own path reveals. Its
//! `DT_RUNPATH` names that path exactly. The Mach-O equivalent is the absolute
//! `LC_LOAD_DYLIB` install name, which Homebrew and nix-darwin both record.
//!
//! Nothing here reads a directory beside *pulpit's* executable. A djvulibre
//! found there would be a bundled one, and §65.1 forbids bundling a Class B
//! library.

use std::path::{Path, PathBuf};

/// The file names a system-installed djvulibre goes by.
pub(crate) fn library_names() -> &'static [&'static str] {
    // Naming a shared library is the platform boundary's business, not a
    // capability question, so this is the one place a target check belongs.
    if cfg!(target_os = "windows") {
        &["libdjvulibre.dll", "djvulibre.dll"]
    } else if cfg!(target_os = "macos") {
        &["libdjvulibre.21.dylib", "libdjvulibre.dylib"]
    } else {
        &["libdjvulibre.so.21", "libdjvulibre.so"]
    }
}

/// The command-line tools djvulibre installs beside its library.
const TOOLS: &[&str] = &["ddjvu", "djvused", "djvutxt"];

/// How much of a program pulpit will read looking for a library path.
///
/// The tools are a few hundred kilobytes. Anything past this is not one of
/// them, and reading it would be work done on a file that cannot answer.
const MAX_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;

/// Every place a djvulibre might be, most specific first.
///
/// Paths are candidates, not findings: each is handed to the loader in turn
/// and every one of them is allowed to fail. Nothing here touches a library.
pub(crate) fn candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("PULPIT_DJVU_PATH") {
        let configured = PathBuf::from(configured);
        if configured.is_dir() {
            candidates.extend(library_names().iter().map(|name| configured.join(name)));
        } else {
            candidates.push(configured);
        }
    }
    // The loader first, before anything guessed: on a machine where it can
    // answer, its answer is the one the rest of the system uses too.
    candidates.extend(library_names().iter().map(PathBuf::from));

    let mut directories = Vec::new();
    for tool in tools_on_path() {
        directories.extend(beside(&tool));
        directories.extend(recorded_in(&tool));
    }
    directories.extend(well_known());

    for directory in directories {
        candidates.extend(library_names().iter().map(|name| directory.join(name)));
    }
    candidates.dedup();
    candidates
}

/// Every djvulibre tool on `PATH`, with symlinks resolved.
///
/// Resolved because the useful part is where the program really lives:
/// `/run/current-system/sw/bin/djvused` says nothing, and the store path it
/// points at says everything.
fn tools_on_path() -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for directory in std::env::split_paths(&path) {
        for tool in TOOLS {
            let executable = directory.join(if cfg!(target_os = "windows") {
                format!("{tool}.exe")
            } else {
                (*tool).to_string()
            });
            if let Ok(resolved) = executable.canonicalize() {
                if !found.contains(&resolved) {
                    found.push(resolved);
                }
            }
        }
    }
    found
}

/// The library directories that sit beside one installed program.
///
/// `<prefix>/bin/ddjvu` puts the library in `<prefix>/lib` on every Unix
/// prefix there is — `/usr`, `/usr/local`, a Homebrew cellar, a MacPorts tree,
/// a conda environment. The program's own directory is there for Windows,
/// which has no such split and keeps the DLL next to the executable.
fn beside(program: &Path) -> Vec<PathBuf> {
    let Some(directory) = program.parent() else {
        return Vec::new();
    };
    let mut directories = vec![directory.to_path_buf()];
    if let Some(prefix) = directory.parent() {
        directories.push(prefix.join("lib"));
        directories.push(prefix.join("lib64"));
    }
    directories
}

/// The library directories recorded *inside* one program.
///
/// ELF's `DT_RUNPATH` and `DT_RPATH`, or Mach-O's `LC_LOAD_DYLIB` install
/// names. This is the case a prefix cannot answer: a Nix store path holds the
/// library under a different hash from the tools, and only the tool itself
/// knows which one.
fn recorded_in(program: &Path) -> Vec<PathBuf> {
    let Ok(metadata) = std::fs::metadata(program) else {
        return Vec::new();
    };
    if metadata.len() > MAX_PROGRAM_BYTES {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(program) else {
        return Vec::new();
    };
    let origin = program.parent().unwrap_or(Path::new("."));
    elf_search_paths(&bytes)
        .into_iter()
        .map(|entry| expand(&entry, origin))
        .chain(mach_o_library_directories(&bytes))
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect()
}

/// `$ORIGIN`, which an ELF search path uses to mean "beside this program".
///
/// `$LIB` and `$PLATFORM` are left alone: they expand to something the loader
/// knows and this does not, and a path that keeps them simply fails to open,
/// which is what every candidate here is allowed to do.
fn expand(entry: &str, origin: &Path) -> PathBuf {
    if !entry.contains("$ORIGIN") && !entry.contains("${ORIGIN}") {
        return PathBuf::from(entry);
    }
    let origin = origin.to_string_lossy();
    PathBuf::from(
        entry
            .replace("${ORIGIN}", &origin)
            .replace("$ORIGIN", &origin),
    )
}

/// The prefixes a package manager puts a library under.
///
/// Only ever additions to the loader's own answer, and cheap: a path that does
/// not exist costs one failed open.
fn well_known() -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    // A Nix profile that carries the library's own output rather than only the
    // tools. `NIX_PROFILES` is what a Nix session sets to say where it looks.
    if let Some(profiles) = std::env::var_os("NIX_PROFILES") {
        directories.extend(
            profiles
                .to_string_lossy()
                .split_whitespace()
                .map(|profile| Path::new(profile).join("lib")),
        );
    }
    if cfg!(target_os = "macos") {
        directories.extend(
            [
                "/opt/homebrew/lib",
                "/opt/homebrew/opt/djvulibre/lib",
                "/usr/local/lib",
                "/usr/local/opt/djvulibre/lib",
                "/opt/local/lib",
            ]
            .map(PathBuf::from),
        );
    } else if !cfg!(target_os = "windows") {
        directories.extend(
            [
                "/usr/lib",
                "/usr/lib64",
                "/usr/local/lib",
                "/usr/local/lib64",
                "/lib",
                "/lib64",
            ]
            .map(PathBuf::from),
        );
        // Debian and its derivatives put libraries under a triplet directory,
        // which is the one place on those systems the bare names would not be
        // found if the cache were stale.
        directories.push(PathBuf::from(format!(
            "/usr/lib/{}-linux-gnu",
            std::env::consts::ARCH
        )));
    }
    directories
}

// ---------------------------------------------------------------------------
// The two executable formats, read only far enough to answer one question.
//
// Both readers are total: every field is bounds-checked against the bytes
// actually present, every failure is an empty answer, and neither of them
// follows a length or an offset without checking it first. They are handed
// whatever was on `PATH` under a djvulibre tool's name, which is not
// necessarily a djvulibre tool.
// ---------------------------------------------------------------------------

const ELF_MAGIC: &[u8] = b"\x7fELF";
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: u64 = 0;
const DT_STRTAB: u64 = 5;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;

/// One ELF program header, in the only three fields this needs.
struct Segment {
    kind: u32,
    offset: u64,
    virtual_address: u64,
    file_size: u64,
}

/// `DT_RUNPATH` and `DT_RPATH`, split on the colons that separate them.
fn elf_search_paths(bytes: &[u8]) -> Vec<String> {
    let Some(elf) = Elf::read(bytes) else {
        return Vec::new();
    };
    let Some(dynamic) = elf.segments.iter().find(|s| s.kind == PT_DYNAMIC) else {
        return Vec::new();
    };

    // The dynamic section's strings are addressed by where they will be in
    // memory, not by where they are in the file, so the loadable segments are
    // what translates one into the other.
    let mut string_table = None;
    let mut entries = Vec::new();
    let stride = if elf.is_64_bit { 16 } else { 8 };
    let mut at = dynamic.offset as usize;
    let end = at
        .saturating_add(dynamic.file_size as usize)
        .min(bytes.len());
    while at + stride <= end {
        let pair = if elf.is_64_bit {
            elf.word(bytes, at)
                .zip(elf.word(bytes, at.saturating_add(8)))
        } else {
            elf.half_word(bytes, at)
                .zip(elf.half_word(bytes, at.saturating_add(4)))
                .map(|(tag, value)| (tag as u64, value as u64))
        };
        let Some((tag, value)) = pair else {
            break;
        };
        match tag {
            DT_NULL => break,
            DT_STRTAB => string_table = Some(value),
            DT_RPATH | DT_RUNPATH => entries.push(value),
            _ => {}
        }
        at += stride;
    }

    let Some(table) = string_table.and_then(|address| elf.file_offset(address)) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter_map(|offset| read_c_string(bytes, table.checked_add(offset)? as usize))
        .flat_map(|paths| {
            paths
                .split(':')
                .map(str::to_string)
                .collect::<Vec<_>>()
                .into_iter()
        })
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// Just enough of an ELF header to walk its segments.
struct Elf {
    is_64_bit: bool,
    little_endian: bool,
    segments: Vec<Segment>,
}

impl Elf {
    fn read(bytes: &[u8]) -> Option<Elf> {
        if bytes.get(..4)? != ELF_MAGIC {
            return None;
        }
        let is_64_bit = match bytes.get(4)? {
            1 => false,
            2 => true,
            _ => return None,
        };
        let little_endian = match bytes.get(5)? {
            1 => true,
            2 => false,
            _ => return None,
        };
        let mut elf = Elf {
            is_64_bit,
            little_endian,
            segments: Vec::new(),
        };
        let (header_offset, size_at, count_at) = if is_64_bit {
            (elf.word(bytes, 32)?, 54, 56)
        } else {
            (elf.half_word(bytes, 28)? as u64, 42, 44)
        };
        let entry_size = elf.short(bytes, size_at)? as usize;
        let count = elf.short(bytes, count_at)? as usize;
        // A segment table that does not fit the file, or entries too small to
        // hold the fields read below, is a file to walk away from.
        let minimum = if is_64_bit { 56 } else { 32 };
        if entry_size < minimum || count > 4096 {
            return None;
        }
        for index in 0..count {
            let at = (header_offset as usize).checked_add(index.checked_mul(entry_size)?)?;
            let field = |offset: usize| at.checked_add(offset);
            let segment = if is_64_bit {
                Segment {
                    kind: elf.half_word(bytes, at)?,
                    offset: elf.word(bytes, field(8)?)?,
                    virtual_address: elf.word(bytes, field(16)?)?,
                    file_size: elf.word(bytes, field(32)?)?,
                }
            } else {
                Segment {
                    kind: elf.half_word(bytes, at)?,
                    offset: elf.half_word(bytes, field(4)?)? as u64,
                    virtual_address: elf.half_word(bytes, field(8)?)? as u64,
                    file_size: elf.half_word(bytes, field(16)?)? as u64,
                }
            };
            elf.segments.push(segment);
        }
        Some(elf)
    }

    /// Where a memory address lands in the file, through the segment that
    /// covers it.
    fn file_offset(&self, address: u64) -> Option<u64> {
        self.segments
            .iter()
            .filter(|segment| segment.kind == PT_LOAD)
            .find_map(|segment| {
                let inside = address.checked_sub(segment.virtual_address)?;
                (inside < segment.file_size).then(|| segment.offset.checked_add(inside))?
            })
    }

    fn short(&self, bytes: &[u8], at: usize) -> Option<u16> {
        let raw = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.little_endian {
            u16::from_le_bytes(raw)
        } else {
            u16::from_be_bytes(raw)
        })
    }

    fn half_word(&self, bytes: &[u8], at: usize) -> Option<u32> {
        let raw = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    }

    fn word(&self, bytes: &[u8], at: usize) -> Option<u64> {
        let raw = bytes.get(at..at.checked_add(8)?)?.try_into().ok()?;
        Some(if self.little_endian {
            u64::from_le_bytes(raw)
        } else {
            u64::from_be_bytes(raw)
        })
    }
}

const MACH_O_64: u32 = 0xfeed_facf;
const MACH_O_64_SWAPPED: u32 = 0xcffa_edfe;
const LC_LOAD_DYLIB: u32 = 0x0c;

/// The directories of the djvulibre dylibs one Mach-O program loads.
///
/// A Mach-O records the *absolute* install name of what it links against, so
/// a Homebrew or nix-darwin `ddjvu` names the library's directory outright.
fn mach_o_library_directories(bytes: &[u8]) -> Vec<PathBuf> {
    let Some(magic) = bytes.get(..4).and_then(|raw| raw.try_into().ok()) else {
        return Vec::new();
    };
    let magic = u32::from_le_bytes(magic);
    // 32-bit Mach-O is not read: no macOS that can run this is 32-bit.
    let little_endian = match magic {
        MACH_O_64 => true,
        MACH_O_64_SWAPPED => false,
        _ => return Vec::new(),
    };
    let read = |at: usize| -> Option<u32> {
        let raw = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    };
    let Some(count) = read(16) else {
        return Vec::new();
    };
    let mut directories = Vec::new();
    let mut at = 32; // The 64-bit header, including its reserved word.
    for _ in 0..count.min(4096) {
        let (Some(kind), Some(size)) = (read(at), read(at + 4)) else {
            break;
        };
        let size = size as usize;
        if size < 8 || at.saturating_add(size) > bytes.len() {
            break;
        }
        if kind == LC_LOAD_DYLIB {
            // A dylib command carries the offset of its name, from the start
            // of the command itself.
            if let Some(name_at) = read(at + 8).map(|offset| at.saturating_add(offset as usize)) {
                if name_at < at + size {
                    if let Some(name) = read_c_string(bytes, name_at) {
                        let name = PathBuf::from(name);
                        let is_djvulibre = name
                            .file_name()
                            .and_then(|file| file.to_str())
                            .is_some_and(|file| file.starts_with("libdjvulibre"));
                        if is_djvulibre {
                            if let Some(directory) = name.parent() {
                                directories.push(directory.to_path_buf());
                            }
                        }
                    }
                }
            }
        }
        at += size;
    }
    directories
}

/// A NUL-terminated string, bounded by the bytes that are actually there.
fn read_c_string(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let end = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    // Bounded because this is a length-free string in a file pulpit did not
    // write: a path is a path, and a megabyte of one is a malformed file.
    if end > 4096 {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian 64-bit ELF carrying one search path, built here rather
    /// than checked in: the parser must be pinned by something that is not
    /// whichever machine happens to run the tests.
    fn elf_64_le(search_path: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[..4].copy_from_slice(ELF_MAGIC);
        bytes[4] = 2; // 64-bit
        bytes[5] = 1; // little-endian
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes()); // e_phnum

        let dynamic_at = 64 + 2 * 56;
        let strings_at = dynamic_at + 3 * 16;
        // The strings live at this address in memory; the loadable segment
        // below is what maps that back to where they are in the file.
        const BASE: u64 = 0x1000;

        let mut load = vec![0u8; 56];
        load[..4].copy_from_slice(&PT_LOAD.to_le_bytes());
        load[8..16].copy_from_slice(&0u64.to_le_bytes()); // p_offset
        load[16..24].copy_from_slice(&BASE.to_le_bytes()); // p_vaddr
        load[32..40].copy_from_slice(&0xffffu64.to_le_bytes()); // p_filesz
        bytes.extend(load);

        let mut dynamic = vec![0u8; 56];
        dynamic[..4].copy_from_slice(&PT_DYNAMIC.to_le_bytes());
        dynamic[8..16].copy_from_slice(&(dynamic_at as u64).to_le_bytes());
        dynamic[32..40].copy_from_slice(&(3u64 * 16).to_le_bytes());
        bytes.extend(dynamic);

        for (tag, value) in [
            (DT_STRTAB, BASE + strings_at as u64),
            (DT_RUNPATH, 1), // The first byte of a string table is a NUL.
            (DT_NULL, 0),
        ] {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        bytes.push(0);
        bytes.extend(search_path.as_bytes());
        bytes.push(0);
        bytes
    }

    /// The case that motivated all of this: a Nix store path, which no prefix
    /// rule and no loader path would ever produce.
    #[test]
    fn a_run_time_search_path_is_read_out_of_an_elf() {
        let store = "/nix/store/aaaa-djvulibre-3.5.29-lib/lib";
        let bytes = elf_64_le(&format!("{store}:/nix/store/bbbb-glibc/lib"));
        assert_eq!(
            elf_search_paths(&bytes),
            vec![store.to_string(), "/nix/store/bbbb-glibc/lib".to_string()],
            "both entries, split on the colon between them"
        );
    }

    /// `$ORIGIN` is the loader's way of saying "beside this program", and a
    /// relocatable prefix — a Homebrew cellar, an AppImage, a conda
    /// environment — writes its whole search path in terms of it.
    #[test]
    fn origin_is_expanded_to_the_programs_own_directory() {
        assert_eq!(
            expand("$ORIGIN/../lib", Path::new("/opt/thing/bin")),
            PathBuf::from("/opt/thing/bin/../lib")
        );
        assert_eq!(
            expand("${ORIGIN}/../lib64", Path::new("/opt/thing/bin")),
            PathBuf::from("/opt/thing/bin/../lib64")
        );
        // `$LIB` is the loader's, not this one's: left alone, it fails to open
        // like any other candidate, which is what they are all allowed to do.
        assert_eq!(
            expand("/usr/$LIB", Path::new("/bin")),
            PathBuf::from("/usr/$LIB")
        );
    }

    /// macOS records the absolute install name of what it links against, so
    /// the dylib names its own directory — the Mach-O answer to the question
    /// `DT_RUNPATH` answers on Linux.
    #[test]
    fn a_dylib_directory_is_read_out_of_a_mach_o() {
        let name = "/opt/homebrew/opt/djvulibre/lib/libdjvulibre.21.dylib";
        let mut command = vec![0u8; 24];
        command[..4].copy_from_slice(&LC_LOAD_DYLIB.to_le_bytes());
        command[8..12].copy_from_slice(&24u32.to_le_bytes()); // name offset
        command.extend(name.as_bytes());
        command.push(0);
        while !command.len().is_multiple_of(8) {
            command.push(0);
        }
        let size = command.len() as u32;
        command[4..8].copy_from_slice(&size.to_le_bytes());

        let mut bytes = vec![0u8; 32];
        bytes[..4].copy_from_slice(&MACH_O_64.to_le_bytes());
        bytes[16..20].copy_from_slice(&1u32.to_le_bytes()); // ncmds
        bytes.extend(command);

        assert_eq!(
            mach_o_library_directories(&bytes),
            vec![PathBuf::from("/opt/homebrew/opt/djvulibre/lib")]
        );
    }

    /// Anything on `PATH` under a djvulibre tool's name is read here, and most
    /// of what could be there is not a djvulibre tool. Every reader answers
    /// "nothing" rather than reaching past the bytes it was given.
    #[test]
    fn nothing_that_is_not_an_executable_makes_a_reader_reach_past_its_bytes() {
        for bytes in [
            b"#!/bin/sh\nexec ddjvu \"$@\"\n".to_vec(),
            Vec::new(),
            ELF_MAGIC.to_vec(),
            b"\x7fELF\x02\x01".to_vec(),
            {
                // A well-formed header whose segment table points off the end.
                let mut truncated = elf_64_le("/somewhere");
                truncated.truncate(70);
                truncated
            },
            {
                // Every offset in the file, at its maximum.
                let mut absurd = elf_64_le("/somewhere");
                absurd[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
                absurd
            },
            vec![0xff; 256],
        ] {
            assert!(elf_search_paths(&bytes).is_empty());
            assert!(mach_o_library_directories(&bytes).is_empty());
        }
    }

    /// The order is the argument: what somebody configured, then what the
    /// machine's own loader says, and only then anything derived or guessed.
    #[test]
    fn the_loader_is_asked_before_anything_is_derived_or_guessed() {
        let candidates = candidates();
        let bare = candidates
            .iter()
            .position(|candidate| candidate == Path::new(library_names()[0]))
            .expect("the bare names are always asked");
        assert!(
            candidates.iter().skip(bare).any(|c| c.is_absolute()),
            "derived and well-known directories come after the loader"
        );
        assert!(
            candidates.iter().all(|candidate| candidate
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains("djvulibre"))),
            "every candidate names the library, so a wrong guess opens nothing"
        );
    }
}
