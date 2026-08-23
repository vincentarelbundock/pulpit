#![forbid(unsafe_code)]

//! Decoding PDF *text strings* into Rust `String`s.
//!
//! A field's `/T`, a signature's `/M`, a `/FieldMDP` `/Fields` entry: all of
//! them are text strings in the sense of ISO 32000-1 §7.9.2.2, which is not
//! the same thing as bytes that happen to be UTF-8. A text string is either
//!
//! * UTF-16BE, introduced by a `FE FF` byte-order mark (and, since PDF 2.0,
//!   UTF-8 introduced by `EF BB BF`), or
//! * PDFDocEncoding, a single-byte encoding that agrees with ASCII below
//!   `0x80` and with Latin-1 over most — but not all — of the range above it.
//!
//! Acrobat writes UTF-16BE for any name that is not plain ASCII, so a form
//! whose fields are named in French arrives here as `FE FF 00 50 00 72 00 E9
//! …`. Reading those bytes as UTF-8 fails outright; reading them lossily
//! produces mojibake. Either way a name the user can see in Acrobat is a name
//! pulpit cannot match, which is why this decode is shared rather than
//! repeated: verification, pre-flight and the signing path must all agree on
//! what a field is called, and they must agree with the UTF-8 name the
//! application hands them.
//!
//! Decoding never fails. A truncated or ill-formed encoding yields
//! `U+FFFD REPLACEMENT CHARACTER`, because a name that cannot be decoded is
//! still a name that must be reported and compared, and a panic or a refusal
//! in the middle of reading a document's field list would be worse than an
//! unmatchable name. Nothing in this module decides whether signing is
//! permitted, so being lenient here does not weaken a fail-closed check.

/// Undo the literal-string escapes of §7.3.4.2, at the byte level.
///
/// `body` is the content between the outer parentheses of a `(…)` token, with
/// balanced inner parentheses still present. Escapes must be resolved before
/// the encoding is judged: `\376\377` is how a producer may write the UTF-16
/// byte-order mark inside a literal string, and a decoder that looked for
/// `FE FF` first would not see it.
pub fn unescape_literal(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        let byte = body[index];
        if byte != b'\\' {
            out.push(byte);
            index += 1;
            continue;
        }
        index += 1;
        let Some(&escaped) = body.get(index) else {
            // A trailing backslash: nothing follows to escape.
            break;
        };
        index += 1;
        match escaped {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            // A backslash before an end-of-line marker is a line
            // continuation: neither byte is part of the string.
            b'\n' => {}
            b'\r' => {
                if body.get(index) == Some(&b'\n') {
                    index += 1;
                }
            }
            b'0'..=b'7' => {
                let mut value = u32::from(escaped - b'0');
                for _ in 0..2 {
                    match body.get(index) {
                        Some(&digit @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(digit - b'0');
                            index += 1;
                        }
                        _ => break,
                    }
                }
                out.push((value & 0xff) as u8);
            }
            // `\\`, `\(`, `\)` and, per the spec, any other escaped byte
            // stands for itself.
            other => out.push(other),
        }
    }
    out
}

/// Decode the bytes of a text string — already unescaped, or already unhexed
/// — into UTF-8, per §7.9.2.2.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(utf16) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16be(utf16);
    }
    // PDF 2.0 §7.9.2.2.1 admits a UTF-8 text string introduced by the UTF-8
    // byte-order mark.
    if let Some(utf8) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(utf8).into_owned();
    }
    bytes.iter().map(|&byte| pdf_doc_encoding(byte)).collect()
}

/// Decode a whole string *token* as the tokenizer returns it: `(…)` including
/// its parentheses, or `<…>` including its angle brackets. Anything else is
/// not a string and returns `None`.
pub fn decode_text_string_token(token: &[u8]) -> Option<String> {
    if token.len() >= 2 && token[0] == b'(' && token[token.len() - 1] == b')' {
        let body = &token[1..token.len() - 1];
        return Some(decode_text_string(&unescape_literal(body)));
    }
    if token.len() >= 2 && token[0] == b'<' && token[token.len() - 1] == b'>' {
        let body = &token[1..token.len() - 1];
        // `<<` is a dictionary opener, not a string.
        if body.first() == Some(&b'<') {
            return None;
        }
        return Some(decode_text_string(&unhex(body)));
    }
    None
}

