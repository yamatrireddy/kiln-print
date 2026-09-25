//! Driver-rendered text printing through GDI.
//!
//! Used for `TEXT` documents in `RENDERED` mode. Works with every driver model (v3, v4,
//! XPS) because GDI output is converted by the spooler as needed.

use std::path::Path;

use kiln_core::error::PrintError;
use kiln_core::model::{Orientation, TextAlignment};
use kiln_core::provider::TextLayout;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    CreateDCW, CreateFontIndirectW, DEFAULT_CHARSET, DEVMODEW, DM_IN_BUFFER, DM_ORIENTATION,
    DM_OUT_BUFFER, DeleteDC, DeleteObject, FW_BOLD, FW_NORMAL, GetDeviceCaps,
    GetTextExtentPoint32W, GetTextMetricsW, HDC, HGDIOBJ, HORZRES, LOGFONTW, LOGPIXELSX,
    LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, PROOF_QUALITY,
    SelectObject, SetBkMode, TEXTMETRICW, TRANSPARENT, TextOutW, VERTRES,
};
use windows::Win32::Graphics::Printing::DocumentPropertiesW;
use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::core::{PCWSTR, w};

use crate::ffi::{Buffer, PrinterHandle, PrinterNameExt, map_error, wide};
use crate::layout;

const DEFAULT_FONT: &str = "Courier New";

struct Dc(HDC);

impl Drop for Dc {
    fn drop(&mut self) {
        // SAFETY: the DC came from CreateDCW and is deleted once.
        let _ = unsafe { DeleteDC(self.0) };
    }
}

struct ActiveDoc<'a> {
    dc: &'a Dc,
    finished: bool,
}

impl Drop for ActiveDoc<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // SAFETY: a document is active on this DC.
            unsafe { AbortDoc(self.dc.0) };
        }
    }
}

pub(crate) fn submit(
    printer_name: &str,
    document_name: &str,
    copies: u32,
    text: &TextLayout,
    output_file: Option<&Path>,
) -> Result<u32, PrintError> {
    let name_w = wide(printer_name);
    let devmode = devmode_for(printer_name, &name_w, text.orientation);
    let devmode_ptr = devmode.as_ref().map(|b| b.as_ptr::<DEVMODEW>());

    // SAFETY: strings are null-terminated; the DEVMODE buffer outlives the call.
    let dc = Dc(unsafe {
        CreateDCW(
            w!("WINSPOOL"),
            PCWSTR(name_w.as_ptr()),
            PCWSTR::null(),
            devmode_ptr,
        )
    });
    if dc.0.is_invalid() {
        return Err(last_error(
            "could not create a device context for the printer",
            printer_name,
        ));
    }

    let doc_name = wide(document_name);
    let output = output_file.map(|p| wide(&p.to_string_lossy()));
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
        return Err(last_error("the spooler refused the job", printer_name));
    }
    let mut doc = ActiveDoc {
        dc: &dc,
        finished: false,
    };

    let page = PageGeometry::measure(&dc, text);
    let font = Font::create(text, page.dpi_y)?;
    let _selected = font.select(&dc);
    // SAFETY: valid DC.
    unsafe { SetBkMode(dc.0, TRANSPARENT) };

    let mut metrics = TEXTMETRICW::default();
    // SAFETY: valid DC and out-pointer.
    unsafe { GetTextMetricsW(dc.0, &mut metrics) }
        .ok()
        .map_err(|e| {
            map_error("could not read font metrics", &e).with_printer_name(printer_name)
        })?;
    let line_height = (metrics.tmHeight + metrics.tmExternalLeading).max(1);
    let width = (page.right - page.left).max(1);

    let measure = |s: &str| -> i32 {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE::default();
        // SAFETY: valid DC; `units` outlives the call.
        let _ = unsafe { GetTextExtentPoint32W(dc.0, &units, &mut size) };
        size.cx
    };
    let fits = |s: &str| measure(s) <= width;
    let pages: Vec<Vec<String>> = text
        .pages
        .iter()
        .map(|lines| {
            lines
                .iter()
                .flat_map(|line| {
                    if text.wrap {
                        layout::wrap_line(line, &fits)
                    } else {
                        vec![line.clone()]
                    }
                })
                .collect()
        })
        .collect();
    let lines_per_page = ((page.bottom - page.top) / line_height).max(1) as usize;
    let pages = layout::paginate(pages, lines_per_page);

    for _ in 0..copies {
        for lines in &pages {
            // SAFETY: a document is active.
            if unsafe { StartPage(dc.0) } <= 0 {
                return Err(last_error("could not start a page", printer_name));
            }
            let mut y = page.top;
            for line in lines {
                let units: Vec<u16> = line.encode_utf16().collect();
                let x = match text.alignment {
                    TextAlignment::Left => page.left,
                    TextAlignment::Center => page.left + (width - measure(line)).max(0) / 2,
                    TextAlignment::Right => page.right - measure(line),
                };
                // SAFETY: valid DC; `units` outlives the call.
                unsafe { TextOutW(dc.0, x.max(0), y, &units) }
                    .ok()
                    .map_err(|e| {
                        map_error("drawing text failed", &e).with_printer_name(printer_name)
                    })?;
                y += line_height;
            }
            // SAFETY: a page is active.
            if unsafe { EndPage(dc.0) } <= 0 {
                return Err(last_error("could not finish a page", printer_name));
            }
        }
    }
    // SAFETY: a document is active.
    if unsafe { EndDoc(dc.0) } <= 0 {
        return Err(last_error("could not finish the spool job", printer_name));
    }
    doc.finished = true;
    Ok(job_id as u32)
}

