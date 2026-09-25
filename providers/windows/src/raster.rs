//! Prints raster pages (rendered PDF pages, images) through GDI.
//!
//! Pages are drawn with `StretchDIBits` in horizontal bands of at most ~8 MB, so neither
//! the agent nor the driver ever handles one giant bitmap, and a failure mid-document
//! aborts the whole job (`AbortDoc`) instead of printing a partial document.

use std::borrow::Cow;
use std::path::Path;

use kiln_core::error::PrintError;
use kiln_core::model::{Align, Orientation, PageSetup, Placement, Rect, place};
use kiln_core::provider::RasterImage;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DEVMODEW, DIB_RGB_COLORS, DeleteDC,
    GetDeviceCaps, HALFTONE, HDC, HORZRES, IntersectClipRect, LOGPIXELSX, LOGPIXELSY,
    PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY, SetBrushOrgEx,
    SetStretchBltMode, StretchDIBits, VERTRES,
};
use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::core::{PCWSTR, w};

use crate::devmode;
use crate::ffi::{PrinterNameExt, map_error, wide};

const BAND_BYTES: usize = 8 * 1024 * 1024;

/// Supplies pages to print. Sizes are asked for before rendering so that each page can be
/// rasterised directly at its final device size.
pub(crate) trait RasterSource {
    fn page_count(&self) -> usize;
    /// Physical page size in inches.
    fn page_size_in(&mut self, index: usize) -> Result<(f64, f64), PrintError>;
    /// Pixels for page `index`, ideally `target` sized (sources may return any size; GDI
    /// scales it to the destination).
    fn render(
        &mut self,
        index: usize,
        target: (u32, u32),
    ) -> Result<Cow<'_, RasterImage>, PrintError>;
}

struct Dc(HDC);

impl Drop for Dc {
    fn drop(&mut self) {
        // SAFETY: created by CreateDCW, deleted once.
        let _ = unsafe { DeleteDC(self.0) };
    }
}

struct ActiveDoc<'a>(&'a Dc, bool);

impl Drop for ActiveDoc<'_> {
    fn drop(&mut self) {
        if !self.1 {
            // SAFETY: a document is active on this DC.
            unsafe { AbortDoc(self.0.0) };
        }
    }
}

pub(crate) struct Job<'a> {
    pub printer_name: &'a str,
    pub port: Option<&'a str>,
    pub document_name: &'a str,
    pub copies: u32,
    pub setup: &'a PageSetup,
    pub placement: Placement,
    pub align: Align,
    pub output_file: Option<&'a Path>,
}

pub(crate) fn print(job: &Job<'_>, source: &mut dyn RasterSource) -> Result<u32, PrintError> {
    let printer = job.printer_name;
    if source.page_count() == 0 {
        return Err(PrintError::invalid_payload(
            "the document has no pages to print",
        ));
    }
    // Orientation follows the content unless the client chose one.
    let first = source.page_size_in(0)?;
    let content = if first.0 > first.1 {
        Orientation::Landscape
    } else {
        Orientation::Portrait
    };
    let orientation = Some(job.setup.orientation.unwrap_or(content));
    let devmode = devmode::build(printer, job.port, job.setup, orientation)?;
    let name_w = wide(printer);
    // SAFETY: strings are null-terminated; the DEVMODE buffer outlives the call.
    let dc = Dc(unsafe {
        CreateDCW(
            w!("WINSPOOL"),
            PCWSTR(name_w.as_ptr()),
            PCWSTR::null(),
            devmode.as_ref().map(|b| b.as_ptr::<DEVMODEW>()),
        )
    });
    if dc.0.is_invalid() {
        return Err(last_error(
            "could not create a device context for the printer",
            printer,
        ));
    }

    let doc_name = wide(job.document_name);
    let output = job.output_file.map(|p| wide(&p.to_string_lossy()));
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(doc_name.as_ptr()),
        lpszOutput: output
            .as_ref()
            .map_or(PCWSTR::null(), |o| PCWSTR(o.as_ptr())),
        lpszDatatype: PCWSTR::null(),
        fwType: 0,
    };
    // SAFETY: `info` and its strings outlive the call.
    let job_id = unsafe { StartDocW(dc.0, &info) };
    if job_id <= 0 {
        return Err(last_error("the spooler refused the job", printer));
    }
    let mut doc = ActiveDoc(&dc, false);

    let geometry = Geometry::measure(&dc);
    let area = geometry.area(job.setup, job.placement);
    for _ in 0..job.copies {
        for index in 0..source.page_count() {
            let size_in = source.page_size_in(index)?;
            let dest = place(size_in, area, geometry.dpi, job.placement, job.align);
            let target = (dest.width.max(1) as u32, dest.height.max(1) as u32);
            let image = source.render(index, target)?;

            // SAFETY: a document is active.
            if unsafe { StartPage(dc.0) } <= 0 {
                return Err(last_error("could not start a page", printer));
            }
            // DC state may reset per page, so set it after StartPage.
            // SAFETY: valid DC.
            unsafe {
                SetStretchBltMode(dc.0, HALFTONE);
                let _ = SetBrushOrgEx(dc.0, 0, 0, None);
                IntersectClipRect(
                    dc.0,
                    area.x,
                    area.y,
                    area.x + area.width,
                    area.y + area.height,
                );
            }
            draw(&dc, &image, dest).map_err(|e| e.with_printer_name(printer))?;
            // SAFETY: a page is active.
            if unsafe { EndPage(dc.0) } <= 0 {
                return Err(last_error("could not finish a page", printer));
            }
        }
    }
    // SAFETY: a document is active.
    if unsafe { EndDoc(dc.0) } <= 0 {
        return Err(last_error("could not finish the spool job", printer));
    }
    doc.1 = true;
    Ok(job_id as u32)
}

