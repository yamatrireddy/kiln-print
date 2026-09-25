//! LABEL documents: a language-neutral label encoded for the target printer's language.

use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{Document, DocumentType};
use kiln_core::protocol::ProtocolRegistry;
use kiln_core::provider::{PayloadKind, PrintPayload, RawPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};

#[derive(Debug, Clone)]
pub struct LabelRenderer {
    protocols: ProtocolRegistry,
}

impl Default for LabelRenderer {
    fn default() -> Self {
        let mut protocols = ProtocolRegistry::new();
        for protocol in kiln_protocols::builtin() {
            protocols.register(protocol);
        }
        Self { protocols }
    }
}

impl LabelRenderer {
    /// Uses a custom registry, so languages added by an embedder get label support too.
    pub fn with_protocols(protocols: ProtocolRegistry) -> Self {
        Self { protocols }
    }

    fn label_languages(&self) -> Vec<&'static str> {
        self.protocols
            .languages()
            .iter()
            .filter(|l| {
                self.protocols.resolve(l.id).is_some_and(|p| {
                    // Probe support without a real document: languages without label
                    // support return None for any input.
                    p.encode_label(&probe()).is_some()
                })
            })
            .map(|l| l.id)
            .collect()
    }
}

fn probe() -> kiln_core::model::LabelDocument {
    kiln_core::model::LabelDocument {
        width_mm: 10.0,
        height_mm: 10.0,
        dpi: 203,
        language: None,
        gap_mm: None,
        darkness: None,
        speed: None,
        elements: vec![kiln_core::model::LabelElement::Raw {
            data: String::new(),
        }],
    }
}

impl DocumentRenderer for LabelRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Label
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Raw]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::Label(label) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a LABEL document",
            ));
        };
        label.validate()?;
        if let Some(language) = &label.language {
            let supported = self
                .protocols
                .resolve(language)
                .is_some_and(|p| p.encode_label(&probe()).is_some());
            if !supported {
                return Err(PrintError::invalid_payload(format!(
                    "'{language}' is not a label language; use one of {}",
                    self.label_languages().join(", ")
                )));
            }
        }
        Ok(())
    }

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Label(label) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a LABEL document",
            ));
        };
        let language = label
            .language
            .clone()
            .or_else(|| target.printer.language.clone())
            .ok_or_else(|| {
                PrintError::new(
                    ErrorCode::UnsupportedDocument,
                    format!(
                        "the label language of printer '{}' is unknown; set language to one of {}",
                        target.printer.name,
                        self.label_languages().join(", ")
                    ),
                )
                .with_printer(target.printer.id.as_str())
            })?;
        let protocol = self.protocols.resolve(&language).ok_or_else(|| {
            PrintError::invalid_payload(format!("unknown printer language '{language}'"))
        })?;
        let bytes = protocol.encode_label(&label).ok_or_else(|| {
            PrintError::new(
                ErrorCode::UnsupportedDocument,
                format!(
                    "printer '{}' uses {}, which has no label support; use one of {}",
                    target.printer.name,
                    protocol.info().id,
                    self.label_languages().join(", ")
                ),
            )
        })??;
        Ok(PrintPayload::Raw(RawPayload {
            bytes: bytes.into(),
            language: Some(protocol.info().id.to_owned()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::model::*;

    fn target(language: Option<&str>) -> RenderTarget {
        RenderTarget {
            printer: Printer {
                id: "p".into(),
                name: "Label printer".into(),
                display_name: "Label printer".into(),
                provider: "t".into(),
                connection: ConnectionType::Usb,
                driver: None,
                port: None,
                location: None,
                default: false,
                online: true,
                status: PrinterState::Ready,
                conditions: vec![],
                queued_jobs: None,
                language: language.map(str::to_owned),
                capabilities: None,
            },
            accepted: vec![PayloadKind::Raw],
            default_paper_mm: None,
        }
    }

    fn label(language: Option<&str>) -> Document {
        Document::Label(LabelDocument {
            width_mm: 50.0,
            height_mm: 25.0,
            dpi: 203,
            language: language.map(str::to_owned),
            gap_mm: None,
            darkness: None,
            speed: None,
            elements: vec![LabelElement::Text {
                x_mm: 2.0,
                y_mm: 2.0,
                text: "Hi".into(),
                height_mm: 3.0,
                rotation: 0,
                font: None,
            }],
        })
    }

    fn render(doc: Document, target: &RenderTarget) -> Result<RawPayload> {
        let renderer = LabelRenderer::default();
        renderer.validate(&doc)?;
        match renderer.render(doc, target)? {
            PrintPayload::Raw(raw) => Ok(raw),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn language_comes_from_document_or_printer_hint() {
        let raw = render(label(None), &target(Some("zpl"))).expect("hint");
        assert!(raw.bytes.starts_with(b"^XA"));
        assert_eq!(raw.language.as_deref(), Some("ZPL"));
        let raw = render(label(Some("TSPL2")), &target(Some("ZPL"))).expect("explicit wins");
        assert!(raw.bytes.starts_with(b"SIZE 50 mm,25 mm"));
    }

    #[test]
    fn missing_or_non_label_languages_are_reported() {
        let err = render(label(None), &target(None)).expect_err("no language");
        assert_eq!(err.error_code, ErrorCode::UnsupportedDocument);
        assert!(
            err.message.contains("ZPL, EPL, CPCL, TSPL") || err.message.contains("ZPL"),
            "{}",
            err.message
        );
        let err = render(label(None), &target(Some("ESC/POS"))).expect_err("receipt printer");
        assert_eq!(err.error_code, ErrorCode::UnsupportedDocument);
        let err = render(label(Some("ESC/POS")), &target(None)).expect_err("validation");
        assert_eq!(err.error_code, ErrorCode::InvalidPayload);
    }
}
