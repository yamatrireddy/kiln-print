//! Plain-text renderer.
//!
//! * `RAW` mode encodes the text and sends it as bytes — the right choice for dot-matrix,
//!   line and receipt printers, which then print with their resident fonts. Line endings
//!   are normalised to the requested terminator; nothing else is added except an optional
//!   trailing form feed.
//! * `RENDERED` mode produces a [`TextLayout`] that the platform provider draws with the
//!   printer driver (GDI on Windows), honouring font, size, alignment and margins.

use bytes::Bytes;
use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{Document, DocumentType, TextDocument, TextMode, TextOptions};
use kiln_core::provider::{PayloadKind, PrintPayload, RawPayload, TextLayout};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};

use crate::encoding::TextEncoder;

const FORM_FEED: char = '\u{c}';

#[derive(Debug, Clone, Copy, Default)]
pub struct TextRenderer;

impl DocumentRenderer for TextRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Text
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Text, PayloadKind::Raw]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::Text(TextDocument { options, .. }) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a TEXT document",
            ));
        };
        validate_options(options)
    }

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Text(TextDocument { text, options }) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a TEXT document",
            ));
        };
        let text = expand_tabs(&normalise_newlines(&text), options.tab_width);
        let (kind, payload) = match options.mode {
            TextMode::Raw => (
                PayloadKind::Raw,
                PrintPayload::Raw(render_raw(&text, &options)?),
            ),
            TextMode::Rendered => (
                PayloadKind::Text,
                PrintPayload::Text(layout(&text, &options)),
            ),
        };
        if !target.accepted.contains(&kind) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                format!(
                    "printer '{}' does not support {:?} text mode",
                    target.printer.name, options.mode
                ),
            )
            .with_printer(target.printer.id.as_str()));
        }
        Ok(payload)
    }
}

fn validate_options(o: &TextOptions) -> Result<()> {
    if !(1.0..=200.0).contains(&o.font_size) {
        return Err(PrintError::invalid_payload(
            "fontSize must be between 1 and 200 points",
        ));
    }
    let m = o.margins_mm;
    if [m.top, m.right, m.bottom, m.left]
        .iter()
        .any(|v| !(0.0..=100.0).contains(v))
    {
        return Err(PrintError::invalid_payload(
            "margins must be between 0 and 100 mm",
        ));
    }
    if o.tab_width > 32 {
        return Err(PrintError::invalid_payload("tabWidth must be at most 32"));
    }
    if let Some(font) = &o.font_family {
        // LOGFONT face names are limited to 31 characters.
        if font.is_empty() || font.chars().count() > 31 || font.chars().any(char::is_control) {
            return Err(PrintError::invalid_payload(
                "fontFamily must be 1-31 printable characters",
            ));
        }
    }
    if o.mode == TextMode::Raw {
        TextEncoder::for_label(&o.encoding)?;
    }
    Ok(())
}