/// Bytes of a hex string. Whitespace is ignored and an odd final digit is
/// padded with `0`, both as §7.3.4.3 requires.
fn unhex(body: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = body.iter().filter_map(|&b| hex_value(b)).collect();
    digits
        .chunks(2)
        .map(|pair| pair[0] * 16 + pair.get(1).copied().unwrap_or(0))
        .collect()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// UTF-16BE, leniently: an unpaired surrogate or a trailing odd byte becomes
/// `U+FFFD` rather than an error.
fn decode_utf16be(bytes: &[u8]) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(u16::from_be_bytes);
    let mut out: String = char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    if bytes.len() % 2 == 1 {
        out.push(char::REPLACEMENT_CHARACTER);
    }
    out
}

/// One byte of PDFDocEncoding as a `char` (ISO 32000-1 Annex D.2).
///
/// Below `0x80` and from `0xA1` up it agrees with Latin-1, with three
/// exceptions that are why this is a table and not a cast: `0x18`–`0x1F` are
/// diacritics rather than control characters, `0xA0` is the euro sign rather
/// than a no-break space, and `0x7F`, `0x9F` and `0xAD` are undefined. The
/// undefined codes decode to `U+FFFD`: they carry no meaning to preserve, and
/// a name containing one cannot be matched against a name that does not.
fn pdf_doc_encoding(byte: u8) -> char {
    match byte {
        0x18 => '\u{02D8}', // BREVE
        0x19 => '\u{02C7}', // CARON
        0x1A => '\u{02C6}', // MODIFIER LETTER CIRCUMFLEX ACCENT
        0x1B => '\u{02D9}', // DOT ABOVE
        0x1C => '\u{02DD}', // DOUBLE ACUTE ACCENT
        0x1D => '\u{02DB}', // OGONEK
        0x1E => '\u{02DA}', // RING ABOVE
        0x1F => '\u{02DC}', // SMALL TILDE
        0x7F => char::REPLACEMENT_CHARACTER,
        0x80 => '\u{2022}',                  // BULLET
        0x81 => '\u{2020}',                  // DAGGER
        0x82 => '\u{2021}',                  // DOUBLE DAGGER
        0x83 => '\u{2026}',                  // HORIZONTAL ELLIPSIS
        0x84 => '\u{2014}',                  // EM DASH
        0x85 => '\u{2013}',                  // EN DASH
        0x86 => '\u{0192}',                  // LATIN SMALL LETTER F WITH HOOK
        0x87 => '\u{2044}',                  // FRACTION SLASH
        0x88 => '\u{2039}',                  // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
        0x89 => '\u{203A}',                  // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
        0x8A => '\u{2212}',                  // MINUS SIGN
        0x8B => '\u{2030}',                  // PER MILLE SIGN
        0x8C => '\u{201E}',                  // DOUBLE LOW-9 QUOTATION MARK
        0x8D => '\u{201C}',                  // LEFT DOUBLE QUOTATION MARK
        0x8E => '\u{201D}',                  // RIGHT DOUBLE QUOTATION MARK
        0x8F => '\u{2018}',                  // LEFT SINGLE QUOTATION MARK
        0x90 => '\u{2019}',                  // RIGHT SINGLE QUOTATION MARK
        0x91 => '\u{201A}',                  // SINGLE LOW-9 QUOTATION MARK
        0x92 => '\u{2122}',                  // TRADE MARK SIGN
        0x93 => '\u{FB01}',                  // LATIN SMALL LIGATURE FI
        0x94 => '\u{FB02}',                  // LATIN SMALL LIGATURE FL
        0x95 => '\u{0141}',                  // LATIN CAPITAL LETTER L WITH STROKE
        0x96 => '\u{0152}',                  // LATIN CAPITAL LIGATURE OE
        0x97 => '\u{0160}',                  // LATIN CAPITAL LETTER S WITH CARON
        0x98 => '\u{0178}',                  // LATIN CAPITAL LETTER Y WITH DIAERESIS
        0x99 => '\u{017D}',                  // LATIN CAPITAL LETTER Z WITH CARON
        0x9A => '\u{0131}',                  // LATIN SMALL LETTER DOTLESS I
        0x9B => '\u{0142}',                  // LATIN SMALL LETTER L WITH STROKE
        0x9C => '\u{0153}',                  // LATIN SMALL LIGATURE OE
        0x9D => '\u{0161}',                  // LATIN SMALL LETTER S WITH CARON
        0x9E => '\u{017E}',                  // LATIN SMALL LETTER Z WITH CARON
        0x9F => char::REPLACEMENT_CHARACTER, // undefined
        0xA0 => '\u{20AC}',                  // EURO SIGN
        0xAD => char::REPLACEMENT_CHARACTER, // undefined
        other => other as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_unchanged() {
        assert_eq!(decode_text_string(b"Signature1"), "Signature1");
        assert_eq!(
            decode_text_string_token(b"(Signature1)").as_deref(),
            Some("Signature1")
        );
    }

    /// The case the real Acrobat form is made of: a UTF-16BE `/T`, written as
    /// a literal string whose non-printable bytes are octal escapes.
    #[test]
    fn a_utf16be_literal_decodes_to_the_name_acrobat_shows() {
        // FE FF then "Président-rapporteur" in UTF-16BE.
        let mut body = vec![0xFEu8, 0xFF];
        for unit in "Président-rapporteur".encode_utf16() {
            body.extend_from_slice(&unit.to_be_bytes());
        }
        let mut token = vec![b'('];
        for byte in &body {
            match byte {
                b'(' | b')' | b'\\' => {
                    token.push(b'\\');
                    token.push(*byte);
                }
                0x20..=0x7E => token.push(*byte),
                other => token.extend_from_slice(format!("\\{other:03o}").as_bytes()),
            }
        }
        token.push(b')');

        assert_eq!(
            decode_text_string_token(&token).as_deref(),
            Some("Président-rapporteur")
        );
    }

    #[test]
    fn a_utf16be_hex_string_decodes_the_same_way() {
        let mut hex = String::from("<FEFF");
        for unit in "Président".encode_utf16() {
            hex.push_str(&format!("{unit:04X}"));
        }
        hex.push('>');
        assert_eq!(
            decode_text_string_token(hex.as_bytes()).as_deref(),
            Some("Président")
        );
    }

    #[test]
    fn pdfdocencoding_differs_from_latin1_where_the_table_says_so() {
        // é is one byte in PDFDocEncoding and agrees with Latin-1.
        assert_eq!(decode_text_string(b"Caf\xE9"), "Café");
        // 0xA0 is the euro sign, not a no-break space.
        assert_eq!(decode_text_string(b"\xA0"), "€");
        // 0x90 is a right single quotation mark, not a Latin-1 control byte.
        assert_eq!(decode_text_string(b"L\x90an"), "L’an");
        // Undefined codes decode to the replacement character.
        assert_eq!(decode_text_string(b"\x9F"), "\u{FFFD}");
        assert_eq!(decode_text_string(b"\xAD"), "\u{FFFD}");
    }

    #[test]
    fn literal_escapes_are_resolved_before_the_encoding_is_judged() {
        // The BOM written as octal escapes must still be recognised.
        assert_eq!(
            decode_text_string_token(b"(\\376\\377\\000A)").as_deref(),
            Some("A")
        );
        assert_eq!(
            decode_text_string_token(b"(a\\nb)").as_deref(),
            Some("a\nb")
        );
        assert_eq!(
            decode_text_string_token(b"(a\\(b\\))").as_deref(),
            Some("a(b)")
        );
        // A backslash before an end-of-line marker joins the two lines.
        assert_eq!(decode_text_string_token(b"(a\\\nb)").as_deref(), Some("ab"));
        // Balanced inner parentheses need no escape.
        assert_eq!(
            decode_text_string_token(b"(a(b)c)").as_deref(),
            Some("a(b)c")
        );
    }

    /// Leniency: an ill-formed UTF-16BE string must produce a name, never a
    /// panic, because the caller still has to report and compare it.
    #[test]
    fn ill_formed_utf16be_yields_replacement_characters() {
        // A lone high surrogate.
        assert_eq!(decode_text_string(b"\xFE\xFF\xD8\x3D"), "\u{FFFD}");
        // A trailing odd byte.
        assert_eq!(decode_text_string(b"\xFE\xFF\x00A\x00"), "A\u{FFFD}");
        // A surrogate pair is one character, not two replacements.
        assert_eq!(decode_text_string(b"\xFE\xFF\xD8\x3D\xDE\x00"), "\u{1F600}");
    }

    #[test]
    fn a_dictionary_opener_is_not_a_string() {
        assert_eq!(decode_text_string_token(b"<<"), None);
        assert_eq!(decode_text_string_token(b"/Name"), None);
        assert_eq!(decode_text_string_token(b"12"), None);
    }
}
