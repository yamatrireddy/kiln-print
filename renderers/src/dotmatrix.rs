//! DOT_MATRIX documents: ESC/P text for impact printers with pitch, line spacing, form
//! length, perforation skip, margins, styles and code pages. Output is plain ESC/P bytes;
//! it is never rasterised or converted to PDF.

use base64::Engine;
use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{
    Document, DocumentType, DotMatrixDocument, DotMatrixItem, DotMatrixLine, DotMatrixQuality,
};
use kiln_core::provider::{PayloadKind, PrintPayload, RawPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};
use kiln_protocols::encoding::{TextEncoder, canonical_name};
use kiln_protocols::escp_commands as escp;

#[derive(Debug, Clone, Copy, Default)]
pub struct DotMatrixRenderer;

impl DocumentRenderer for DotMatrixRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::DotMatrix
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Raw]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::DotMatrix(doc) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a DOT_MATRIX document",
            ));
        };
        doc.validate()?;
        TextEncoder::for_label(&doc.encoding)?;
        Ok(())
    }

    fn render(&self, document: Document, _target: &RenderTarget) -> Result<PrintPayload> {
        let Document::DotMatrix(doc) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a DOT_MATRIX document",
            ));
        };
        Ok(PrintPayload::Raw(RawPayload {
            bytes: encode(&doc)?.into(),
            language: Some("ESC/P".into()),
        }))
    }
}

pub fn encode(doc: &DotMatrixDocument) -> Result<Vec<u8>> {
    let encoder = TextEncoder::for_label(&doc.encoding)?;
    let condensed_pitch = matches!(doc.cpi, 17 | 20);
    let mut out = Vec::new();
    if doc.initialize {
        out.extend(escp::INITIALIZE);
    }
    out.extend(escp::quality(doc.quality == DotMatrixQuality::Nlq));
    out.extend(escp::pitch(doc.cpi));
    // Line spacing must be set before ESC C n, which measures the page in lines.
    out.extend(escp::line_spacing(doc.lpi, doc.pins));
    if let Some(lines) = doc.form_length_lines {
        out.extend(escp::form_length_lines(lines));
    }
    if let Some(inches) = doc.form_length_inches {
        out.extend(escp::form_length_inches(inches));
    }
    if let Some(lines) = doc.skip_perforation_lines {
        out.extend(escp::skip_perforation(lines));
    }
    if let Some(columns) = doc.left_margin {
        out.extend(escp::left_margin(columns));
    }
    if let Some(columns) = doc.right_margin {
        out.extend(escp::right_margin(columns));
    }
    let table = doc
        .character_table
        .or_else(|| canonical_name(&doc.encoding).and_then(escp::character_table_for));
    if let Some(table) = table {
        out.extend(escp::character_table(table));
    }

    let text_line = |out: &mut Vec<u8>, text: &str| -> Result<()> {
        // A literal form feed inside text starts a new page.
        let mut pages = text.split('\u{c}').peekable();
        while let Some(part) = pages.next() {
            out.extend(encoder.encode(&part.replace(['\r', '\n'], " "))?);
            if pages.peek().is_some() {
                out.push(escp::FORM_FEED);
            }
        }
        out.extend(escp::CR_LF);
        Ok(())
    };

    for line in &doc.lines {
        match line {
            DotMatrixLine::Text(text) => {
                for part in text.replace("\r\n", "\n").split('\n') {
                    text_line(&mut out, part)?;
                }
            }
            DotMatrixLine::Item(item @ DotMatrixItem::Line { text, .. }) => {
                let style = item.style();
                let mut on = Vec::new();
                let mut off = Vec::new();
                if style.bold {
                    on.extend(escp::bold(true));
                    off.extend(escp::bold(false));
                }
                if style.double_strike {
                    on.extend(escp::double_strike(true));
                    off.extend(escp::double_strike(false));
                }
                if style.italic {
                    on.extend(escp::italic(true));
                    off.extend(escp::italic(false));
                }
                if style.underline {
                    on.extend(escp::underline(true));
                    off.extend(escp::underline(false));
                }
                if style.double_width {
                    on.extend(escp::double_width(true));
                    off.extend(escp::double_width(false));
                }
                if style.condensed && !condensed_pitch {
                    on.push(escp::CONDENSED_ON);
                    off.push(escp::CONDENSED_OFF);
                }
                out.extend(on);
                out.extend(encoder.encode(&text.replace(['\r', '\n', '\u{c}'], " "))?);
                out.extend(off);
                out.extend(escp::CR_LF);
            }
            DotMatrixLine::Item(DotMatrixItem::LineFeed { lines }) => {
                for _ in 0..*lines {
                    out.extend(escp::CR_LF);
                }
            }
            DotMatrixLine::Item(DotMatrixItem::FormFeed) => out.push(escp::FORM_FEED),
            DotMatrixLine::Item(DotMatrixItem::Raw { data }) => out.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| {
                        PrintError::invalid_payload(format!("invalid base64 data: {e}"))
                    })?,
            ),
        }
    }
    if doc.form_feed && out.last() != Some(&escp::FORM_FEED) {
        out.push(escp::FORM_FEED);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(lines: Vec<DotMatrixLine>) -> DotMatrixDocument {
        serde_json::from_value::<DotMatrixDocument>(serde_json::json!({ "lines": [] }))
            .map(|mut d| {
                d.lines = lines;
                d
            })
            .expect("defaults")
    }

    #[test]
    fn golden_invoice_form() {
        let mut d = doc(vec![
            DotMatrixLine::Text("Invoice ½".into()),
            DotMatrixLine::Item(DotMatrixItem::Line {
                text: "TOTAL".into(),
                bold: true,
                condensed: true,
                double_width: false,
                underline: false,
                italic: false,
                double_strike: false,
            }),
        ]);
        d.cpi = 12;
        d.form_length_inches = Some(11);
        d.skip_perforation_lines = Some(3);
        let bytes = encode(&d).expect("encode");
        let mut expected = vec![0x1B, b'@', 0x1B, b'x', 0, 0x1B, b'M', 0x12, 0x1B, b'2'];
        expected.extend([0x1B, b'C', 0, 11, 0x1B, b'N', 3, 0x1B, b't', 1]);
        expected.extend(b"Invoice \xAB\r\n");
        expected.extend([0x1B, b'E', 0x0F]);
        expected.extend(b"TOTAL");
        expected.extend([0x1B, b'F', 0x12, 0x0D, 0x0A, 0x0C]);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn continuous_forms_and_raw_sequences() {
        let mut d = doc(vec![
            DotMatrixLine::Item(DotMatrixItem::Raw {
                data: "G0EB".into(),
            }), // ESC A 1
            DotMatrixLine::Item(DotMatrixItem::FormFeed),
        ]);
        d.initialize = false;
        d.cpi = 17;
        d.lpi = 8.0;
        let bytes = encode(&d).expect("encode");
        assert!(bytes.starts_with(&[0x1B, b'x', 0, 0x1B, b'P', 0x0F, 0x1B, b'0']));
        assert!(
            bytes.windows(3).any(|w| w == [0x1B, b'A', 1]),
            "raw bytes pass through"
        );
        assert_eq!(
            bytes.iter().filter(|b| **b == 0x0C).count(),
            1,
            "no second form feed"
        );
    }

    #[test]
    fn embedded_form_feeds_break_pages() {
        let bytes =
            encode(&doc(vec![DotMatrixLine::Text("page1\u{c}page2".into())])).expect("encode");
        assert!(bytes.windows(7).any(|w| w == b"page1\x0cp"));
    }
}
