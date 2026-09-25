//! Page setup shared by every graphical document type (PDF, image, HTML).
//!
//! Every field is optional: `None` means "use the printer's current default" (orientation
//! `None` means "follow the content"). Providers translate this into native settings
//! (a `DEVMODE` on Windows, IPP attributes on CUPS) and reject values the printer does not
//! support, rather than silently substituting.

use serde::{Deserialize, Serialize};

use super::{MarginsMm, Orientation};
use crate::error::{PrintError, Result};

/// A paper size by name/native id (`"A4"`, `"Letter"`, `"9"`) or explicit dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaperRequest {
    Named(String),
    Custom {
        #[serde(rename = "widthMm")]
        width_mm: f32,
        #[serde(rename = "heightMm")]
        height_mm: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Duplex {
    Simplex,
    /// Flip on the long edge (book style for portrait).
    LongEdge,
    /// Flip on the short edge (calendar style for portrait).
    ShortEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ColorMode {
    Color,
    Monochrome,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageSetup {
    pub paper_size: Option<PaperRequest>,
    /// `None`: follow the content (landscape pages print landscape).
    pub orientation: Option<Orientation>,
    /// `None`: use the printer's minimum (unprintable) margins.
    pub margins_mm: Option<MarginsMm>,
    pub duplex: Option<Duplex>,
    pub color: Option<ColorMode>,
    /// Input tray id, as listed in `PrinterCapabilities::trays`.
    pub tray: Option<String>,
}

impl PageSetup {
    pub fn validate(&self) -> Result<()> {
        if let Some(PaperRequest::Custom {
            width_mm,
            height_mm,
        }) = &self.paper_size
        {
            let ok = |v: f32| (10.0..=3000.0).contains(&v);
            if !ok(*width_mm) || !ok(*height_mm) {
                return Err(PrintError::invalid_payload(
                    "custom paper dimensions must be between 10 and 3000 mm",
                ));
            }
        }
        if let Some(PaperRequest::Named(name)) = &self.paper_size {
            if name.trim().is_empty() || name.len() > 64 {
                return Err(PrintError::invalid_payload(
                    "paperSize name must be 1-64 characters",
                ));
            }
        }
        if let Some(m) = self.margins_mm {
            if [m.top, m.right, m.bottom, m.left]
                .iter()
                .any(|v| !(0.0..=200.0).contains(v))
            {
                return Err(PrintError::invalid_payload(
                    "margins must be between 0 and 200 mm",
                ));
            }
        }
        Ok(())
    }
}

/// Inclusive, 1-based page ranges parsed from `"1-3,5,8-"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRanges(Vec<(u32, Option<u32>)>);

impl PageRanges {
    pub fn parse(spec: &str) -> Result<Self> {
        let invalid = || {
            PrintError::invalid_payload(format!(
                "invalid pageRange '{spec}'; use e.g. \"1-3,5,8-\""
            ))
        };
        let mut ranges = Vec::new();
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (start, end) = match part.split_once('-') {
                Some((a, b)) => {
                    let start = a.trim().parse::<u32>().map_err(|_| invalid())?;
                    let end = match b.trim() {
                        "" => None,
                        b => Some(b.parse::<u32>().map_err(|_| invalid())?),
                    };
                    (start, end)
                }
                None => {
                    let page = part.parse::<u32>().map_err(|_| invalid())?;
                    (page, Some(page))
                }
            };
            if start == 0 || end.is_some_and(|e| e < start) {
                return Err(invalid());
            }
            ranges.push((start, end));
        }
        if ranges.is_empty() {
            return Err(invalid());
        }
        Ok(Self(ranges))
    }

    /// Selected zero-based page indices for a document of `page_count` pages, in the
    /// order given (so `"3,1"` prints page 3 first).
    pub fn indices(&self, page_count: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for &(start, end) in &self.0 {
            let end = end.unwrap_or(page_count).min(page_count);
            for page in start..=end {
                out.push(page - 1);
            }
        }
        out
    }
}

/// How content is sized onto the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Scale up or down to fit the area, preserving aspect ratio.
    Fit,
    /// Like `Fit`, but never enlarge.
    ShrinkToFit,
    /// Scale to cover the whole area, preserving aspect ratio; overflow is clipped.
    Fill,
    /// Physical size of the content (PDF page size, image size at its DPI).
    ActualSize,
    /// Percentage of the actual size.
    Percent(f32),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Align {
    #[default]
    Center,
    TopLeft,
}

/// Rectangle in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Computes where content of physical size `content_in` (inches) lands inside `area`
/// on a device with resolution `dpi`. The result may extend beyond `area` for `Fill`,
/// `ActualSize` and `Percent`; callers clip.
pub fn place(
    content_in: (f64, f64),
    area: Rect,
    dpi: (f64, f64),
    placement: Placement,
    align: Align,
) -> Rect {
    let natural_w = (content_in.0 * dpi.0).max(1.0);
    let natural_h = (content_in.1 * dpi.1).max(1.0);
    let fit = (f64::from(area.width) / natural_w).min(f64::from(area.height) / natural_h);
    let fill = (f64::from(area.width) / natural_w).max(f64::from(area.height) / natural_h);
    let scale = match placement {
        Placement::Fit => fit,
        Placement::ShrinkToFit => fit.min(1.0),
        Placement::Fill => fill,
        Placement::ActualSize => 1.0,
        Placement::Percent(p) => f64::from(p) / 100.0,
    };
    let width = (natural_w * scale).round().max(1.0) as i32;
    let height = (natural_h * scale).round().max(1.0) as i32;
    let (x, y) = match align {
        Align::Center => (
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
        ),
        Align::TopLeft => (area.x, area.y),
    };
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        width: 2000,
        height: 3000,
    };

    #[test]
    fn placement_modes() {
        // A 4x3 inch image on a 200 dpi device: natural size 800x600 px.
        let p = |placement, align| place((4.0, 3.0), AREA, (200.0, 200.0), placement, align);
        assert_eq!(
            p(Placement::ActualSize, Align::TopLeft),
            Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600
            }
        );
        assert_eq!(p(Placement::Fit, Align::TopLeft).width, 2000);
        assert_eq!(p(Placement::Fit, Align::TopLeft).height, 1500);
        assert_eq!(
            p(Placement::ShrinkToFit, Align::Center),
            Rect {
                x: 600,
                y: 1200,
                width: 800,
                height: 600
            }
        );
        let fill = p(Placement::Fill, Align::Center);
        assert_eq!((fill.width, fill.height), (4000, 3000));
        assert_eq!(fill.x, -1000, "fill overflows and is centred");
        assert_eq!(p(Placement::Percent(50.0), Align::TopLeft).width, 400);
    }

    #[test]
    fn shrink_to_fit_shrinks_oversized_content() {
        let r = place(
            (20.0, 10.0),
            AREA,
            (200.0, 200.0),
            Placement::ShrinkToFit,
            Align::TopLeft,
        );
        assert_eq!((r.width, r.height), (2000, 1000));
    }

    #[test]
    fn page_ranges() {
        let r = PageRanges::parse("1-3, 5,8-").expect("valid");
        assert_eq!(r.indices(9), vec![0, 1, 2, 4, 7, 8]);
        assert_eq!(r.indices(4), vec![0, 1, 2]);
        assert_eq!(
            PageRanges::parse("3,1").expect("valid").indices(5),
            vec![2, 0]
        );
        for bad in ["", "0", "3-1", "a", "1-2-3", "-2"] {
            assert!(PageRanges::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paper_request_accepts_name_or_dimensions() {
        let named: PaperRequest = serde_json::from_str("\"A4\"").expect("name");
        assert_eq!(named, PaperRequest::Named("A4".into()));
        let custom: PaperRequest =
            serde_json::from_str(r#"{"widthMm":100,"heightMm":150}"#).expect("custom");
        assert_eq!(
            custom,
            PaperRequest::Custom {
                width_mm: 100.0,
                height_mm: 150.0
            }
        );
        let bad = PageSetup {
            paper_size: Some(PaperRequest::Custom {
                width_mm: 1.0,
                height_mm: 150.0,
            }),
            ..PageSetup::default()
        };
        assert!(bad.validate().is_err());
    }
}
