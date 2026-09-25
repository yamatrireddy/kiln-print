//! Image renderer: decodes PNG, JPEG, BMP, TIFF (first page) and GIF (first frame) into an
//! RGB raster with rotation applied. Placement on the page happens in the provider, which
//! knows the device's printable area and resolution.
//!
//! Decoding untrusted images is a classic attack surface, so dimensions and allocations
//! are capped before any pixel buffer is created (decompression-bomb defence).

use std::io::Cursor;
use std::sync::Arc;

use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{Document, DocumentType, ImageDocument};
use kiln_core::provider::{ImagePayload, PayloadKind, PrintPayload, RasterImage};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};

const DEFAULT_DPI: f32 = 96.0;
const SUPPORTED: [ImageFormat; 5] = [
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::Bmp,
    ImageFormat::Tiff,
    ImageFormat::Gif,
];

#[derive(Debug, Clone, Copy)]
pub struct ImageRenderer {
    /// Maximum decoded pixel count (width x height).
    pub max_pixels: u64,
}

impl Default for ImageRenderer {
    fn default() -> Self {
        // 80 MP: an A3 page at 600 dpi is ~70 MP.
        Self {
            max_pixels: 80_000_000,
        }
    }
}

fn format_of(data: &[u8]) -> Result<ImageFormat> {
    image::guess_format(data)
        .ok()
        .filter(|f| SUPPORTED.contains(f))
        .ok_or_else(|| {
            PrintError::new(
                ErrorCode::UnsupportedDocument,
                "unsupported image format; use PNG, JPEG, BMP, TIFF or GIF",
            )
        })
}

impl DocumentRenderer for ImageRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Image
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Image]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::Image(ImageDocument { data, options }) = document else {
            return Err(wrong_type());
        };
        format_of(data)?;
        options.validate()
    }

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Image(ImageDocument { data, options }) = document else {
            return Err(wrong_type());
        };
        if !target.accepted.contains(&PayloadKind::Image) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                format!("printer '{}' cannot print images", target.printer.name),
            )
            .with_printer(target.printer.id.as_str()));
        }
        let format = format_of(&data)?;
        let decoded = self.decode(&data, format)?;
        let (dpi_x, dpi_y) = options
            .dpi
            .map(|d| (d, d))
            .or_else(|| metadata_dpi(&data, format))
            .unwrap_or((DEFAULT_DPI, DEFAULT_DPI));
        let (decoded, dpi_x, dpi_y) = match options.rotate {
            90 => (decoded.rotate90(), dpi_y, dpi_x),
            180 => (decoded.rotate180(), dpi_x, dpi_y),
            270 => (decoded.rotate270(), dpi_y, dpi_x),
            _ => (decoded, dpi_x, dpi_y),
        };
        let raster = RasterImage {
            width: decoded.width(),
            height: decoded.height(),
            rgb: flatten_on_white(decoded),
            dpi_x: dpi_x.round().max(1.0) as u32,
            dpi_y: dpi_y.round().max(1.0) as u32,
        };
        Ok(PrintPayload::Image(ImagePayload {
            image: Arc::new(raster),
            placement: options.placement(),
            align: options.align,
            setup: options.page_setup(),
        }))
    }
}

impl ImageRenderer {
    fn decode(&self, data: &[u8], format: ImageFormat) -> Result<DynamicImage> {
        let mut reader = ImageReader::with_format(Cursor::new(data), format);
        let mut limits = Limits::default();
        limits.max_image_width = Some(40_000);
        limits.max_image_height = Some(40_000);
        limits.max_alloc = Some(self.max_pixels.saturating_mul(4));
        reader.limits(limits);
        let (w, h) = reader
            .into_dimensions()
            .map_err(|e| PrintError::invalid_payload(format!("unreadable image: {e}")))?;
        if u64::from(w) * u64::from(h) > self.max_pixels {
            return Err(PrintError::new(
                ErrorCode::PayloadTooLarge,
                format!(
                    "image is {w}x{h} pixels; the limit is {} pixels",
                    self.max_pixels
                ),
            ));
        }
        let mut reader = ImageReader::with_format(Cursor::new(data), format);
        let mut limits = Limits::default();
        limits.max_alloc = Some(self.max_pixels.saturating_mul(8));
        reader.limits(limits);
        reader
            .decode()
            .map_err(|e| PrintError::invalid_payload(format!("could not decode image: {e}")))
    }
}

