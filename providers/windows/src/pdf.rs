//! PDF rasterisation with the Windows built-in PDF engine (`Windows.Data.Pdf`, Windows 10+).
//!
//! No PDF viewer is opened and nothing is installed: pages are rendered straight into
//! memory at the size they will occupy on paper (capped by `max_dpi`) and printed through
//! the raster path.

use std::borrow::Cow;

use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::model::PageRanges;
use kiln_core::provider::RasterImage;
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::UI::Color;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

use crate::raster::RasterSource;

/// Initialises the Windows Runtime on the current (blocking-pool) thread. Idempotent.
fn ensure_winrt() {
    // SAFETY: RoInitialize may be called repeatedly; S_FALSE and RPC_E_CHANGED_MODE
    // (already initialised) are both fine for our use.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
}

fn engine_error(context: &str, error: &windows::core::Error) -> PrintError {
    tracing::warn!(target: "kiln::windows", %context, error = %error.message(), "PDF engine error");
    PrintError::new(
        ErrorCode::PrintFailed,
        format!("{context}: {}", error.message().trim()),
    )
}

pub(crate) struct PdfSource {
    document: PdfDocument,
    pages: Vec<u32>,
    max_dpi: u32,
    current: Option<RasterImage>,
}

impl PdfSource {
    pub(crate) fn open(
        bytes: &[u8],
        ranges: Option<&PageRanges>,
        max_dpi: u32,
    ) -> Result<Self, PrintError> {
        ensure_winrt();
        let load = || -> windows::core::Result<PdfDocument> {
            let stream = InMemoryRandomAccessStream::new()?;
            let writer = DataWriter::CreateDataWriter(&stream)?;
            writer.WriteBytes(bytes)?;
            writer.StoreAsync()?.join()?;
            writer.FlushAsync()?.join()?;
            writer.DetachStream()?;
            stream.Seek(0)?;
            PdfDocument::LoadFromStreamAsync(&stream)?.join()
        };
        let document = load().map_err(|e| {
            PrintError::invalid_payload(format!(
                "the PDF could not be opened (damaged or password-protected): {}",
                e.message().trim()
            ))
        })?;
        let count = document
            .PageCount()
            .map_err(|e| engine_error("could not read the PDF", &e))?;
        let pages = match ranges {
            Some(r) => r.indices(count),
            None => (0..count).collect(),
        };
        if pages.is_empty() {
            return Err(PrintError::invalid_payload(format!(
                "pageRange selects no pages (the document has {count})"
            )));
        }
        Ok(Self {
            document,
            pages,
            max_dpi: max_dpi.max(72),
            current: None,
        })
    }

    fn page(&self, index: usize) -> Result<windows::Data::Pdf::PdfPage, PrintError> {
        self.document
            .GetPage(self.pages[index])
            .map_err(|e| engine_error("could not read a PDF page", &e))
    }
}

impl RasterSource for PdfSource {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size_in(&mut self, index: usize) -> Result<(f64, f64), PrintError> {
        // Size is in device-independent pixels (1/96 in) and already reflects /Rotate.
        let size = self
            .page(index)?
            .Size()
            .map_err(|e| engine_error("could not read a PDF page", &e))?;
        Ok((f64::from(size.Width) / 96.0, f64::from(size.Height) / 96.0))
    }

    fn render(
        &mut self,
        index: usize,
        target: (u32, u32),
    ) -> Result<Cow<'_, RasterImage>, PrintError> {
        let (w_in, h_in) = self.page_size_in(index)?;
        // Never rasterise beyond max_dpi relative to the page's printed size.
        let scale_cap = (f64::from(self.max_dpi) * w_in / f64::from(target.0.max(1)))
            .min(f64::from(self.max_dpi) * h_in / f64::from(target.1.max(1)))
            .min(1.0);
        let width = ((f64::from(target.0) * scale_cap).round() as u32).max(1);
        let height = ((f64::from(target.1) * scale_cap).round() as u32).max(1);

        let page = self.page(index)?;
        let render = || -> windows::core::Result<Vec<u8>> {
            let options = PdfPageRenderOptions::new()?;
            options.SetDestinationWidth(width)?;
            options.SetDestinationHeight(height)?;
            options.SetBackgroundColor(Color {
                A: 255,
                R: 255,
                G: 255,
                B: 255,
            })?;
            options.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId()?)?;
            let out = InMemoryRandomAccessStream::new()?;
            page.RenderWithOptionsToStreamAsync(&out, &options)?
                .join()?;
            let size = out.Size()?;
            let reader = DataReader::CreateDataReader(&out.GetInputStreamAt(0)?)?;
            reader
                .LoadAsync(u32::try_from(size).unwrap_or(u32::MAX))?
                .join()?;
            let mut bytes = vec![0u8; size as usize];
            reader.ReadBytes(&mut bytes)?;
            Ok(bytes)
        };
        let bmp = render().map_err(|e| engine_error("rendering a PDF page failed", &e))?;
        let decoded = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp)
            .map_err(|e| PrintError::internal(format!("decoding rendered PDF page: {e}")))?
            .into_rgb8();
        let (dpi_x, dpi_y) = (
            (f64::from(decoded.width()) / w_in).round() as u32,
            (f64::from(decoded.height()) / h_in).round() as u32,
        );
        let page = self.current.insert(RasterImage {
            width: decoded.width(),
            height: decoded.height(),
            rgb: decoded.into_raw(),
            dpi_x: dpi_x.max(1),
            dpi_y: dpi_y.max(1),
        });
        Ok(Cow::Borrowed(page))
    }
}
