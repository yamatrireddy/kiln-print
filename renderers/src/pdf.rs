//! PDF renderer: validates the document and options and hands the PDF to the provider.
//!
//! Rasterisation is platform-specific and lives in the provider (Windows uses the OS PDF
//! engine; CUPS accepts PDF natively), so this renderer never needs a PDF library and
//! never opens an external viewer.

use bytes::Bytes;
use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{Document, DocumentType, PageRanges, PdfDocument, PdfOptions};
use kiln_core::provider::{PayloadKind, PdfPayload, PrintPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};

/// Rasterisation cap when the client does not set `dpi`. 300 dpi is visually lossless for
/// text on office printers and keeps a Letter page under 30 MB of pixels.
pub const DEFAULT_MAX_DPI: u32 = 300;

#[derive(Debug, Clone, Copy, Default)]
pub struct PdfRenderer;

/// Checks the `%PDF-` signature. The PDF spec allows up to 1024 bytes of leading junk.
pub fn check_signature(data: &[u8]) -> Result<()> {
    let head = &data[..data.len().min(1024)];
    if head.windows(5).any(|w| w == b"%PDF-") {
        Ok(())
    } else {
        Err(PrintError::invalid_payload(
            "data is not a PDF document (missing %PDF- header)",
        ))
    }
}

pub fn payload(data: Bytes, options: &PdfOptions) -> Result<PdfPayload> {
    Ok(PdfPayload {
        bytes: data,
        pages: options
            .page_range
            .as_deref()
            .map(PageRanges::parse)
            .transpose()?,
        placement: options.scale.placement(),
        setup: options.page_setup(),
        max_dpi: options.dpi.unwrap_or(DEFAULT_MAX_DPI),
    })
}

impl DocumentRenderer for PdfRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Pdf
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Pdf]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::Pdf(PdfDocument { data, options }) = document else {
            return Err(wrong_type());
        };
        check_signature(data)?;
        options.validate()
    }

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Pdf(PdfDocument { data, options }) = document else {
            return Err(wrong_type());
        };
        if !target.accepted.contains(&PayloadKind::Pdf) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                format!(
                    "printer '{}' cannot print PDF documents",
                    target.printer.name
                ),
            )
            .with_printer(target.printer.id.as_str()));
        }
        Ok(PrintPayload::Pdf(payload(data, &options)?))
    }
}

fn wrong_type() -> PrintError {
    PrintError::new(ErrorCode::UnsupportedDocument, "expected a PDF document")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::model::{Placement, Scale, ScaleMode};

    #[test]
    fn signature_check() {
        assert!(check_signature(b"%PDF-1.7\n...").is_ok());
        assert!(check_signature(b"\xEF\xBB\xBF junk %PDF-1.4").is_ok());
        assert!(check_signature(b"<html>").is_err());
    }

    #[test]
    fn options_become_payload() {
        let options = PdfOptions {
            page_range: Some("2-3".into()),
            scale: Scale::Mode(ScaleMode::ActualSize),
            ..PdfOptions::default()
        };
        let p = payload(Bytes::from_static(b"%PDF-1.7"), &options).expect("payload");
        assert_eq!(p.pages.expect("ranges").indices(5), vec![1, 2]);
        assert_eq!(p.placement, Placement::ActualSize);
        assert_eq!(p.max_dpi, DEFAULT_MAX_DPI);
    }

    #[test]
    fn scale_accepts_keyword_or_percent() {
        let o: PdfOptions = serde_json::from_str(r#"{"scale": 80}"#).expect("percent");
        assert_eq!(o.scale.placement(), Placement::Percent(80.0));
        let o: PdfOptions =
            serde_json::from_str(r#"{"scale": "FIT", "duplex": "LONG_EDGE"}"#).expect("mode");
        assert_eq!(o.scale.placement(), Placement::Fit);
        assert!(serde_json::from_str::<PdfOptions>(r#"{"scael": 80}"#).is_err());
        let o: PdfOptions = serde_json::from_str(r#"{"scale": 0}"#).expect("parses");
        assert!(o.validate().is_err());
    }
}