/// Converts to RGB, compositing any transparency over white paper (dropping alpha would
/// turn transparent areas black on many images).
fn flatten_on_white(image: DynamicImage) -> Vec<u8> {
    if !image.color().has_alpha() {
        return image.into_rgb8().into_raw();
    }
    let rgba = image.into_rgba8();
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.pixels() {
        let [r, g, b, a] = px.0;
        let a = u16::from(a);
        let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        out.extend_from_slice(&[blend(r), blend(g), blend(b)]);
    }
    out
}

/// Physical resolution recorded in the file, if any (PNG `pHYs`, JPEG JFIF, BMP header).
fn metadata_dpi(data: &[u8], format: ImageFormat) -> Option<(f32, f32)> {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let dpi = match format {
        ImageFormat::Png => {
            let mut pos = 8;
            loop {
                let header = data.get(pos..pos + 8)?;
                let len = be32(&header[..4]) as usize;
                let kind = &header[4..8];
                if kind == b"pHYs" {
                    let body = data.get(pos + 8..pos + 17)?;
                    if body[8] != 1 {
                        return None; // unit unknown: aspect ratio only
                    }
                    break (
                        be32(&body[..4]) as f32 * 0.0254,
                        be32(&body[4..8]) as f32 * 0.0254,
                    );
                }
                if kind == b"IDAT" || kind == b"IEND" {
                    return None;
                }
                pos = pos.checked_add(12 + len)?;
            }
        }
        ImageFormat::Jpeg => {
            let app0 = data.get(2..18)?;
            if &app0[..2] != b"\xFF\xE0" || &app0[4..9] != b"JFIF\0" {
                return None;
            }
            let (x, y) = (
                f32::from(u16::from_be_bytes([app0[12], app0[13]])),
                f32::from(u16::from_be_bytes([app0[14], app0[15]])),
            );
            match app0[11] {
                1 => (x, y),
                2 => (x * 2.54, y * 2.54),
                _ => return None,
            }
        }
        ImageFormat::Bmp => {
            let le = |o: usize| {
                data.get(o..o + 4)
                    .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            };
            (le(38)? as f32 * 0.0254, le(42)? as f32 * 0.0254)
        }
        _ => return None,
    };
    (dpi.0 >= 10.0 && dpi.1 >= 10.0).then_some(dpi)
}

