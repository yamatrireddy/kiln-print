//! Encoders from the language-neutral [`LabelDocument`] to label printer languages.
//!
//! Each encoder maps positions from millimetres to dots at the label's `dpi`, picks the
//! closest resident font for a requested text height, and escapes field data so client
//! text can never inject commands. Elements a language cannot express are reported as
//! `UNSUPPORTED_OPERATION` rather than silently dropped.

pub mod cpcl;
pub mod epl;
pub mod tspl;
pub mod zpl;

use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{LabelDocument, Symbology};

pub(crate) fn unsupported(language: &str, what: &str) -> PrintError {
    PrintError::new(
        ErrorCode::UnsupportedOperation,
        format!("{what} is not supported by {language} labels"),
    )
}

/// Positions and sizes in dots for one label.
pub(crate) struct Dots<'a>(pub &'a LabelDocument);

impl Dots<'_> {
    pub fn of(&self, mm: f32) -> u32 {
        self.0.dots(mm)
    }

    pub fn at_least_one(&self, mm: f32) -> u32 {
        self.0.dots(mm).max(1)
    }
}

/// Rotation index 0..=3 for 0, 90, 180, 270 degrees.
pub(crate) fn quarter_turns(rotation: u16) -> u8 {
    ((rotation / 90) % 4) as u8
}

/// Chooses the bitmap font and integer magnification whose height is closest to
/// `target` dots. `fonts` lists (font id, height in dots at 203 dpi).
pub(crate) fn pick_font(
    fonts: &[(&'static str, u32)],
    target: u32,
    dpi: u32,
    max_mult: u32,
) -> (&'static str, u32) {
    let scale = |h: u32| (u64::from(h) * u64::from(dpi) / 203).max(1) as u32;
    let mut best = (fonts[0].0, 1, u32::MAX);
    for &(font, h) in fonts {
        let h = scale(h);
        let mult = ((target + h / 2) / h).clamp(1, max_mult);
        let error = (h * mult).abs_diff(target);
        // Ties go to the larger font (listed later): less magnification prints cleaner.
        if error <= best.2 {
            best = (font, mult, error);
        }
    }
    (best.0, best.1)
}

/// Barcode payload as the printer expects it: EAN/UPC without the check digit, which
/// printers compute themselves (a wrong supplied check digit would be printed as-is by
/// some firmware).
pub(crate) fn barcode_payload(symbology: Symbology, data: &str) -> &str {
    let keep = match symbology {
        Symbology::Ean13 => 12,
        Symbology::Ean8 => 7,
        Symbology::UpcA => 11,
        _ => data.len(),
    };
    &data[..keep.min(data.len())]
}

/// Removes characters that would break a line-oriented command language.
pub(crate) fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(crate) fn encode_text(text: &str, encoding: &str) -> Result<Vec<u8>> {
    crate::encoding::TextEncoder::for_label(encoding)?.encode(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_choice() {
        let fonts = [("1", 12), ("2", 16), ("5", 48)];
        assert_eq!(pick_font(&fonts, 24, 203, 9), ("1", 2));
        assert_eq!(pick_font(&fonts, 48, 203, 9), ("5", 1));
        assert_eq!(pick_font(&fonts, 16, 203, 9), ("2", 1));
        // At 406 dpi every font is twice as tall in dots.
        assert_eq!(pick_font(&fonts, 32, 406, 9), ("2", 1));
    }

    #[test]
    fn check_digits_are_left_to_the_printer() {
        assert_eq!(
            barcode_payload(Symbology::Ean13, "4006381333931"),
            "400638133393"
        );
        assert_eq!(barcode_payload(Symbology::Code128, "ABC"), "ABC");
    }
}