fn normalise_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn expand_tabs(text: &str, width: u8) -> String {
    if width == 0 || !text.contains('\t') {
        return text.to_owned();
    }
    let width = usize::from(width);
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    for c in text.chars() {
        match c {
            '\t' => {
                let spaces = width - column % width;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' | FORM_FEED => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

fn render_raw(text: &str, options: &TextOptions) -> Result<RawPayload> {
    let encoder = TextEncoder::for_label(&options.encoding)?;
    let eol = options.line_ending.as_bytes();
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut out = Vec::with_capacity(text.len() + text.len() / 16 + 2);
    for line in body.split('\n') {
        out.extend_from_slice(&encoder.encode(line)?);
        out.extend_from_slice(eol);
    }
    if options.form_feed && !body.ends_with(FORM_FEED) {
        out.push(0x0C);
    }
    Ok(RawPayload {
        bytes: Bytes::from(out),
        language: None,
    })
}

fn layout(text: &str, options: &TextOptions) -> TextLayout {
    let pages = text
        .split(FORM_FEED)
        .map(|page| {
            let page = page.strip_suffix('\n').unwrap_or(page);
            page.split('\n').map(str::to_owned).collect()
        })
        .collect();
    TextLayout {
        pages,
        font_family: options.font_family.clone(),
        font_size_pt: options.font_size,
        bold: options.bold,
        alignment: options.alignment,
        margins_mm: options.margins_mm,
        orientation: options.orientation,
        wrap: options.wrap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::model::{ConnectionType, LineEnding, Printer, PrinterId, PrinterState};

    fn target() -> RenderTarget {
        RenderTarget {
            printer: Printer {
                id: PrinterId::from("t-1"),
                name: "Test".into(),
                display_name: "Test".into(),
                provider: "t".into(),
                connection: ConnectionType::Local,
                driver: None,
                port: None,
                location: None,
                default: false,
                online: true,
                status: PrinterState::Ready,
                conditions: vec![],
                queued_jobs: None,
                capabilities: None,
            },
            accepted: vec![PayloadKind::Raw, PayloadKind::Text],
        }
    }

    fn render(text: &str, options: TextOptions) -> PrintPayload {
        let doc = Document::Text(TextDocument {
            text: text.into(),
            options,
        });
        TextRenderer.validate(&doc).expect("valid");
        TextRenderer.render(doc, &target()).expect("render")
    }

    fn raw_opts() -> TextOptions {
        TextOptions {
            mode: TextMode::Raw,
            ..TextOptions::default()
        }
    }

    #[test]
    fn raw_mode_normalises_line_endings_and_appends_form_feed() {
        let PrintPayload::Raw(raw) = render("A\nB\r\nC", raw_opts()) else {
            panic!("raw")
        };
        assert_eq!(&raw.bytes[..], b"A\r\nB\r\nC\r\n\x0c");
    }

    #[test]
    fn raw_mode_respects_existing_form_feed_and_lf_option() {
        let opts = TextOptions {
            line_ending: LineEnding::Lf,
            ..raw_opts()
        };
        let PrintPayload::Raw(raw) = render("A\n\x0c", opts) else {
            panic!("raw")
        };
        assert_eq!(&raw.bytes[..], b"A\n\x0c\n");
    }

    #[test]
    fn raw_mode_without_form_feed_for_continuous_forms() {
        let opts = TextOptions {
            form_feed: false,
            ..raw_opts()
        };
        let PrintPayload::Raw(raw) = render("line 1\n", opts) else {
            panic!("raw")
        };
        assert_eq!(&raw.bytes[..], b"line 1\r\n");
    }

    #[test]
    fn raw_mode_encodes_cp437() {
        let opts = TextOptions {
            encoding: "ibm437".into(),
            form_feed: false,
            ..raw_opts()
        };
        let PrintPayload::Raw(raw) = render("½", opts) else {
            panic!("raw")
        };
        assert_eq!(&raw.bytes[..], &[0xAB, b'\r', b'\n']);
    }

    #[test]
    fn tabs_expand_to_columns() {
        assert_eq!(expand_tabs("a\tb\n\tc", 4), "a   b\n    c");
    }

    #[test]
    fn rendered_mode_splits_pages_on_form_feed() {
        let PrintPayload::Text(layout) = render("p1 l1\np1 l2\n\x0cp2", TextOptions::default())
        else {
            panic!("text")
        };
        assert_eq!(layout.pages, vec![vec!["p1 l1", "p1 l2"], vec!["p2"]]);
    }

    #[test]
    fn invalid_options_are_rejected() {
        let doc = Document::Text(TextDocument {
            text: "x".into(),
            options: TextOptions {
                font_size: 0.0,
                ..TextOptions::default()
            },
        });
        assert!(TextRenderer.validate(&doc).is_err());
        let doc = Document::Text(TextDocument {
            text: "x".into(),
            options: TextOptions {
                encoding: "nope".into(),
                ..raw_opts()
            },
        });
        assert!(TextRenderer.validate(&doc).is_err());
    }
}