/// Device metrics in device pixels. GDI coordinates start at the printable-area origin,
/// which is offset from the paper edge by the unprintable margin.
struct Geometry {
    dpi: (f64, f64),
    printable: Rect,
    paper: Rect,
}

impl Geometry {
    fn measure(dc: &Dc) -> Self {
        // SAFETY: valid DC.
        let cap = |index| unsafe { GetDeviceCaps(Some(dc.0), index) };
        let (w, h) = (cap(HORZRES), cap(VERTRES));
        let (ox, oy) = (cap(PHYSICALOFFSETX), cap(PHYSICALOFFSETY));
        Self {
            dpi: (
                f64::from(cap(LOGPIXELSX).max(1)),
                f64::from(cap(LOGPIXELSY).max(1)),
            ),
            printable: Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            paper: Rect {
                x: -ox,
                y: -oy,
                width: cap(PHYSICALWIDTH).max(w),
                height: cap(PHYSICALHEIGHT).max(h),
            },
        }
    }

    /// The area content is placed in: the paper minus explicit margins; otherwise the
    /// printable area for fitting modes (nothing is clipped) and the whole paper for
    /// actual-size modes (content keeps its true position and size).
    fn area(&self, setup: &PageSetup, placement: Placement) -> Rect {
        if let Some(m) = setup.margins_mm {
            let px = |mm: f32, dpi: f64| (f64::from(mm) / 25.4 * dpi).round() as i32;
            let left = px(m.left, self.dpi.0);
            let top = px(m.top, self.dpi.1);
            return Rect {
                x: self.paper.x + left,
                y: self.paper.y + top,
                width: (self.paper.width - left - px(m.right, self.dpi.0)).max(1),
                height: (self.paper.height - top - px(m.bottom, self.dpi.1)).max(1),
            };
        }
        match placement {
            Placement::ActualSize | Placement::Percent(_) => self.paper,
            _ => self.printable,
        }
    }
}

/// Draws `image` into `dest` in bands of 24-bit top-down DIBs.
fn draw(dc: &Dc, image: &RasterImage, dest: Rect) -> Result<(), PrintError> {
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 {
        return Ok(());
    }
    let stride = (w * 3 + 3) & !3;
    let band_rows = (BAND_BYTES / stride).clamp(1, h);
    let mut bgr = vec![0u8; stride * band_rows];
    let mut row = 0;
    while row < h {
        let rows = band_rows.min(h - row);
        for r in 0..rows {
            let src = &image.rgb[(row + r) * w * 3..(row + r + 1) * w * 3];
            let dst = &mut bgr[r * stride..r * stride + w * 3];
            for (d, s) in dst.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
            }
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(rows as i32), // negative: top-down rows
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                biSizeImage: (stride * rows) as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        // Integer band edges so bands tile the destination without gaps or overlaps.
        let edge = |r: usize| dest.y + (r as i64 * i64::from(dest.height) / h as i64) as i32;
        let (y0, y1) = (edge(row), edge(row + rows));
        if y1 > y0 {
            // SAFETY: `bgr` holds `rows` rows of `stride` bytes described by `info`.
            let drawn = unsafe {
                StretchDIBits(
                    dc.0,
                    dest.x,
                    y0,
                    dest.width,
                    y1 - y0,
                    0,
                    0,
                    w as i32,
                    rows as i32,
                    Some(bgr.as_ptr().cast()),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                )
            };
            if drawn == 0 || drawn == windows::Win32::Graphics::Gdi::GDI_ERROR {
                return Err(map_error(
                    "drawing the page failed",
                    &windows::core::Error::from_thread(),
                ));
            }
        }
        row += rows;
    }
    Ok(())
}

fn last_error(context: &str, printer: &str) -> PrintError {
    map_error(context, &windows::core::Error::from_thread()).with_printer_name(printer)
}

/// A single in-memory image, printed as one page per copy.
pub(crate) struct ImageSource<'a>(pub &'a RasterImage);

impl RasterSource for ImageSource<'_> {
    fn page_count(&self) -> usize {
        1
    }

    fn page_size_in(&mut self, _index: usize) -> Result<(f64, f64), PrintError> {
        Ok(self.0.size_in())
    }

    fn render(
        &mut self,
        _index: usize,
        _target: (u32, u32),
    ) -> Result<Cow<'_, RasterImage>, PrintError> {
        Ok(Cow::Borrowed(self.0))
    }
}
