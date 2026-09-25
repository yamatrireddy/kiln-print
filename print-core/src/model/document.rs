//! Print requests and document payloads, after wire decoding.
//!
//! The wire layer (agent) decodes base64/hex/text into these types; the engine never sees
//! transport encodings. Raw bytes are held in [`Bytes`] so they are shared, never copied or
//! re-encoded, on their way to the provider.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::{Align, ColorMode, Duplex, Orientation, PageSetup, PaperRequest, Placement, PrinterId};
use crate::error::{PrintError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DocumentType {
    Raw,
    Text,
    Pdf,
    Html,
    Image,
}

impl DocumentType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "RAW",
            Self::Text => "TEXT",
            Self::Pdf => "PDF",
            Self::Html => "HTML",
            Self::Image => "IMAGE",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Document {
    Raw(RawDocument),
    Text(TextDocument),
    Pdf(PdfDocument),
    Html(HtmlDocument),
    Image(ImageDocument),
}

impl Document {
    pub fn document_type(&self) -> DocumentType {
        match self {
            Self::Raw(_) => DocumentType::Raw,
            Self::Text(_) => DocumentType::Text,
            Self::Pdf(_) => DocumentType::Pdf,
            Self::Html(_) => DocumentType::Html,
            Self::Image(_) => DocumentType::Image,
        }
    }

    /// Size of the decoded payload, used for limits and queue accounting.
    pub fn size_bytes(&self) -> u64 {
        match self {
            Self::Raw(raw) => raw.data.len() as u64,
            Self::Text(text) => text.text.len() as u64,
            Self::Pdf(pdf) => pdf.data.len() as u64,
            Self::Html(html) => html.html.len() as u64,
            Self::Image(image) => image.data.len() as u64,
        }
    }

    pub fn language(&self) -> Option<&str> {
        match self {
            Self::Raw(raw) => raw.language.as_deref(),
            _ => None,
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

#[derive(Debug, Clone)]
pub struct PdfDocument {
    pub data: Bytes,
    pub options: PdfOptions,
}

#[derive(Debug, Clone)]
pub struct HtmlDocument {
    pub html: String,
    pub options: HtmlOptions,
}

#[derive(Debug, Clone)]
pub struct ImageDocument {
    pub data: Bytes,
    pub options: ImageOptions,
}

/// Sizing keyword for PDF pages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScaleMode {
    Fit,
    #[default]
    ShrinkToFit,
    ActualSize,
}

/// `"FIT" | "SHRINK_TO_FIT" | "ACTUAL_SIZE"` or a percentage of the actual size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scale {
    Mode(ScaleMode),
    Percent(f32),
}

impl Default for Scale {
    fn default() -> Self {
        Self::Mode(ScaleMode::ShrinkToFit)
    }
}

impl Scale {
    pub fn placement(self) -> Placement {
        match self {
            Self::Mode(ScaleMode::Fit) => Placement::Fit,
            Self::Mode(ScaleMode::ShrinkToFit) => Placement::ShrinkToFit,
            Self::Mode(ScaleMode::ActualSize) => Placement::ActualSize,
            Self::Percent(p) => Placement::Percent(p),
        }
    }

    fn validate(self) -> Result<()> {
        match self {
            Self::Percent(p) if !(1.0..=1000.0).contains(&p) => Err(PrintError::invalid_payload(
                "scale percentage must be between 1 and 1000",
            )),
            _ => Ok(()),
        }
    }
}

/// Adds the page-setup fields shared by every graphical document type.
macro_rules! with_page_setup {
    ($(#[$meta:meta])* pub struct $name:ident { $($(#[$fmeta:meta])* pub $field:ident : $ty:ty,)* }) => {
        $(#[$meta])*
        pub struct $name {
            $($(#[$fmeta])* pub $field: $ty,)*
            pub paper_size: Option<PaperRequest>,
            pub orientation: Option<Orientation>,
            pub margins_mm: Option<MarginsMm>,
            pub duplex: Option<Duplex>,
            pub color: Option<ColorMode>,
            pub tray: Option<String>,
        }

        impl $name {
            pub fn page_setup(&self) -> PageSetup {
                PageSetup {
                    paper_size: self.paper_size.clone(),
                    orientation: self.orientation,
                    margins_mm: self.margins_mm,
                    duplex: self.duplex,
                    color: self.color,
                    tray: self.tray.clone(),
                }
            }
        }
    };
}

with_page_setup! {
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", default, deny_unknown_fields)]
    pub struct PdfOptions {
        /// `"1-3,5,8-"`; all pages when absent.
        pub page_range: Option<String>,
        pub scale: Scale,
        /// Upper bound for the rasterisation resolution on platforms that rasterise
        /// (Windows). Defaults to the printer resolution capped at 300 dpi.
        pub dpi: Option<u32>,
    }
}

impl PdfOptions {
    pub fn validate(&self) -> Result<()> {
        if let Some(range) = &self.page_range {
            super::PageRanges::parse(range)?;
        }
        self.scale.validate()?;
        validate_dpi(self.dpi)?;
        self.page_setup().validate()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImageFit {
    /// Print at the image's physical size (pixels / dpi).
    Original,
    #[default]
    Fit,
    ShrinkToFit,
    Fill,
}

with_page_setup! {
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", default, deny_unknown_fields)]
    pub struct ImageOptions {
        pub fit: ImageFit,
        /// Percentage of the original size; overrides `fit`.
        pub scale: Option<f32>,
        /// Clockwise rotation in degrees: 0, 90, 180 or 270.
        pub rotate: u16,
        /// Image resolution used for `ORIGINAL` and `scale` sizing. Defaults to the file's
        /// metadata, else 96 dpi.
        pub dpi: Option<f32>,
        pub align: Align,
    }
}

impl ImageOptions {
    pub fn placement(&self) -> Placement {
        match (self.scale, self.fit) {
            (Some(p), _) => Placement::Percent(p),
            (None, ImageFit::Original) => Placement::ActualSize,
            (None, ImageFit::Fit) => Placement::Fit,
            (None, ImageFit::ShrinkToFit) => Placement::ShrinkToFit,
            (None, ImageFit::Fill) => Placement::Fill,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !matches!(self.rotate, 0 | 90 | 180 | 270) {
            return Err(PrintError::invalid_payload(
                "rotate must be 0, 90, 180 or 270",
            ));
        }
        if let Some(scale) = self.scale {
            Scale::Percent(scale).validate()?;
        }
        if self.dpi.is_some_and(|d| !(10.0..=4800.0).contains(&d)) {
            return Err(PrintError::invalid_payload(
                "dpi must be between 10 and 4800",
            ));
        }
        self.page_setup().validate()
    }
}

with_page_setup! {
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", default, deny_unknown_fields)]
    pub struct HtmlOptions {
        pub page_range: Option<String>,
        /// Layout zoom in percent (10-200).
        pub scale: f32,
        pub print_background: bool,
        /// Honour CSS `@page { size: ... }` over `paperSize`.
        pub prefer_css_page_size: bool,
        /// Header/footer templates (HTML). Elements with the classes `pageNumber`,
        /// `totalPages`, `date` and `title` are filled in.
        pub header_html: Option<String>,
        pub footer_html: Option<String>,
        pub dpi: Option<u32>,
    }
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            page_range: None,
            scale: 100.0,
            print_background: true,
            prefer_css_page_size: true,
            header_html: None,
            footer_html: None,
            dpi: None,
            paper_size: None,
            orientation: None,
            margins_mm: None,
            duplex: None,
            color: None,
            tray: None,
        }
    }
}

impl HtmlOptions {
    pub fn validate(&self) -> Result<()> {
        if let Some(range) = &self.page_range {
            super::PageRanges::parse(range)?;
        }
        if !(10.0..=200.0).contains(&self.scale) {
            return Err(PrintError::invalid_payload(
                "scale must be between 10 and 200 percent",
            ));
        }
        for template in [&self.header_html, &self.footer_html].into_iter().flatten() {
            if template.len() > 64 * 1024 {
                return Err(PrintError::invalid_payload(
                    "header and footer templates are limited to 64 KiB",
                ));
            }
        }
        validate_dpi(self.dpi)?;
        self.page_setup().validate()
    }
}

fn validate_dpi(dpi: Option<u32>) -> Result<()> {
    if dpi.is_some_and(|d| !(72..=1200).contains(&d)) {
        return Err(PrintError::invalid_payload(
            "dpi must be between 72 and 1200",
        ));
    }
    Ok(())
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
