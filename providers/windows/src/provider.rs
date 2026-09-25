//! [`PrintProvider`] implementation for the Windows spooler.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use kiln_core::error::{PrintError, Result};
use kiln_core::model::{Printer, PrinterCapabilities};
use kiln_core::provider::{
    PayloadKind, PrintPayload, PrintProvider, PrinterDiscoveryProvider, ProviderJobState,
    QueueEntry, SubmitOutcome, SubmitSpec,
};

use crate::discovery::{self, MetaCache, PrinterMeta};
use crate::pdf::PdfSource;
use crate::raster::{self, ImageSource};
use crate::watch::JobWatchers;
use crate::{capabilities, gdi, jobs, raw, status};

pub(crate) const PROVIDER_ID: &str = "windows";

/// Drives printers installed in the Windows spooler (local, USB, network connections,
/// virtual printers). Stateless apart from a small driver-metadata cache.
#[derive(Debug, Default)]
pub struct WindowsPrintProvider {
    meta: MetaCache,
    watchers: JobWatchers,
}

impl WindowsPrintProvider {
    pub fn new() -> Self {
        Self {
            meta: Mutex::new(HashMap::new()),
            watchers: JobWatchers::default(),
        }
    }

    fn meta(&self, printer: &Printer) -> Option<PrinterMeta> {
        self.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&printer.name)
            .cloned()
    }

    /// Submits with the spooler's output redirected to `output` instead of the port.
    ///
    /// Test/diagnostic hook only: lets hardware tests verify spooled bytes through the
    /// real spooler without a physical device. It is intentionally not reachable from the
    /// agent API, because writing arbitrary paths on a client's behalf would be a
    /// file-write primitive.
    #[doc(hidden)]
    pub fn submit_to_file(
        &self,
        printer: &Printer,
        spec: &SubmitSpec,
        output: &Path,
    ) -> Result<SubmitOutcome> {
        self.submit_inner(printer, spec, Some(output))
    }

    fn submit_inner(
        &self,
        printer: &Printer,
        spec: &SubmitSpec,
        output: Option<&Path>,
    ) -> Result<SubmitOutcome> {
        // Subscribe to spooler notifications before the job exists, so its whole life
        // (including a print-and-vanish between polls) is observed.
        self.watchers.ensure(&printer.name);
        let raster_job = |setup, placement, align| raster::Job {
            printer_name: &printer.name,
            port: printer.port.as_deref(),
            document_name: &spec.document_name,
            copies: spec.copies,
            setup,
            placement,
            align,
            output_file: output,
        };
        let job_id = match &spec.payload {
            PrintPayload::Raw(payload) => raw::submit(
                &printer.name,
                &spec.document_name,
                spec.copies,
                &payload.bytes,
                output,
            )?,
            PrintPayload::Text(layout) => gdi::submit(
                &printer.name,
                &spec.document_name,
                spec.copies,
                layout,
                output,
            )?,
            PrintPayload::Pdf(pdf) => {
                let mut source = PdfSource::open(&pdf.bytes, pdf.pages.as_ref(), pdf.max_dpi)?;
                let job = raster_job(&pdf.setup, pdf.placement, kiln_core::model::Align::Center);
                raster::print(&job, &mut source)?
            }
            PrintPayload::Image(image) => {
                let job = raster_job(&image.setup, image.placement, image.align);
                raster::print(&job, &mut ImageSource(&image.image))?
            }
        };
        Ok(SubmitOutcome::Spooled {
            spooler_job_id: u64::from(job_id),
        })
    }
}

impl PrinterDiscoveryProvider for WindowsPrintProvider {
    fn discover(&self) -> Result<Vec<Printer>> {
        discovery::enumerate(&self.meta)
    }
}

impl PrintProvider for WindowsPrintProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn capabilities(&self, printer: &Printer) -> Result<PrinterCapabilities> {
        let mut caps = capabilities::query(&printer.name, printer.port.as_deref());
        if let Some(meta) = self.meta(printer) {
            caps.raw = meta.raw_supported();
            caps.datatypes = meta.datatypes;
        }
        Ok(caps)
    }

    fn supports(&self, printer: &Printer, kind: PayloadKind) -> bool {
        match kind {
            // Unknown metadata: let the spooler decide rather than refusing up front.
            PayloadKind::Raw => self
                .meta(printer)
                .and_then(|m| m.raw_supported())
                .unwrap_or(true),
            // GDI output (text, rasterised PDF pages, images) works with every driver model.
            PayloadKind::Text | PayloadKind::Pdf | PayloadKind::Image => true,
        }
    }

    fn submit(&self, printer: &Printer, spec: &SubmitSpec) -> Result<SubmitOutcome> {
        self.submit_inner(printer, spec, None)
    }

    fn job_state(&self, printer: &Printer, spooler_job_id: u64) -> Result<ProviderJobState> {
        let id = to_u32(spooler_job_id)?;
        let current = jobs::current(&printer.name, id)?;
        let observed = self.watchers.observed(&printer.name, id);
        let state = status::resolve_job_state(current, observed);
        if matches!(
            state,
            ProviderJobState::Printed | ProviderJobState::Cancelled | ProviderJobState::Gone
        ) {
            self.watchers.forget(&printer.name, id);
        }
        Ok(state)
    }

    fn default_paper_mm(&self, printer: &Printer) -> Option<(f32, f32)> {
        capabilities::default_paper_mm(&printer.name)
    }

    fn cancel(&self, printer: &Printer, spooler_job_id: u64) -> Result<()> {
        jobs::cancel(&printer.name, to_u32(spooler_job_id)?)
    }

    fn queue(&self, printer: &Printer) -> Result<Vec<QueueEntry>> {
        jobs::queue(&printer.name)
    }
}

fn to_u32(id: u64) -> Result<u32> {
    u32::try_from(id).map_err(|_| PrintError::internal("spooler job id out of range"))
}
