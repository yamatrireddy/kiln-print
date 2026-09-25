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

#[derive(Debug, Clone, Copy)]
pub enum TextEncoder {
    Utf8,
    Cp437,
    Whatwg(&'static Encoding),
}

impl TextEncoder {
    pub fn for_label(label: &str) -> Result<Self> {
        let normalised = label.trim().to_ascii_lowercase();
        match normalised.as_str() {
            "utf-8" | "utf8" => return Ok(Self::Utf8),
            "ibm437" | "ibm-437" | "cp437" | "437" | "dos-437" => return Ok(Self::Cp437),
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
            Self::Cp437 => text
                .chars()
                .map(|c| {
                    if c.is_ascii() {
                        Ok(c as u8)
                    } else {
                        CP437_HIGH
                            .iter()
                            .position(|&m| m == c)
                            .map(|i| 0x80 + i as u8)
                            .ok_or_else(|| unmappable(c, "IBM437"))
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
