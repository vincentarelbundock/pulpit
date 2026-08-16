//! Checking exported PDFs with somebody else's PDF implementation.
//!
//! Filling a form and reading it back with the same library proves that the
//! library agrees with itself. It does not prove the file says what it should:
//! a value written where only PDFium looks for it, or a field left to
//! `/NeedAppearances` that most viewers ignore, passes a self-round-trip and
//! still opens blank in Preview, Chrome, and Acrobat.
//!
//! So the assertions that matter most are made with MuPDF and Poppler —
//! independent implementations, neither sharing code with PDFium.
//!
//! When no external engine is installed these checks step aside and say so,
//! because a test suite that silently degrades is worse than one that is
//! honestly narrower. Set `PDFFORM_REQUIRE_VERIFIER=1` to turn a missing
//! engine into a failure, which is what continuous integration should do.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The external PDF implementations found on this machine.
pub struct Engines {
    /// MuPDF's `mutool`: an object-graph dump and a renderer.
    pub mutool: Option<PathBuf>,
    /// Poppler's `pdftoppm`: a renderer.
    pub pdftoppm: Option<PathBuf>,
    /// Poppler's `pdftotext`: text extraction.
    pub pdftotext: Option<PathBuf>,
}

fn find(binary: &str, environment: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(environment) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(binary))
        .find(|candidate| candidate.is_file())
}

impl Engines {
    pub fn detect() -> &'static Engines {
        static ENGINES: OnceLock<Engines> = OnceLock::new();
        ENGINES.get_or_init(|| Engines {
            mutool: find("mutool", "PDFFORM_MUTOOL"),
            pdftoppm: find("pdftoppm", "PDFFORM_PDFTOPPM"),
            pdftotext: find("pdftotext", "PDFFORM_PDFTOTEXT"),
        })
    }

    pub fn describe(&self) -> String {
        let mut found = Vec::new();
        if self.mutool.is_some() {
            found.push("mutool");
        }
        if self.pdftoppm.is_some() {
            found.push("pdftoppm");
        }
        if self.pdftotext.is_some() {
            found.push("pdftotext");
        }
        if found.is_empty() {
            "none".into()
        } else {
            found.join(", ")
        }
    }

    /// Whether a check needing `engine` can run, complaining in the way the
    /// environment asks for when it cannot.
    fn available(&self, engine: Option<&PathBuf>, name: &str, what: &str) -> bool {
        if engine.is_some() {
            return true;
        }
        let required = std::env::var("PDFFORM_REQUIRE_VERIFIER")
            .map(|value| value != "0" && !value.is_empty())
            .unwrap_or(false);
        assert!(
            !required,
            "PDFFORM_REQUIRE_VERIFIER is set but {name} is not installed, so {what} \
             cannot be checked against an independent implementation"
        );
        eprintln!("SKIP: {what} — {name} not installed (set PDFFORM_MUTOOL, or install it)");
        false
    }

    /// Can the object graph be inspected independently?
    pub fn can_read_objects(&self, what: &str) -> bool {
        self.available(self.mutool.as_ref(), "mutool", what)
    }

    /// Can the page be rendered by an engine that is not PDFium?
    pub fn can_render(&self, what: &str) -> bool {
        if self.mutool.is_some() || self.pdftoppm.is_some() {
            return true;
        }
        self.available(None, "mutool or pdftoppm", what)
    }

    /// Every object in the file, one per line, as MuPDF reads them.
    pub fn objects(&self, pdf: &Path) -> Option<Vec<String>> {
        let mutool = self.mutool.as_ref()?;
        let output = Command::new(mutool)
            .arg("show")
            .arg(pdf)
            .arg("grep")
            .output()
            .ok()?;
        Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_owned)
                .collect(),
        )
    }

    /// The `/V` of every object whose `/T` is `name`, as MuPDF sees them.
    ///
    /// Returns an empty vector when the field exists with no value, and `None`
    /// only when MuPDF is unavailable or the file would not parse at all.
    pub fn field_values(&self, pdf: &Path, name: &str) -> Option<Vec<String>> {
        let objects = self.objects(pdf)?;
        let mut values = Vec::new();
        for line in objects {
            match entry(&line, "/T") {
                Some(found) if decode(&found) == name => {}
                _ => continue,
            }
            values.push(
                entry(&line, "/V")
                    .map(|raw| decode(&raw))
                    .unwrap_or_default(),
            );
        }
        Some(values)
    }

    /// Every `/T` in the file, so a test can assert on what a foreign reader
    /// believes the fields are called.
    pub fn field_names(&self, pdf: &Path) -> Option<Vec<String>> {
        Some(
            self.objects(pdf)?
                .iter()
                .filter_map(|line| entry(line, "/T"))
                .map(|raw| decode(&raw))
                .collect(),
        )
    }

    pub fn text(&self, pdf: &Path) -> Option<String> {
        let pdftotext = self.pdftotext.as_ref()?;
        let output = Command::new(pdftotext)
            .arg("-q")
            .arg(pdf)
            .arg("-")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Render `page` with an engine that is not PDFium.
    pub fn render(&self, pdf: &Path, page: usize, dpi: u32) -> Option<Gray> {
        if let Some(mutool) = self.mutool.as_ref() {
            let output = Command::new(mutool)
                .args(["draw", "-F", "pgm", "-r", &dpi.to_string(), "-o", "-"])
                .arg(pdf)
                .arg((page + 1).to_string())
                .output()
                .ok()?;
            if let Some(image) = Gray::from_pgm(&output.stdout) {
                return Some(image);
            }
        }
        let pdftoppm = self.pdftoppm.as_ref()?;
        let directory = tempfile::tempdir().ok()?;
        let prefix = directory.path().join("page");
        let status = Command::new(pdftoppm)
            .args(["-gray", "-r", &dpi.to_string()])
            .args(["-f", &(page + 1).to_string(), "-l", &(page + 1).to_string()])
            .arg(pdf)
            .arg(&prefix)
            .output()
            .ok()?;
        let _ = status;
        let rendered = std::fs::read_dir(directory.path())
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "pgm"))?;
        Gray::from_pgm(&std::fs::read(rendered).ok()?)
    }
}

