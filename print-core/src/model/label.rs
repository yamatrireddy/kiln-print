//! Language-neutral label, receipt and dot-matrix documents.
//!
//! Clients describe *what* to print once; the renderer encodes it for the target printer's
//! command language (ZPL, EPL, TSPL, CPCL, ESC/POS, ESC/P). Output is always RAW bytes for
//! the device: nothing here is rasterised or converted to PDF.

use serde::{Deserialize, Serialize};

use super::TextAlignment;
use crate::error::{PrintError, Result};

// ------------------------------------------------------------------ shared

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Symbology {
    Code128,
    Code39,
    Ean13,
    Ean8,
    UpcA,
    /// Interleaved 2 of 5.
    Itf,
}

impl Symbology {
    /// Validates barcode content for the symbology (lengths and character sets).
    pub fn validate(self, data: &str) -> Result<()> {
        let digits = data.chars().all(|c| c.is_ascii_digit());
        let ok = match self {
            Self::Code128 => {
                !data.is_empty()
                    && data.len() <= 80
                    && data.chars().all(|c| (' '..='~').contains(&c))
            }
            Self::Code39 => {
                !data.is_empty()
                    && data.len() <= 80
                    && data.chars().all(|c| {
                        c.is_ascii_uppercase() || c.is_ascii_digit() || " -.$/+%".contains(c)
                    })
            }
            Self::Ean13 => digits && matches!(data.len(), 12 | 13),
            Self::Ean8 => digits && matches!(data.len(), 7 | 8),
            Self::UpcA => digits && matches!(data.len(), 11 | 12),
            Self::Itf => digits && !data.is_empty() && data.len() % 2 == 0 && data.len() <= 80,
        };
        if ok {
            Ok(())
        } else {
            Err(PrintError::invalid_payload(format!(
                "'{data}' is not valid {self:?} barcode data"
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QrErrorCorrection {
    L,
    #[default]
    M,
    Q,
    H,
}

fn validate_rotation(rotation: u16) -> Result<()> {
    if matches!(rotation, 0 | 90 | 180 | 270) {
        Ok(())
    } else {
        Err(PrintError::invalid_payload(
            "rotation must be 0, 90, 180 or 270",
        ))
    }
}

fn check(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(PrintError::invalid_payload(message))
    }
}

// ------------------------------------------------------------------ labels

fn default_dpi() -> u32 {
    203
}
fn default_text_height() -> f32 {
    3.0
}
fn default_barcode_height() -> f32 {
    10.0
}
fn default_module() -> u8 {
    2
}
fn default_true() -> bool {
    true
}
fn default_magnification() -> u8 {
    4
}
fn default_thickness() -> f32 {
    0.3
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "SCREAMING_SNAKE_CASE",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LabelElement {
    Text {
        x_mm: f32,
        y_mm: f32,
        text: String,
        /// Character height.
        #[serde(default = "default_text_height")]
        height_mm: f32,
        #[serde(default)]
        rotation: u16,
        /// Language-specific font id, passed through (e.g. ZPL `A`..`Z`, `0`).
        #[serde(default)]
        font: Option<String>,
    },
    Barcode {
        x_mm: f32,
        y_mm: f32,
        symbology: Symbology,
        data: String,
        #[serde(default = "default_barcode_height")]
        height_mm: f32,
        /// Narrow bar width in dots.
        #[serde(default = "default_module")]
        module_width: u8,
        /// Print the human-readable text.
        #[serde(default = "default_true")]
        human_readable: bool,
        #[serde(default)]
        rotation: u16,
    },
    Qr {
        x_mm: f32,
        y_mm: f32,
        data: String,
        #[serde(default = "default_magnification")]
        magnification: u8,
        #[serde(default)]
        error_correction: QrErrorCorrection,
    },
    DataMatrix {
        x_mm: f32,
        y_mm: f32,
        data: String,
        /// Module size in dots.
        #[serde(default = "default_magnification")]
        module_size: u8,
    },
    Box {
        x_mm: f32,
        y_mm: f32,
        width_mm: f32,
        height_mm: f32,
        #[serde(default = "default_thickness")]
        thickness_mm: f32,
    },
    /// Commands in the target language, inserted verbatim (escape hatch).
    Raw { data: String },
}

impl LabelElement {
    fn position(&self) -> Option<(f32, f32)> {
        match self {
            Self::Text { x_mm, y_mm, .. }
            | Self::Barcode { x_mm, y_mm, .. }
            | Self::Qr { x_mm, y_mm, .. }
            | Self::DataMatrix { x_mm, y_mm, .. }
            | Self::Box { x_mm, y_mm, .. } => Some((*x_mm, *y_mm)),
            Self::Raw { .. } => None,
        }
    }
}

/// A label described independently of the printer language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabelDocument {
    pub width_mm: f32,
    pub height_mm: f32,
    /// Printer resolution: 203, 300 or 600 typically.
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    /// `ZPL`, `EPL`, `TSPL` or `CPCL`. When absent the printer's language hint is used.
    #[serde(default)]
    pub language: Option<String>,
    /// Gap between labels (EPL/TSPL media setup).
    #[serde(default)]
    pub gap_mm: Option<f32>,
    /// Print darkness, 0-30 (mapped to each language's scale).
    #[serde(default)]
    pub darkness: Option<u8>,
    /// Print speed in inches per second.
    #[serde(default)]
    pub speed: Option<u8>,
    pub elements: Vec<LabelElement>,
}

impl LabelDocument {
    pub const MAX_ELEMENTS: usize = 500;

    /// Millimetres to printer dots.
    pub fn dots(&self, mm: f32) -> u32 {
        (f64::from(mm.max(0.0)) / 25.4 * f64::from(self.dpi)).round() as u32
    }

    pub fn validate(&self) -> Result<()> {
        check(
            (5.0..=600.0).contains(&self.width_mm),
            "label widthMm must be between 5 and 600",
        )?;
        check(
            (5.0..=2000.0).contains(&self.height_mm),
            "label heightMm must be between 5 and 2000",
        )?;
        check(
            (100..=1200).contains(&self.dpi),
            "dpi must be between 100 and 1200",
        )?;
        check(
            self.darkness.is_none_or(|d| d <= 30),
            "darkness must be between 0 and 30",
        )?;
        check(
            self.speed.is_none_or(|s| (1..=14).contains(&s)),
            "speed must be between 1 and 14",
        )?;
        check(
            self.gap_mm.is_none_or(|g| (0.0..=50.0).contains(&g)),
            "gapMm must be between 0 and 50",
        )?;
        check(
            !self.elements.is_empty(),
            "a label needs at least one element",
        )?;
        check(
            self.elements.len() <= Self::MAX_ELEMENTS,
            "a label may have at most 500 elements",
        )?;
        for element in &self.elements {
            if let Some((x, y)) = element.position() {
                check(
                    (0.0..self.width_mm).contains(&x) && (0.0..self.height_mm).contains(&y),
                    "element position is outside the label",
                )?;
            }
            match element {
                LabelElement::Text {
                    text,
                    height_mm,
                    rotation,
                    font,
                    ..
                } => {
                    check(
                        !text.is_empty() && text.len() <= 1000,
                        "text must be 1-1000 characters",
                    )?;
                    check(
                        (0.5..=200.0).contains(height_mm),
                        "text heightMm must be between 0.5 and 200",
                    )?;
                    validate_rotation(*rotation)?;
                    check(
                        font.as_ref().is_none_or(|f| {
                            !f.is_empty()
                                && f.len() <= 32
                                && f.chars().all(|c| c.is_ascii_graphic())
                        }),
                        "font must be 1-32 printable ASCII characters",
                    )?;
                }
                LabelElement::Barcode {
                    symbology,
                    data,
                    height_mm,
                    module_width,
                    rotation,
                    ..
                } => {
                    symbology.validate(data)?;
                    check(
                        (1.0..=300.0).contains(height_mm),
                        "barcode heightMm must be between 1 and 300",
                    )?;
                    check(
                        (1..=10).contains(module_width),
                        "moduleWidth must be between 1 and 10 dots",
                    )?;
                    validate_rotation(*rotation)?;
                }
                LabelElement::Qr {
                    data,
                    magnification,
                    ..
                } => {
                    check(
                        !data.is_empty() && data.len() <= 2000,
                        "QR data must be 1-2000 bytes",
                    )?;
                    check(
                        (1..=10).contains(magnification),
                        "magnification must be between 1 and 10",
                    )?;
                }
                LabelElement::DataMatrix {
                    data, module_size, ..
                } => {
                    check(
                        !data.is_empty() && data.len() <= 1500,
                        "Data Matrix data must be 1-1500 bytes",
                    )?;
                    check(
                        (1..=20).contains(module_size),
                        "moduleSize must be between 1 and 20",
                    )?;
                }
                LabelElement::Box {
                    width_mm,
                    height_mm,
                    thickness_mm,
                    ..
                } => {
                    check(
                        *width_mm > 0.0 && *height_mm > 0.0,
                        "box dimensions must be positive",
                    )?;
                    check(*thickness_mm > 0.0, "thicknessMm must be positive")?;
                }
                LabelElement::Raw { data } => {
                    check(data.len() <= 64 * 1024, "RAW element is limited to 64 KiB")?
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ receipts

fn default_width_chars() -> u8 {
    48
}
fn default_code_page() -> String {
    "ibm437".into()
}
fn default_one() -> u8 {
    1
}
fn default_barcode_dots() -> u16 {
    80
}
fn default_receipt_module() -> u8 {
    3
}
fn default_qr_size() -> u8 {
    6
}
fn default_cut_feed() -> u8 {
    3
}
fn default_separator() -> char {
    '-'
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "SCREAMING_SNAKE_CASE",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReceiptItem {
    Text {
        text: String,
        #[serde(default)]
        align: TextAlignment,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        underline: bool,
        #[serde(default)]
        double_width: bool,
        #[serde(default)]
        double_height: bool,
        /// White on black.
        #[serde(default)]
        invert: bool,
        /// Font B (smaller).
        #[serde(default)]
        small: bool,
    },
    /// Left and right text on one line, padded to `widthChars` (e.g. item and price).
    Columns {
        left: String,
        right: String,
        #[serde(default)]
        bold: bool,
    },
    Separator {
        #[serde(default = "default_separator")]
        character: char,
    },
    Feed {
        #[serde(default = "default_one")]
        lines: u8,
    },
    Barcode {
        symbology: Symbology,
        data: String,
        #[serde(default = "default_barcode_dots")]
        height_dots: u16,
        #[serde(default = "default_receipt_module")]
        module_width: u8,
        #[serde(default = "default_true")]
        human_readable: bool,
        #[serde(default)]
        align: TextAlignment,
    },
    Qr {
        data: String,
        #[serde(default = "default_qr_size")]
        size: u8,
        #[serde(default)]
        error_correction: QrErrorCorrection,
        #[serde(default)]
        align: TextAlignment,
    },
    /// A logo or picture (base64 PNG/JPEG/BMP), dithered to black and white.
    Image {
        data: String,
        #[serde(default)]
        align: TextAlignment,
        /// Defaults to the paper width in dots (576 for 80 mm, 384 for 58 mm).
        #[serde(default)]
        max_width_dots: Option<u16>,
    },
    Cut {
        #[serde(default)]
        partial: bool,
        #[serde(default = "default_cut_feed")]
        feed_lines: u8,
    },
    /// Pulse the cash-drawer kick connector (pin 2 or 5).
    Drawer {
        #[serde(default)]
        pin: u8,
    },
    /// Base64 ESC/POS bytes inserted verbatim.
    Raw { data: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptDocument {
    /// Characters per line in the normal font: 48 for 80 mm paper, 32 for 58 mm.
    #[serde(default = "default_width_chars")]
    pub width_chars: u8,
    /// Printer code page: `ibm437`, `ibm850`, `ibm858`, `windows-1252`, `ibm866`, …
    #[serde(default = "default_code_page")]
    pub code_page: String,
    /// Cut the paper at the end (unless the items already end with `CUT`).
    #[serde(default = "default_true")]
    pub cut: bool,
    #[serde(default)]
    pub open_drawer: bool,
    pub items: Vec<ReceiptItem>,
}

impl ReceiptDocument {
    pub fn validate(&self) -> Result<()> {
        check(
            (16..=96).contains(&self.width_chars),
            "widthChars must be between 16 and 96",
        )?;
        check(
            !self.items.is_empty() && self.items.len() <= 2000,
            "a receipt needs 1-2000 items",
        )?;
        for item in &self.items {
            match item {
                ReceiptItem::Text { text, .. } => {
                    check(text.len() <= 10_000, "text item is too long")?
                }
                ReceiptItem::Columns { left, right, .. } => {
                    check(left.len() + right.len() <= 1000, "columns text is too long")?;
                }
                ReceiptItem::Feed { lines } => {
                    check(*lines <= 100, "feed lines must be at most 100")?
                }
                ReceiptItem::Barcode {
                    symbology,
                    data,
                    height_dots,
                    module_width,
                    ..
                } => {
                    symbology.validate(data)?;
                    check(
                        (1..=255).contains(height_dots),
                        "heightDots must be between 1 and 255",
                    )?;
                    check(
                        (2..=6).contains(module_width),
                        "moduleWidth must be between 2 and 6",
                    )?;
                }
                ReceiptItem::Qr { data, size, .. } => {
                    check(
                        !data.is_empty() && data.len() <= 2000,
                        "QR data must be 1-2000 bytes",
                    )?;
                    check((1..=16).contains(size), "QR size must be between 1 and 16")?;
                }
                ReceiptItem::Image {
                    data,
                    max_width_dots,
                    ..
                } => {
                    check(!data.is_empty(), "image data is empty")?;
                    check(
                        max_width_dots.is_none_or(|w| (8..=2048).contains(&w)),
                        "maxWidthDots must be 8-2048",
                    )?;
                }
                ReceiptItem::Drawer { pin } => {
                    check(*pin <= 1, "drawer pin must be 0 (pin 2) or 1 (pin 5)")?
                }
                ReceiptItem::Separator { .. }
                | ReceiptItem::Cut { .. }
                | ReceiptItem::Raw { .. } => {}
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ dot matrix

fn default_cpi() -> u8 {
    10
}
fn default_lpi() -> f32 {
    6.0
}
fn default_pins() -> u8 {
    24
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DotMatrixQuality {
    #[default]
    Draft,
    /// Near letter quality.
    Nlq,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct DotMatrixStyle {
    pub bold: bool,
    pub condensed: bool,
    pub double_width: bool,
    pub underline: bool,
    pub italic: bool,
    pub double_strike: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "SCREAMING_SNAKE_CASE",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DotMatrixItem {
    Line {
        text: String,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        condensed: bool,
        #[serde(default)]
        double_width: bool,
        #[serde(default)]
        underline: bool,
        #[serde(default)]
        italic: bool,
        #[serde(default)]
        double_strike: bool,
    },
    LineFeed {
        #[serde(default = "default_one")]
        lines: u8,
    },
    /// Advance to the next top-of-form (page break on tractor paper).
    FormFeed,
    /// Base64 bytes sent verbatim (printer-specific escape sequences).
    Raw { data: String },
}

impl DotMatrixItem {
    /// Style of a `LINE` item (default style for other items).
    pub fn style(&self) -> DotMatrixStyle {
        match *self {
            Self::Line {
                bold,
                condensed,
                double_width,
                underline,
                italic,
                double_strike,
                ..
            } => DotMatrixStyle {
                bold,
                condensed,
                double_width,
                underline,
                italic,
                double_strike,
            },
            _ => DotMatrixStyle::default(),
        }
    }
}

/// A line: plain text, or a styled/command item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DotMatrixLine {
    Text(String),
    Item(DotMatrixItem),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DotMatrixDocument {
    /// Characters per inch: 10 (pica), 12 (elite), 15, 17 (condensed pica) or 20
    /// (condensed elite).
    #[serde(default = "default_cpi")]
    pub cpi: u8,
    /// Lines per inch: 6 and 8 use the standard commands; other values use n/180" (24-pin)
    /// or n/216" (9-pin) spacing.
    #[serde(default = "default_lpi")]
    pub lpi: f32,
    /// Print-head pins (9 or 24); selects the line-spacing unit.
    #[serde(default = "default_pins")]
    pub pins: u8,
    #[serde(default)]
    pub quality: DotMatrixQuality,
    /// Page length in lines (tractor forms). Mutually exclusive with inches.
    #[serde(default)]
    pub form_length_lines: Option<u8>,
    #[serde(default)]
    pub form_length_inches: Option<u8>,
    /// Lines to skip over the perforation on continuous paper.
    #[serde(default)]
    pub skip_perforation_lines: Option<u8>,
    #[serde(default)]
    pub left_margin: Option<u8>,
    #[serde(default)]
    pub right_margin: Option<u8>,
    /// Character encoding for text (`ibm437`, `ibm850`, `windows-1252`, …).
    #[serde(default = "default_code_page")]
    pub encoding: String,
    /// ESC/P character table (`ESC t n`); defaults to the table matching `encoding`.
    #[serde(default)]
    pub character_table: Option<u8>,
    /// Send `ESC @` first (reset to power-on settings).
    #[serde(default = "default_true")]
    pub initialize: bool,
    /// Finish with a form feed (next top-of-form).
    #[serde(default = "default_true")]
    pub form_feed: bool,
    pub lines: Vec<DotMatrixLine>,
}

impl DotMatrixDocument {
    pub fn validate(&self) -> Result<()> {
        check(
            matches!(self.cpi, 10 | 12 | 15 | 17 | 20),
            "cpi must be 10, 12, 15, 17 or 20",
        )?;
        check(
            (1.0..=72.0).contains(&self.lpi),
            "lpi must be between 1 and 72",
        )?;
        check(matches!(self.pins, 9 | 24), "pins must be 9 or 24")?;
        check(
            !(self.form_length_lines.is_some() && self.form_length_inches.is_some()),
            "give formLengthLines or formLengthInches, not both",
        )?;
        check(
            self.form_length_lines.is_none_or(|n| n >= 1),
            "formLengthLines must be at least 1",
        )?;
        check(
            self.form_length_inches
                .is_none_or(|n| (1..=22).contains(&n)),
            "formLengthInches must be 1-22",
        )?;
        check(
            !self.lines.is_empty() && self.lines.len() <= 20_000,
            "a document needs 1-20000 lines",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barcode_data_validation() {
        assert!(Symbology::Ean13.validate("400638133393").is_ok());
        assert!(Symbology::Ean13.validate("40063813339X").is_err());
        assert!(Symbology::Code39.validate("ABC-123").is_ok());
        assert!(Symbology::Code39.validate("abc").is_err());
        assert!(Symbology::Itf.validate("123").is_err());
        assert!(Symbology::Code128.validate("Hello, World!").is_ok());
    }

    #[test]
    fn label_json_round_trip_and_validation() {
        let label: LabelDocument = serde_json::from_str(
            r#"{"widthMm": 100, "heightMm": 50, "elements": [
                {"type": "TEXT", "xMm": 5, "yMm": 5, "text": "Hello"},
                {"type": "BARCODE", "xMm": 5, "yMm": 15, "symbology": "CODE128", "data": "ABC123"},
                {"type": "QR", "xMm": 70, "yMm": 5, "data": "https://example.com", "errorCorrection": "H"},
                {"type": "BOX", "xMm": 1, "yMm": 1, "widthMm": 98, "heightMm": 48}
            ]}"#,
        )
        .expect("parse");
        assert_eq!(label.dpi, 203);
        label.validate().expect("valid");
        assert_eq!(label.dots(25.4), 203);

        let outside = LabelDocument {
            elements: vec![
                LabelElement::Raw {
                    data: String::new(),
                },
                LabelElement::Text {
                    x_mm: 150.0,
                    y_mm: 1.0,
                    text: "x".into(),
                    height_mm: 3.0,
                    rotation: 0,
                    font: None,
                },
            ],
            ..label
        };
        assert!(outside.validate().is_err());
        assert!(
            serde_json::from_str::<LabelElement>(
                r#"{"type": "TEXT", "xMm": 1, "yMm": 1, "text": "a", "colour": 1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn dot_matrix_lines_accept_strings_or_items() {
        let doc: DotMatrixDocument = serde_json::from_str(
            r#"{"cpi": 12, "lines": ["plain", {"type": "LINE", "text": "bold", "bold": true}, {"type": "FORM_FEED"}]}"#,
        )
        .expect("parse");
        doc.validate().expect("valid");
        assert!(matches!(&doc.lines[1], DotMatrixLine::Item(item) if item.style().bold));
        let bad = DotMatrixDocument { cpi: 11, ..doc };
        assert!(bad.validate().is_err());
    }
}
