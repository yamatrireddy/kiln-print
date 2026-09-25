//! Translates a [`PageSetup`] into a driver-validated `DEVMODEW`.
//!
//! Requested values are checked against `DeviceCapabilitiesW` first: an unsupported paper
//! size, tray, duplex or colour request fails the job with a clear error instead of being
//! silently replaced by the driver's default.

use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::model::{ColorMode, Duplex, Orientation, PageSetup, PaperRequest};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, DM_COLOR, DM_DEFAULTSOURCE, DM_DUPLEX, DM_IN_BUFFER, DM_ORIENTATION, DM_OUT_BUFFER,
    DM_PAPERLENGTH, DM_PAPERSIZE, DM_PAPERWIDTH, DMCOLOR_COLOR, DMCOLOR_MONOCHROME,
    DMDUP_HORIZONTAL, DMDUP_SIMPLEX, DMDUP_VERTICAL,
};
use windows::Win32::Graphics::Printing::DocumentPropertiesW;
use windows::core::PCWSTR;

use crate::capabilities;
use crate::ffi::{Buffer, PrinterHandle, PrinterNameExt, wide};

/// `DMPAPER_USER`: custom size given by `dmPaperWidth`/`dmPaperLength`.
const DMPAPER_USER: i16 = 256;

/// Returns `None` when nothing needs overriding (use the printer's defaults).
pub(crate) fn build(
    printer_name: &str,
    port: Option<&str>,
    setup: &PageSetup,
    orientation: Option<Orientation>,
) -> Result<Option<Buffer>, PrintError> {
    let needs = orientation.is_some()
        || setup.paper_size.is_some()
        || setup.duplex.is_some()
        || setup.color.is_some()
        || setup.tray.is_some();
    if !needs {
        return Ok(None);
    }
    let caps = capabilities::query(printer_name, port);
    let unsupported = |message: String| {
        PrintError::new(ErrorCode::UnsupportedOperation, message).with_printer_name(printer_name)
    };

    let paper = match &setup.paper_size {
        None => None,
        Some(request) => Some(
            resolve_paper(request, caps.paper_sizes.as_deref())
                .map_err(|m| PrintError::invalid_payload(m).with_printer_name(printer_name))?,
        ),
    };
    if setup.duplex.is_some_and(|d| d != Duplex::Simplex) && caps.duplex == Some(false) {
        return Err(unsupported(format!(
            "printer '{printer_name}' cannot print double-sided"
        )));
    }
    if setup.color == Some(ColorMode::Color) && caps.color == Some(false) {
        return Err(unsupported(format!(
            "printer '{printer_name}' cannot print in colour"
        )));
    }
    let tray = match &setup.tray {
        None => None,
        Some(tray) => {
            let trays = caps.trays.unwrap_or_default();
            let found = trays
                .iter()
                .find(|t| t.id == *tray || t.name.eq_ignore_ascii_case(tray))
                .ok_or_else(|| {
                    PrintError::invalid_payload(format!(
                        "tray '{tray}' does not exist on printer '{printer_name}'"
                    ))
                    .with_printer_name(printer_name)
                })?;
            Some(
                found
                    .id
                    .parse::<i16>()
                    .map_err(|_| PrintError::internal("non-numeric tray id"))?,
            )
        }
    };

    let handle = PrinterHandle::open(printer_name)?;
    let name_w = wide(printer_name);
    let name = PCWSTR(name_w.as_ptr());
    let driver_error = || {
        PrintError::new(
            ErrorCode::SpoolerError,
            "the printer driver rejected the page settings",
        )
        .with_printer_name(printer_name)
    };
    // SAFETY: fMode 0 returns the size of the driver's DEVMODE.
    let size = unsafe { DocumentPropertiesW(None, handle.raw(), name, None, None, 0) };
    let size = usize::try_from(size)
        .ok()
        .filter(|s| *s >= std::mem::size_of::<DEVMODEW>());
    let mut buffer = Buffer::new(size.ok_or_else(driver_error)?);
    let dm = buffer.as_mut_ptr::<DEVMODEW>();
    // SAFETY: `buffer` has the size the driver asked for.
    if unsafe { DocumentPropertiesW(None, handle.raw(), name, Some(dm), None, DM_OUT_BUFFER.0) } < 0
    {
        return Err(driver_error());
    }
    // SAFETY: `dm` points at the DEVMODEW the driver just initialised; the union member
    // used is the printer variant.
    unsafe {
        let fields = &mut (*dm).Anonymous1.Anonymous1;
        if let Some(orientation) = orientation {
            fields.dmOrientation = if orientation == Orientation::Landscape {
                2
            } else {
                1
            };
            (*dm).dmFields |= DM_ORIENTATION;
        }
        match paper {
            Some(Paper::Native(id)) => {
                fields.dmPaperSize = id;
                (*dm).dmFields |= DM_PAPERSIZE;
            }
            Some(Paper::Custom {
                width_tenths,
                length_tenths,
            }) => {
                fields.dmPaperSize = DMPAPER_USER;
                fields.dmPaperWidth = width_tenths;
                fields.dmPaperLength = length_tenths;
                (*dm).dmFields |= DM_PAPERSIZE | DM_PAPERWIDTH | DM_PAPERLENGTH;
            }
            None => {}
        }
        if let Some(tray) = tray {
            fields.dmDefaultSource = tray;
            (*dm).dmFields |= DM_DEFAULTSOURCE;
        }
        if let Some(duplex) = setup.duplex {
            (*dm).dmDuplex = match duplex {
                Duplex::Simplex => DMDUP_SIMPLEX,
                Duplex::LongEdge => DMDUP_VERTICAL,
                Duplex::ShortEdge => DMDUP_HORIZONTAL,
            };
            (*dm).dmFields |= DM_DUPLEX;
        }
        if let Some(color) = setup.color {
            (*dm).dmColor = match color {
                ColorMode::Color => DMCOLOR_COLOR,
                ColorMode::Monochrome => DMCOLOR_MONOCHROME,
            };
            (*dm).dmFields |= DM_COLOR;
        }
    }
    // Let the driver validate and merge (it also updates its private DEVMODE section).
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
    if merged < 0 {
        return Err(driver_error());
    }
    Ok(Some(buffer))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paper {
    Native(i16),
    Custom {
        width_tenths: i16,
        length_tenths: i16,
    },
}

fn resolve_paper(
    request: &PaperRequest,
    available: Option<&[kiln_core::model::PaperSize]>,
) -> Result<Paper, String> {
    let available = available.unwrap_or_default();
    match request {
        PaperRequest::Named(name) => {
            let wanted = name.trim();
            available
                .iter()
                .find(|p| p.id == wanted || p.name.eq_ignore_ascii_case(wanted))
                // Driver names are often decorated ("A4 210 x 297 mm", "Letter 8.5x11in").
                .or_else(|| {
                    available.iter().find(|p| {
                        p.name
                            .split(|c: char| c.is_whitespace() || c == '(')
                            .next()
                            .is_some_and(|first| first.eq_ignore_ascii_case(wanted))
                    })
                })
                .and_then(|p| p.id.parse().ok().map(Paper::Native))
                .ok_or_else(|| {
                    let names: Vec<_> =
                        available.iter().take(25).map(|p| p.name.as_str()).collect();
                    format!(
                        "paper size '{wanted}' is not supported by this printer; available: {}",
                        names.join(", ")
                    )
                })
        }
        PaperRequest::Custom {
            width_mm,
            height_mm,
        } => {
            // Prefer a matching predefined size: drivers honour those far more reliably.
            let (w, h) = (f64::from(*width_mm), f64::from(*height_mm));
            if let Some(p) = available
                .iter()
                .find(|p| (p.width_mm - w).abs() <= 1.0 && (p.height_mm - h).abs() <= 1.0)
            {
                if let Ok(id) = p.id.parse() {
                    return Ok(Paper::Native(id));
                }
            }
            Ok(Paper::Custom {
                width_tenths: (w * 10.0).round() as i16,
                length_tenths: (h * 10.0).round() as i16,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::model::PaperSize;

    fn papers() -> Vec<PaperSize> {
        vec![
            PaperSize {
                id: "1".into(),
                name: "Letter".into(),
                width_mm: 215.9,
                height_mm: 279.4,
            },
            PaperSize {
                id: "9".into(),
                name: "A4 210 x 297 mm".into(),
                width_mm: 210.0,
                height_mm: 297.0,
            },
        ]
    }

    #[test]
    fn named_and_custom_paper_resolution() {
        let p = papers();
        assert_eq!(
            resolve_paper(&PaperRequest::Named("letter".into()), Some(&p)),
            Ok(Paper::Native(1))
        );
        assert_eq!(
            resolve_paper(&PaperRequest::Named("A4".into()), Some(&p)),
            Ok(Paper::Native(9))
        );
        assert_eq!(
            resolve_paper(&PaperRequest::Named("9".into()), Some(&p)),
            Ok(Paper::Native(9))
        );
        assert!(resolve_paper(&PaperRequest::Named("A3".into()), Some(&p)).is_err());
        let near_a4 = PaperRequest::Custom {
            width_mm: 210.4,
            height_mm: 296.8,
        };
        assert_eq!(resolve_paper(&near_a4, Some(&p)), Ok(Paper::Native(9)));
        let label = PaperRequest::Custom {
            width_mm: 100.0,
            height_mm: 150.0,
        };
        assert_eq!(
            resolve_paper(&label, Some(&p)),
            Ok(Paper::Custom {
                width_tenths: 1000,
                length_tenths: 1500
            })
        );
    }
}
