//! Strict text encoders for RAW text output.
//!
//! Unlike browsers, we never substitute unmappable characters: a `?` on a pharmacy label
//! or invoice is a silent data-loss bug. Encoding fails with `INVALID_PAYLOAD` instead.

use encoding_rs::Encoding;
use kiln_core::error::{PrintError, Result};

/// Code page 437 (original IBM PC), still the default on most dot-matrix and receipt
/// printers. Not part of the WHATWG encoding set, so it is implemented here.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Code page 850 (DOS Latin-1, Western Europe).
const CP850_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', 'ø', '£', 'Ø', '×', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '®', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', 'Á', 'Â', 'À', '©', '╣', '║', '╗', '╝', '¢', '¥', '┐', //
    '└', '┴', '┬', '├', '─', '┼', 'ã', 'Ã', '╚', '╔', '╩', '╦', '╠', '═', '╬', '¤', //
    'ð', 'Ð', 'Ê', 'Ë', 'È', 'ı', 'Í', 'Î', 'Ï', '┘', '┌', '█', '▄', '¦', 'Ì', '▀', //
    'Ó', 'ß', 'Ô', 'Ò', 'õ', 'Õ', 'µ', 'þ', 'Þ', 'Ú', 'Û', 'Ù', 'ý', 'Ý', '¯', '´', //
    '\u{ad}', '±', '‗', '¾', '¶', '§', '÷', '¸', '°', '¨', '·', '¹', '³', '²', '■', '\u{a0}',
];

/// Code page 858: 850 with the euro sign replacing dotless i (0xD5).
const CP858_HIGH: [char; 128] = {
    let mut table = CP850_HIGH;
    table[0xD5 - 0x80] = '€';
    table
};

/// The name the ESC/POS and ESC/P code-page selectors know an encoding by.
pub fn canonical_name(label: &str) -> Option<&'static str> {
    match TextEncoder::for_label(label).ok()? {
        TextEncoder::Utf8 => Some("utf-8"),
        TextEncoder::Dos(_, name) => Some(name),
        TextEncoder::Whatwg(enc) => Some(enc.name()),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TextEncoder {
    Utf8,
    /// A DOS code page not covered by WHATWG: upper-half table and display name.
    Dos(&'static [char; 128], &'static str),
    Whatwg(&'static Encoding),
}

impl TextEncoder {
    pub fn for_label(label: &str) -> Result<Self> {
        let normalised = label.trim().to_ascii_lowercase();
        match normalised.as_str() {
            "utf-8" | "utf8" => return Ok(Self::Utf8),
            "ibm437" | "ibm-437" | "cp437" | "437" | "dos-437" => {
                return Ok(Self::Dos(&CP437_HIGH, "IBM437"));
            }
            "ibm850" | "ibm-850" | "cp850" | "850" => return Ok(Self::Dos(&CP850_HIGH, "IBM850")),
            "ibm858" | "ibm-858" | "cp858" | "858" => return Ok(Self::Dos(&CP858_HIGH, "IBM858")),
            _ => {}
        }
        match Encoding::for_label(normalised.as_bytes()) {
            // UTF-16 labels encode to UTF-8 under WHATWG rules; refuse rather than
            // silently produce something other than what was asked for.
            Some(enc) if enc.output_encoding() == enc => Ok(Self::Whatwg(enc)),
            _ => Err(PrintError::invalid_payload(format!(
                "unsupported text encoding '{label}'"
            ))),
        }
    }

    pub fn encode(&self, text: &str) -> Result<Vec<u8>> {
        match self {
            Self::Utf8 => Ok(text.as_bytes().to_vec()),
            Self::Dos(table, name) => text
                .chars()
                .map(|c| {
                    if c.is_ascii() {
                        Ok(c as u8)
                    } else {
                        table
                            .iter()
                            .position(|&m| m == c)
                            .map(|i| 0x80 + i as u8)
                            .ok_or_else(|| unmappable(c, name))
                    }
                })
                .collect(),
            Self::Whatwg(enc) => {
                let (bytes, _, had_errors) = enc.encode(text);
                if had_errors {
                    let bad = text
                        .chars()
                        .find(|c| enc.encode(c.encode_utf8(&mut [0; 4])).2)
                        .unwrap_or('\u{fffd}');
                    return Err(unmappable(bad, enc.name()));
                }
                Ok(bytes.into_owned())
            }
        }
    }
}

fn unmappable(c: char, encoding: &str) -> PrintError {
    PrintError::invalid_payload(format!(
        "character U+{:04X} cannot be represented in encoding {encoding}",
        c as u32
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_round_trip_of_box_drawing() {
        let enc = TextEncoder::for_label("CP437").expect("known");
        assert_eq!(
            enc.encode("╔═╗ Ç½").expect("encodable"),
            vec![0xC9, 0xCD, 0xBB, 0x20, 0x80, 0xAB]
        );
    }

    #[test]
    fn cp850_and_cp858() {
        let e850 = TextEncoder::for_label("cp850").expect("known");
        assert_eq!(
            e850.encode("Ø£ã").expect("encodable"),
            vec![0x9D, 0x9C, 0xC6]
        );
        assert!(e850.encode("€").is_err());
        let e858 = TextEncoder::for_label("IBM858").expect("known");
        assert_eq!(e858.encode("€").expect("encodable"), vec![0xD5]);
        assert_eq!(canonical_name("858"), Some("IBM858"));
        assert_eq!(canonical_name("latin1"), Some("windows-1252"));
    }

    #[test]
    fn windows_1252_and_strictness() {
        let enc = TextEncoder::for_label("windows-1252").expect("known");
        assert_eq!(enc.encode("€5").expect("encodable"), vec![0x80, b'5']);
        let err = enc.encode("日本").expect_err("unmappable");
        assert!(err.message.contains("U+65E5"));
    }

    #[test]
    fn rejects_unknown_and_utf16() {
        assert!(TextEncoder::for_label("klingon").is_err());
        assert!(TextEncoder::for_label("utf-16le").is_err());
    }
}