/// Printable area in device pixels, relative to the printable origin.
struct PageGeometry {
    dpi_y: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl PageGeometry {
    fn measure(dc: &Dc, text: &TextLayout) -> Self {
        // SAFETY: valid DC.
        let cap = |index| unsafe { GetDeviceCaps(Some(dc.0), index) };
        let (dpi_x, dpi_y) = (cap(LOGPIXELSX).max(1), cap(LOGPIXELSY).max(1));
        let (printable_w, printable_h) = (cap(HORZRES), cap(VERTRES));
        let (offset_x, offset_y) = (cap(PHYSICALOFFSETX), cap(PHYSICALOFFSETY));
        let (paper_w, paper_h) = (
            cap(PHYSICALWIDTH).max(printable_w),
            cap(PHYSICALHEIGHT).max(printable_h),
        );
        let px = |mm: f32, dpi: i32| (f64::from(mm) / 25.4 * f64::from(dpi)).round() as i32;
        let m = text.margins_mm;
        // Margins are measured from the paper edge; GDI coordinates start at the edge of
        // the printable area, so subtract the unprintable offset and clamp.
        Self {
            dpi_y,
            left: (px(m.left, dpi_x) - offset_x).clamp(0, printable_w),
            top: (px(m.top, dpi_y) - offset_y).clamp(0, printable_h),
            right: (paper_w - px(m.right, dpi_x) - offset_x).clamp(0, printable_w),
            bottom: (paper_h - px(m.bottom, dpi_y) - offset_y).clamp(0, printable_h),
        }
    }
}

struct Font(windows::Win32::Graphics::Gdi::HFONT);

impl Font {
    fn create(text: &TextLayout, dpi_y: i32) -> Result<Self, PrintError> {
        let mut lf = LOGFONTW {
            // Negative height selects by character height (point size), not cell height.
            lfHeight: -((f64::from(text.font_size_pt) * f64::from(dpi_y) / 72.0).round() as i32)
                .max(1),
            lfWeight: if text.bold {
                FW_BOLD.0 as i32
            } else {
                FW_NORMAL.0 as i32
            },
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: PROOF_QUALITY,
            ..Default::default()
        };
        let face = text.font_family.as_deref().unwrap_or(DEFAULT_FONT);
        for (dst, src) in lf.lfFaceName.iter_mut().take(31).zip(face.encode_utf16()) {
            *dst = src;
        }
        // SAFETY: `lf` is a fully initialised LOGFONTW.
        let font = unsafe { CreateFontIndirectW(&lf) };
        if font.is_invalid() {
            return Err(PrintError::internal("CreateFontIndirectW failed"));
        }
        Ok(Self(font))
    }

    fn select<'a>(&'a self, dc: &'a Dc) -> Selected<'a> {
        // SAFETY: valid DC and font.
        let previous = unsafe { SelectObject(dc.0, HGDIOBJ(self.0.0)) };
        Selected { dc, previous }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: the font is no longer selected (Selected restores first) and deleted once.
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

/// Restores the previously selected font before the font is deleted.
struct Selected<'a> {
    dc: &'a Dc,
    previous: HGDIOBJ,
}

impl Drop for Selected<'_> {
    fn drop(&mut self) {
        // SAFETY: valid DC and the object that was selected before.
        unsafe { SelectObject(self.dc.0, self.previous) };
    }
}

/// Builds a DEVMODE with the requested orientation merged through the driver. Returns
/// `None` (use driver defaults) when no override is needed or the driver refuses.
fn devmode_for(
    printer_name: &str,
    name_w: &[u16],
    orientation: Option<Orientation>,
) -> Option<Buffer> {
    let orientation = orientation?;
    let handle = PrinterHandle::open(printer_name).ok()?;
    let name = PCWSTR(name_w.as_ptr());
    // SAFETY: fMode 0 returns the required DEVMODE size.
    let size = unsafe { DocumentPropertiesW(None, handle.raw(), name, None, None, 0) };
    let mut buffer = Buffer::new(usize::try_from(size).ok().filter(|s| *s > 0)?);
    let dm = buffer.as_mut_ptr::<DEVMODEW>();
    // SAFETY: `buffer` has the size the driver asked for.
    if unsafe { DocumentPropertiesW(None, handle.raw(), name, Some(dm), None, DM_OUT_BUFFER.0) } < 0
    {
        return None;
    }
    // SAFETY: `dm` points at a DEVMODEW the driver just initialised.
    unsafe {
        (*dm).Anonymous1.Anonymous1.dmOrientation = match orientation {
            Orientation::Portrait => 1,
            Orientation::Landscape => 2,
        };
        (*dm).dmFields |= DM_ORIENTATION;
    }
    // SAFETY: in/out DEVMODE is the same valid buffer, as the API permits.
    let merged = unsafe {
        DocumentPropertiesW(
            None,
            handle.raw(),
            name,
            Some(dm),
            Some(dm),
            DM_IN_BUFFER.0 | DM_OUT_BUFFER.0,
        )
    };
    (merged >= 0).then_some(buffer)
}

fn last_error(context: &str, printer: &str) -> PrintError {
    map_error(context, &windows::core::Error::from_thread()).with_printer_name(printer)
}
