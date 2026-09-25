//! Platform adapter interfaces.
//!
//! A [`PrintProvider`] is the only place that talks to an OS spooler, a socket or a serial
//! port. Methods are **blocking** on purpose: native printing APIs are blocking, and the
//! engine runs every provider call on a bounded blocking pool so a stuck driver can never
//! stall the async runtime or another printer's queue.

use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::{ErrorCode, PrintError, Result};
use crate::model::{
    Align, Job, JobId, MarginsMm, Orientation, PageRanges, PageSetup, Placement, Printer,
    PrinterCapabilities, TextAlignment,
};

/// Anything that can enumerate printers. Every [`PrintProvider`] is one; standalone
/// discoverers (mDNS/IPP browse, configured TCP printers) can be added later and yield
/// printers whose `provider` field names the [`PrintProvider`] that drives them.
pub trait PrinterDiscoveryProvider: Send + Sync {
    fn discover(&self) -> Result<Vec<Printer>>;
}

/// Device-ready output of a [`crate::renderer::DocumentRenderer`].
#[derive(Debug, Clone)]
pub enum PrintPayload {
    /// Bytes delivered to the device unmodified.
    Raw(RawPayload),
    /// Logical text laid out and drawn by the platform graphics stack.
    Text(TextLayout),
    /// A PDF document, rasterised or passed through natively by the provider.
    Pdf(PdfPayload),
    /// A decoded raster image placed on a page by the provider.
    Image(ImagePayload),
}

impl PrintPayload {
    pub fn kind(&self) -> PayloadKind {
        match self {
            Self::Raw(_) => PayloadKind::Raw,
            Self::Text(_) => PayloadKind::Text,
            Self::Pdf(_) => PayloadKind::Pdf,
            Self::Image(_) => PayloadKind::Image,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PayloadKind {
    Raw,
    Text,
    Pdf,
    Image,
}

impl PayloadKind {
    pub const ALL: [Self; 4] = [Self::Raw, Self::Text, Self::Pdf, Self::Image];
}

#[derive(Debug, Clone)]
pub struct RawPayload {
    pub bytes: Bytes,
    pub language: Option<String>,
}

/// Text ready for native layout. Pages are split on form feeds; tabs are expanded. Line
/// wrapping and pagination happen in the provider because they depend on device metrics.
#[derive(Debug, Clone)]
pub struct TextLayout {
    /// Explicit pages (split on `\f`), each a list of logical lines.
    pub pages: Vec<Vec<String>>,
    pub font_family: Option<String>,
    pub font_size_pt: f32,
    pub bold: bool,
    pub alignment: TextAlignment,
    pub margins_mm: MarginsMm,
    pub orientation: Option<Orientation>,
    pub wrap: bool,
}

/// A validated PDF plus how to print it.
#[derive(Debug, Clone)]
pub struct PdfPayload {
    pub bytes: Bytes,
    /// Pages to print; all pages when `None`.
    pub pages: Option<PageRanges>,
    pub placement: Placement,
    pub setup: PageSetup,
    /// Upper bound for rasterisation resolution, if the provider rasterises.
    pub max_dpi: u32,
}

/// 8-bit RGB pixels, row-major, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    /// Physical resolution used for actual-size placement.
    pub dpi_x: u32,
    pub dpi_y: u32,
}

impl std::fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("dpi", &(self.dpi_x, self.dpi_y))
            .finish_non_exhaustive()
    }
}

impl RasterImage {
    /// Physical size in inches.
    pub fn size_in(&self) -> (f64, f64) {
        (
            f64::from(self.width) / f64::from(self.dpi_x.max(1)),
            f64::from(self.height) / f64::from(self.dpi_y.max(1)),
        )
    }
}

#[derive(Debug, Clone)]
pub struct ImagePayload {
    /// Shared so copies and retries of the same page never duplicate pixel buffers.
    pub image: Arc<RasterImage>,
    pub placement: Placement,
    pub align: Align,
    pub setup: PageSetup,
}

/// Everything a provider needs to submit one job.
#[derive(Debug, Clone)]
pub struct SubmitSpec {
    pub job_id: JobId,
    /// Shown in the OS queue. Already sanitised by the engine.
    pub document_name: String,
    pub copies: u32,
    pub payload: PrintPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// The OS spooler owns the complete job. The engine keeps monitoring it.
    Spooled { spooler_job_id: u64 },
    /// A direct transport wrote every byte to the device connection.
    Delivered,
}

/// A spooler job's state as last observed by the provider.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderJobState {
    /// In the spooler, not yet printing.
    Pending,
    Printing,
    /// Held by a condition the user can fix; the spooler will resume.
    Blocked {
        condition: ErrorCode,
        message: Option<String>,
    },
    /// The spooler reports the job as printed.
    Printed,
    Failed(PrintError),
    /// Deleted/cancelled in the spooler (by us, the user, or an administrator).
    Cancelled,
    /// No longer in the queue and no terminal flag was seen.
    Gone,
}

/// One entry of an OS queue, including jobs other applications submitted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub spooler_job_id: u64,
    pub document_name: Option<String>,
    /// Normalised flags such as `PRINTING`, `PAUSED`, `ERROR`, `PAPER_OUT`.
    pub status: Vec<String>,
    /// Driver-supplied status text, if any.
    pub status_text: Option<String>,
    pub position: Option<u32>,
    pub total_pages: Option<u32>,
    pub pages_printed: Option<u32>,
    pub size_bytes: Option<u64>,
    pub submitted_at: Option<DateTime<Utc>>,
    /// The agent job this spooler job belongs to, if it was submitted through the agent.
    pub kiln_job: Option<Box<Job>>,
}

pub trait PrintProvider: PrinterDiscoveryProvider {
    /// Stable provider id; becomes the prefix of every [`crate::model::PrinterId`].
    fn id(&self) -> &str;

    fn capabilities(&self, printer: &Printer) -> Result<PrinterCapabilities>;

    /// Whether this printer can accept the payload kind through this provider.
    fn supports(&self, printer: &Printer, kind: PayloadKind) -> bool;

    /// Size of the printer's current default paper in millimetres (portrait), if known.
    /// Used to lay out content that is paginated before printing (HTML).
    fn default_paper_mm(&self, _printer: &Printer) -> Option<(f32, f32)> {
        None
    }

    /// Submits a complete job. Must be all-or-nothing where the platform allows it: on
    /// failure the provider aborts the partial spool job so nothing half-prints.
    fn submit(&self, printer: &Printer, spec: &SubmitSpec) -> Result<SubmitOutcome>;

    fn job_state(&self, printer: &Printer, spooler_job_id: u64) -> Result<ProviderJobState>;

    fn cancel(&self, printer: &Printer, spooler_job_id: u64) -> Result<()>;

    fn queue(&self, printer: &Printer) -> Result<Vec<QueueEntry>>;
}
