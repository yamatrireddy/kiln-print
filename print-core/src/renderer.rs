//! Document renderers turn a validated [`Document`] into a device-ready [`PrintPayload`].
//!
//! Renderers are pure: no device I/O, no network access. They run on the blocking pool
//! because conversion (PDF rasterisation, HTML layout in later phases) can be CPU heavy.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::error::Result;
use crate::model::{Document, DocumentType, Printer};
use crate::provider::{PayloadKind, PrintPayload};

/// Information about the destination a renderer may use to pick an output form.
#[derive(Debug, Clone)]
pub struct RenderTarget {
    pub printer: Printer,
    /// Payload kinds the destination provider accepts for this printer.
    pub accepted: Vec<PayloadKind>,
    /// Printer's default paper (portrait, mm), filled in for document types that are
    /// paginated before printing (HTML).
    pub default_paper_mm: Option<(f32, f32)>,
}

pub trait DocumentRenderer: Send + Sync {
    fn document_type(&self) -> DocumentType;

    /// Payload kinds this renderer can produce. A document type is offered for a printer
    /// only when its provider accepts at least one of them.
    fn output_kinds(&self) -> &'static [PayloadKind];

    /// Checks the document without producing output. Called before a job is queued so
    /// that malformed input is rejected synchronously.
    fn validate(&self, document: &Document) -> Result<()>;

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload>;
}

#[derive(Default, Clone)]
pub struct RendererRegistry {
    renderers: HashMap<DocumentType, Arc<dyn DocumentRenderer>>,
}

impl RendererRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or replaces) the renderer for its document type.
    pub fn register(&mut self, renderer: Arc<dyn DocumentRenderer>) {
        self.renderers.insert(renderer.document_type(), renderer);
    }

    pub fn get(&self, document_type: DocumentType) -> Option<&Arc<dyn DocumentRenderer>> {
        self.renderers.get(&document_type)
    }

    pub fn document_types(&self) -> Vec<DocumentType> {
        let mut types: Vec<_> = self.renderers.keys().copied().collect();
        types.sort_by_key(|t| *t as u8);
        types
    }
}

impl fmt::Debug for RendererRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.renderers.keys()).finish()
    }
}
