//! RAW passthrough. The output shares the input buffer: not a single byte is copied,
//! re-encoded, normalised or appended.

use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{Document, DocumentType};
use kiln_core::provider::{PayloadKind, PrintPayload, RawPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};

#[derive(Debug, Clone, Copy, Default)]
pub struct RawRenderer;

impl DocumentRenderer for RawRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Raw
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Raw]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        match document {
            Document::Raw(_) => Ok(()),
            _ => Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a RAW document",
            )),
        }
    }

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Raw(raw) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a RAW document",
            ));
        };
        if !target.accepted.contains(&PayloadKind::Raw) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                format!("printer '{}' does not accept RAW data", target.printer.name),
            )
            .with_printer(target.printer.id.as_str()));
        }
        Ok(PrintPayload::Raw(RawPayload {
            bytes: raw.data,
            language: raw.language,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use kiln_core::model::*;

    pub(crate) fn target(accepted: Vec<PayloadKind>) -> RenderTarget {
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
                language: None,
                capabilities: None,
            },
            accepted,
            default_paper_mm: None,
        }
    }

    #[test]
    fn raw_bytes_pass_through_without_copying() {
        // Every byte value, including NUL, ESC, CR/LF and 0xFF.
        let data = Bytes::from((0u8..=255).cycle().take(4096).collect::<Vec<_>>());
        let doc = Document::Raw(RawDocument {
            data: data.clone(),
            language: Some("ESC/P".into()),
        });
        let PrintPayload::Raw(out) = RawRenderer
            .render(doc, &target(vec![PayloadKind::Raw]))
            .expect("render")
        else {
            panic!("expected raw payload");
        };
        assert_eq!(out.bytes, data);
        assert_eq!(
            out.bytes.as_ptr(),
            data.as_ptr(),
            "payload must share the input buffer"
        );
    }

    #[test]
    fn refuses_printers_without_raw_support() {
        let doc = Document::Raw(RawDocument {
            data: Bytes::from_static(b"x"),
            language: None,
        });
        let err = RawRenderer
            .render(doc, &target(vec![PayloadKind::Text]))
            .expect_err("no raw");
        assert_eq!(err.error_code, ErrorCode::UnsupportedOperation);
    }
}
