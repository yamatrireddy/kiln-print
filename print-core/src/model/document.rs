//! Print requests and document payloads, after wire decoding.
//!
//! The wire layer (agent) decodes base64/hex/text into these types; the engine never sees
//! transport encodings. Raw bytes are held in [`Bytes`] so they are shared, never copied or
//! re-encoded, on their way to the provider.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::{Orientation, PrinterId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DocumentType {
    Raw,
    Text,
    Pdf,
    Html,
    Image,
}

#[derive(Debug, Clone)]
pub enum Document {
    Raw(RawDocument),
    Text(TextDocument),
}

impl Document {
    pub fn document_type(&self) -> DocumentType {
        match self {
            Self::Raw(_) => DocumentType::Raw,
            Self::Text(_) => DocumentType::Text,
        }
    }

    /// Size of the decoded payload, used for limits and queue accounting.
    pub fn size_bytes(&self) -> u64 {
        match self {
            Self::Raw(raw) => raw.data.len() as u64,
            Self::Text(text) => text.text.len() as u64,
        }
    }

    pub fn language(&self) -> Option<&str> {
        match self {
            Self::Raw(raw) => raw.language.as_deref(),
            Self::Text(_) => None,
        }
    }
}

/// Bytes delivered to the device exactly as received.
#[derive(Debug, Clone)]
pub struct RawDocument {
    pub data: Bytes,
    /// Printer command language hint (`ZPL`, `ESC/POS`, …). Informational only: it selects
    /// validation, never transformation.
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TextDocument {
    pub text: String,
    pub options: TextOptions,
}

/// How plain text reaches the printer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TextMode {
    /// Laid out and drawn by the OS graphics stack with the printer driver. Works with
    /// any driver-backed printer; honours font, size and alignment.
    #[default]
    Rendered,
    /// Encoded to bytes and sent as RAW. Use for dot-matrix, line and receipt printers,
    /// which print their resident font far faster and more crisply than graphics.
    Raw,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LineEnding {
    #[default]
    Crlf,
    Lf,
    Cr,
}

impl LineEnding {
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Crlf => b"\r\n",
            Self::Lf => b"\n",
            Self::Cr => b"\r",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginsMm {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Default for MarginsMm {
    fn default() -> Self {
        Self {
            top: 10.0,
            right: 10.0,
            bottom: 10.0,
            left: 10.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct TextOptions {
    pub mode: TextMode,
    /// RAW mode: target character encoding (`utf-8`, `windows-1252`, `ibm437`, …).
    pub encoding: String,
    /// RAW mode: line terminator written for every line break.
    pub line_ending: LineEnding,
    /// RAW mode: append a form feed so sheet printers eject and tractor-feed printers
    /// advance to the next top-of-form.
    pub form_feed: bool,
    /// RENDERED mode options.
    pub font_family: Option<String>,
    pub font_size: f32,
    pub bold: bool,
    pub alignment: TextAlignment,
    pub margins_mm: MarginsMm,
    pub orientation: Option<Orientation>,
    pub wrap: bool,
    /// Both modes: tab stops are expanded to spaces at this width (0 keeps tabs as-is in RAW mode).
    pub tab_width: u8,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            mode: TextMode::Rendered,
            encoding: "utf-8".into(),
            line_ending: LineEnding::Crlf,
            form_feed: true,
            font_family: None,
            font_size: 10.0,
            bold: false,
            alignment: TextAlignment::Left,
            margins_mm: MarginsMm::default(),
            orientation: None,
            wrap: true,
            tab_width: 8,
        }
    }
}

/// How a request names its target printer. There is deliberately no implicit "default
/// printer" selector: silently sending label commands to whatever is default would print
/// garbage pages, so clients must choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrinterSelector {
    Id(PrinterId),
    Name(String),
}

#[derive(Debug, Clone)]
pub struct PrintRequest {
    pub printer: PrinterSelector,
    pub document: Document,
    pub copies: u32,
    pub job_name: Option<String>,
    /// Client-chosen key making submission idempotent: resubmitting with the same key
    /// (e.g. after a reconnect) returns the original job instead of printing again.
    pub idempotency_key: Option<String>,
}