fn wrong_type() -> PrintError {
    PrintError::new(ErrorCode::UnsupportedDocument, "expected an IMAGE document")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use kiln_core::model::*;

    fn target() -> RenderTarget {
        RenderTarget {
            printer: Printer {
                id: PrinterId::from("t-1"),
                name: "Test".into(),
                display_name: "Test".into(),
                provider: "t".into(),
                connection: ConnectionType::Local,
                driver: None,
                port: None,
                location: None,
                default: false,
                online: true,
                status: PrinterState::Ready,
                conditions: vec![],
                queued_jobs: None,
                capabilities: None,
            },
            accepted: vec![PayloadKind::Image],
            default_paper_mm: None,
        }
    }

    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).expect("encode");
        out.into_inner()
    }

    fn render(data: Vec<u8>, options: ImageOptions) -> ImagePayload {
        let doc = Document::Image(ImageDocument {
            data: data.into(),
            options,
        });
        ImageRenderer::default().validate(&doc).expect("valid");
        match ImageRenderer::default()
            .render(doc, &target())
            .expect("render")
        {
            PrintPayload::Image(p) => p,
            _ => panic!("image payload expected"),
        }
    }

    /// 4x2 image: left half red, right half transparent.
    fn sample() -> DynamicImage {
        DynamicImage::ImageRgba8(ImageBuffer::from_fn(4, 2, |x, _| {
            if x < 2 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        }))
    }

    #[test]
    fn png_decodes_with_transparency_on_white() {
        let p = render(encode(sample(), ImageFormat::Png), ImageOptions::default());
        assert_eq!((p.image.width, p.image.height), (4, 2));
        assert_eq!(&p.image.rgb[..3], &[255, 0, 0]);
        assert_eq!(
            &p.image.rgb[6..9],
            &[255, 255, 255],
            "transparent pixels become paper white"
        );
        assert_eq!((p.image.dpi_x, p.image.dpi_y), (96, 96));
        assert_eq!(p.placement, Placement::Fit);
    }

    #[test]
    fn rotation_swaps_dimensions() {
        let options = ImageOptions {
            rotate: 90,
            dpi: Some(300.0),
            ..ImageOptions::default()
        };
        let p = render(encode(sample(), ImageFormat::Png), options);
        assert_eq!((p.image.width, p.image.height), (2, 4));
        // Rotating clockwise puts the red (left) half at the top.
        assert_eq!(&p.image.rgb[..3], &[255, 0, 0]);
        assert_eq!(p.image.dpi_x, 300);
    }

    #[test]
    fn every_supported_format_decodes() {
        let rgb = DynamicImage::ImageRgb8(sample().into_rgb8());
        for format in [
            ImageFormat::Jpeg,
            ImageFormat::Bmp,
            ImageFormat::Tiff,
            ImageFormat::Gif,
        ] {
            let p = render(encode(rgb.clone(), format), ImageOptions::default());
            assert_eq!((p.image.width, p.image.height), (4, 2), "{format:?}");
        }
    }

    #[test]
    fn png_physical_resolution_is_read() {
        let mut png = encode(sample(), ImageFormat::Png);
        // Insert a pHYs chunk (300 dpi = 11811 px/m) right after IHDR.
        let ppm = 11811u32.to_be_bytes();
        let mut chunk = vec![0, 0, 0, 9];
        chunk.extend_from_slice(b"pHYs");
        chunk.extend_from_slice(&ppm);
        chunk.extend_from_slice(&ppm);
        chunk.push(1);
        chunk.extend_from_slice(&[0, 0, 0, 0]); // CRC is not checked by our reader
        png.splice(33..33, chunk);
        let dpi = metadata_dpi(&png, ImageFormat::Png).expect("dpi");
        assert!((dpi.0 - 300.0).abs() < 0.5);
    }

    #[test]
    fn rejects_unknown_formats_and_bad_options() {
        let doc = Document::Image(ImageDocument {
            data: b"GIF87?not".to_vec().into(),
            options: ImageOptions::default(),
        });
        assert!(ImageRenderer::default().validate(&doc).is_err());
        let doc = Document::Image(ImageDocument {
            data: b"hello".to_vec().into(),
            options: ImageOptions::default(),
        });
        assert_eq!(
            ImageRenderer::default()
                .validate(&doc)
                .expect_err("format")
                .error_code,
            ErrorCode::UnsupportedDocument
        );
        let options = ImageOptions {
            rotate: 45,
            ..ImageOptions::default()
        };
        let doc = Document::Image(ImageDocument {
            data: encode(sample(), ImageFormat::Png).into(),
            options,
        });
        assert!(ImageRenderer::default().validate(&doc).is_err());
    }

    #[test]
    fn decompression_bombs_are_refused() {
        let big = DynamicImage::ImageRgb8(ImageBuffer::new(3000, 3000));
        let data = encode(big, ImageFormat::Png);
        let renderer = ImageRenderer {
            max_pixels: 1_000_000,
        };
        let doc = Document::Image(ImageDocument {
            data: data.into(),
            options: ImageOptions::default(),
        });
        let err = renderer
            .render(doc, &target())
            .expect_err("too many pixels");
        assert_eq!(err.error_code, ErrorCode::PayloadTooLarge);
    }
}
