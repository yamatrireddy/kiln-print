//! Printer capabilities via `DeviceCapabilitiesW`. Each query that the driver does not
//! answer (returns -1 or 0 entries) is reported as unknown (`None`), never guessed.

use kiln_core::model::{Orientation, PaperSize, PrinterCapabilities, Resolution, Tray};
use windows::Win32::Storage::Xps::{
    DC_BINNAMES, DC_BINS, DC_COLLATE, DC_COLORDEVICE, DC_COPIES, DC_DUPLEX, DC_ENUMRESOLUTIONS,
    DC_ORIENTATION, DC_PAPERNAMES, DC_PAPERS, DC_PAPERSIZE, DeviceCapabilitiesW,
    PRINTER_DEVICE_CAPABILITIES,
};
use windows::core::{PCWSTR, PWSTR};

use crate::ffi::wide;

struct Device {
    name: Vec<u16>,
    port: Option<Vec<u16>>,
}

impl Device {
    /// Calls `DeviceCapabilitiesW`, returning the entry count/value (`-1` on failure).
    fn call(&self, cap: PRINTER_DEVICE_CAPABILITIES, output: Option<*mut u16>) -> i32 {
        let port = self
            .port
            .as_ref()
            .map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr()));
        // SAFETY: strings are null-terminated; `output` (if any) was sized by a prior
        // count query as documented for each capability.
        unsafe {
            DeviceCapabilitiesW(
                PCWSTR(self.name.as_ptr()),
                port,
                cap,
                output.map(PWSTR),
                None,
            )
        }
    }

    fn count(&self, cap: PRINTER_DEVICE_CAPABILITIES) -> Option<usize> {
        usize::try_from(self.call(cap, None))
            .ok()
            .filter(|n| *n > 0)
    }

    fn flag(&self, cap: PRINTER_DEVICE_CAPABILITIES) -> Option<bool> {
        match self.call(cap, None) {
            -1 => None,
            v => Some(v == 1),
        }
    }

    /// Fetches `n` fixed-width UTF-16 names of `width` units each.
    fn names(
        &self,
        cap: PRINTER_DEVICE_CAPABILITIES,
        n: usize,
        width: usize,
    ) -> Option<Vec<String>> {
        let mut buf = vec![0u16; n * width];
        (self.call(cap, Some(buf.as_mut_ptr())) >= 0).then(|| {
            buf.chunks(width)
                .map(|chunk| {
                    let end = chunk.iter().position(|c| *c == 0).unwrap_or(chunk.len());
                    String::from_utf16_lossy(&chunk[..end])
                })
                .collect()
        })
    }

    fn words(&self, cap: PRINTER_DEVICE_CAPABILITIES, n: usize) -> Option<Vec<u16>> {
        let mut buf = vec![0u16; n];
        (self.call(cap, Some(buf.as_mut_ptr())) >= 0).then_some(buf)
    }

    fn longs(&self, cap: PRINTER_DEVICE_CAPABILITIES, n: usize) -> Option<Vec<i32>> {
        let mut buf = vec![0i32; n];
        (self.call(cap, Some(buf.as_mut_ptr().cast())) >= 0).then_some(buf)
    }
}

pub(crate) fn query(printer_name: &str, port: Option<&str>) -> PrinterCapabilities {
    let dev = Device {
        name: wide(printer_name),
        port: port.map(wide),
    };

    let paper_sizes = dev.count(DC_PAPERS).and_then(|n| {
        let ids = dev.words(DC_PAPERS, n)?;
        let names = dev.names(DC_PAPERNAMES, n, 64)?;
        let sizes = dev.longs(DC_PAPERSIZE, n * 2)?; // POINT pairs, tenths of a millimetre
        Some(
            (0..n)
                .map(|i| PaperSize {
                    id: ids[i].to_string(),
                    name: names[i].clone(),
                    width_mm: f64::from(sizes[i * 2]) / 10.0,
                    height_mm: f64::from(sizes[i * 2 + 1]) / 10.0,
                })
                .collect(),
        )
    });

    let trays = dev.count(DC_BINS).and_then(|n| {
        let ids = dev.words(DC_BINS, n)?;
        let names = dev.names(DC_BINNAMES, n, 24)?;
        Some(
            ids.iter()
                .zip(names)
                .map(|(id, name)| Tray {
                    id: id.to_string(),
                    name,
                })
                .collect(),
        )
    });

    let resolutions = dev.count(DC_ENUMRESOLUTIONS).and_then(|n| {
        let values = dev.longs(DC_ENUMRESOLUTIONS, n * 2)?;
        let mut list: Vec<Resolution> = values
            .chunks(2)
            .filter(|p| p[0] > 0 && p[1] > 0)
            .map(|p| Resolution {
                x_dpi: p[0] as u32,
                y_dpi: p[1] as u32,
            })
            .collect();
        list.dedup();
        Some(list)
    });

    let orientations = match dev.call(DC_ORIENTATION, None) {
        -1 => None,
        0 => Some(vec![Orientation::Portrait]),
        _ => Some(vec![Orientation::Portrait, Orientation::Landscape]),
    };

    PrinterCapabilities {
        paper_sizes,
        color: dev.flag(DC_COLORDEVICE),
        duplex: dev.flag(DC_DUPLEX),
        resolutions,
        max_copies: u32::try_from(dev.call(DC_COPIES, None))
            .ok()
            .filter(|n| *n > 0),
        collate: dev.flag(DC_COLLATE),
        orientations,
        trays,
        raw: None,
        datatypes: None,
        document_types: Vec::new(),
    }
}