/// A rendered page in 8-bit grayscale.
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Gray {
    /// Parse binary PGM (`P5`), which both engines emit.
    pub fn from_pgm(bytes: &[u8]) -> Option<Gray> {
        let mut fields = Vec::new();
        let mut at = 0;
        // Magic, width, height, maximum: whitespace-separated, `#` comments.
        while fields.len() < 4 {
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            if at < bytes.len() && bytes[at] == b'#' {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
                continue;
            }
            let start = at;
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            if start == at {
                return None;
            }
            fields.push(std::str::from_utf8(&bytes[start..at]).ok()?.to_owned());
        }
        if fields[0] != "P5" {
            return None;
        }
        let width: u32 = fields[1].parse().ok()?;
        let height: u32 = fields[2].parse().ok()?;
        at += 1; // the single whitespace byte after the maximum value
        let wanted = width as usize * height as usize;
        let pixels = bytes.get(at..at + wanted)?.to_vec();
        Some(Gray {
            width,
            height,
            pixels,
        })
    }

    /// The fraction of pixels darker than `threshold` inside a rectangle given
    /// in normalized page coordinates (0..1, y downward).
    pub fn ink(&self, left: f32, top: f32, right: f32, bottom: f32, threshold: u8) -> f32 {
        let x0 = ((left.clamp(0.0, 1.0) * self.width as f32) as u32).min(self.width);
        let x1 = ((right.clamp(0.0, 1.0) * self.width as f32).ceil() as u32).min(self.width);
        let y0 = ((top.clamp(0.0, 1.0) * self.height as f32) as u32).min(self.height);
        let y1 = ((bottom.clamp(0.0, 1.0) * self.height as f32).ceil() as u32).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return 0.0;
        }
        let mut dark = 0u64;
        let mut total = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = self.pixels[(y as usize) * self.width as usize + x as usize];
                total += 1;
                if pixel < threshold {
                    dark += 1;
                }
            }
        }
        if total == 0 {
            0.0
        } else {
            dark as f32 / total as f32
        }
    }

    /// The fraction of the whole page that is not blank.
    pub fn total_ink(&self, threshold: u8) -> f32 {
        self.ink(0.0, 0.0, 1.0, 1.0, threshold)
    }

    /// The number of pixels darker than `threshold` inside a normalized
    /// rectangle.
    ///
    /// Counts rather than fractions, so that ink inside a region and ink
    /// everywhere else can be subtracted from one another — which is how a
    /// test asks not just "did the mark appear" but "did it appear *only*
    /// where it was put".
    pub fn dark_in(&self, left: f32, top: f32, right: f32, bottom: f32, threshold: u8) -> u64 {
        let x0 = ((left.clamp(0.0, 1.0) * self.width as f32) as u32).min(self.width);
        let x1 = ((right.clamp(0.0, 1.0) * self.width as f32).ceil() as u32).min(self.width);
        let y0 = ((top.clamp(0.0, 1.0) * self.height as f32) as u32).min(self.height);
        let y1 = ((bottom.clamp(0.0, 1.0) * self.height as f32).ceil() as u32).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return 0;
        }
        let mut dark = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                if self.pixels[(y as usize) * self.width as usize + x as usize] < threshold {
                    dark += 1;
                }
            }
        }
        dark
    }

    /// Every dark pixel on the page.
    pub fn dark_total(&self, threshold: u8) -> u64 {
        self.dark_in(0.0, 0.0, 1.0, 1.0, threshold)
    }

    /// Dark pixels anywhere except inside the given rectangle.
    pub fn dark_outside(&self, left: f32, top: f32, right: f32, bottom: f32, threshold: u8) -> u64 {
        self.dark_total(threshold)
            .saturating_sub(self.dark_in(left, top, right, bottom, threshold))
    }
}

/// The raw text of `key`'s value in a one-line object dump.
///
/// MuPDF prints dictionaries without spaces, so the value begins immediately
/// after the key and runs to the end of its own bracketing. Keys are matched
/// only when the next byte cannot continue a name, so `/V` does not match
/// `/Version`.
fn entry(line: &str, key: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut at = 0;
    while let Some(found) = line[at..].find(key) {
        let start = at + found;
        let after = start + key.len();
        at = after;
        let next = bytes.get(after).copied();
        let continues_name = next.is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_'
        });
        if continues_name {
            continue;
        }
        return Some(match next {
            Some(b'(') => balanced(&line[after..], b'(', b')'),
            Some(b'[') => balanced(&line[after..], b'[', b']'),
            Some(b'<') => balanced(&line[after..], b'<', b'>'),
            Some(b'/') => {
                let rest = &line[after..];
                let end = rest[1..]
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '#' && c != '_' && c != '-')
                    .map(|index| index + 1)
                    .unwrap_or(rest.len());
                rest[..end].to_owned()
            }
            _ => {
                let rest = line[after..].trim_start();
                let end = rest
                    .find(|c: char| c == '/' || c == '>' || c == ']' || c.is_whitespace())
                    .unwrap_or(rest.len());
                rest[..end].to_owned()
            }
        });
    }
    None
}

/// The bracketed run starting at the front of `text`, respecting escapes and
/// nesting, so a `)` inside a string does not end it early.
fn balanced(text: &str, open: u8, close: u8) -> String {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match *byte {
            b'\\' if open == b'(' => escaped = true,
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return text[..=index].to_owned();
                }
            }
            _ => {}
        }
    }
    text.to_owned()
}

/// Turn a raw PDF value into the text it stands for: literal strings
/// unescaped, hex strings decoded, UTF-16 recognized, names stripped of their
/// slash, arrays reduced to their elements joined by `\u{1}`.
pub fn decode(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut parts = Vec::new();
        let mut rest = inner;
        while !rest.trim().is_empty() {
            let rest_trimmed = rest.trim_start();
            let skipped = rest.len() - rest_trimmed.len();
            let token = match rest_trimmed.as_bytes().first() {
                Some(b'(') => balanced(rest_trimmed, b'(', b')'),
                Some(b'<') => balanced(rest_trimmed, b'<', b'>'),
                _ => {
                    let end = rest_trimmed
                        .find(|c: char| c.is_whitespace() || c == '(' || c == '<')
                        .unwrap_or(rest_trimmed.len());
                    rest_trimmed[..end.max(1)].to_owned()
                }
            };
            rest = &rest[skipped + token.len()..];
            parts.push(decode(&token));
        }
        return parts.join("\u{1}");
    }
    if let Some(inner) = raw.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        return unescape(inner);
    }
    if let Some(inner) = raw.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        let digits: Vec<u8> = inner
            .bytes()
            .filter(|byte| byte.is_ascii_hexdigit())
            .collect();
        let bytes: Vec<u8> = digits
            .chunks(2)
            .map(|pair| {
                let high = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
                let low = pair
                    .get(1)
                    .and_then(|byte| (*byte as char).to_digit(16))
                    .unwrap_or(0) as u8;
                high * 16 + low
            })
            .collect();
        return decode_bytes(&bytes);
    }
    if let Some(name) = raw.strip_prefix('/') {
        return name.replace("#20", " ");
    }
    raw.to_owned()
}

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            out.push(bytes[at]);
            at += 1;
            continue;
        }
        at += 1;
        let Some(byte) = bytes.get(at) else { break };
        match byte {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'0'..=b'7' => {
                let mut value = 0u32;
                let mut digits = 0;
                while digits < 3 {
                    match bytes.get(at) {
                        Some(digit @ b'0'..=b'7') => {
                            value = value * 8 + (digit - b'0') as u32;
                            at += 1;
                            digits += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
                continue;
            }
            other => out.push(*other),
        }
        at += 1;
    }
    decode_bytes(&out)
}

/// PDF text is UTF-16BE when it starts with a byte-order mark, and otherwise
/// a single-byte encoding that agrees with Latin-1 over the range these tests
/// use.
fn decode_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|byte| *byte as char).collect(),
    }
}
