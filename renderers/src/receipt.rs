//! RECEIPT documents: ESC/POS receipts with styled text, columns, barcodes, QR codes,
//! dithered logos, paper cut and cash-drawer kick.

use base64::Engine;
use image::{GrayImage, imageops};
use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{Document, DocumentType, ReceiptDocument, ReceiptItem, TextAlignment};
use kiln_core::provider::{PayloadKind, PrintPayload, RawPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};
use kiln_protocols::encoding::{TextEncoder, canonical_name};
use kiln_protocols::escpos_commands as esc;

const LF: u8 = 0x0A;
/// Rows per `GS v 0` block; keeps each block inside small printer buffers.
const RASTER_BAND_ROWS: u32 = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiptRenderer;

fn base64(data: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| PrintError::invalid_payload(format!("invalid base64 data: {e}")))
}

impl DocumentRenderer for ReceiptRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Receipt
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Raw]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::Receipt(receipt) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a RECEIPT document",
            ));
        };
        receipt.validate()?;
        TextEncoder::for_label(&receipt.code_page)?;
        Ok(())
    }

    fn render(&self, document: Document, _target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Receipt(receipt) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected a RECEIPT document",
            ));
        };
        Ok(PrintPayload::Raw(RawPayload {
            bytes: encode(&receipt)?.into(),
            language: Some("ESC/POS".into()),
        }))
    }
}

pub fn encode(receipt: &ReceiptDocument) -> Result<Vec<u8>> {
    let encoder = TextEncoder::for_label(&receipt.code_page)?;
    let width = usize::from(receipt.width_chars);
    let mut out = esc::INITIALIZE.to_vec();
    if let Some(table) = canonical_name(&receipt.code_page).and_then(esc::code_table_for) {
        out.extend(esc::code_table(table));
    }
    let mut ends_with_cut = false;
    for item in &receipt.items {
        ends_with_cut = matches!(item, ReceiptItem::Cut { .. });
        match item {
            ReceiptItem::Text {
                text,
                align,
                bold,
                underline,
                double_width,
                double_height,
                invert,
                small,
            } => {
                out.extend(esc::align(*align));
                out.extend(esc::small_font(*small));
                out.extend(esc::bold(*bold));
                out.extend(esc::underline(*underline));
                out.extend(esc::size(*double_width, *double_height));
                out.extend(esc::invert(*invert));
                for line in text.replace("\r\n", "\n").split('\n') {
                    out.extend(encoder.encode(line)?);
                    out.push(LF);
                }
                out.extend(esc::invert(false));
                out.extend(esc::size(false, false));
                out.extend(esc::underline(false));
                out.extend(esc::bold(false));
                out.extend(esc::small_font(false));
                out.extend(esc::align(TextAlignment::Left));
            }
            ReceiptItem::Columns { left, right, bold } => {
                out.extend(esc::bold(*bold));
                out.extend(encoder.encode(&columns(left, right, width))?);
                out.push(LF);
                out.extend(esc::bold(false));
            }
            ReceiptItem::Separator { character } => {
                let line: String = std::iter::repeat_n(*character, width).collect();
                let bytes = encoder
                    .encode(&line)
                    .or_else(|_| encoder.encode(&"-".repeat(width)))?;
                out.extend(bytes);
                out.push(LF);
            }
            ReceiptItem::Feed { lines } => out.extend(esc::feed(*lines)),
            ReceiptItem::Barcode {
                symbology,
                data,
                height_dots,
                module_width,
                human_readable,
                align,
            } => {
                out.extend(esc::align(*align));
                let height = (*height_dots).min(255) as u8;
                out.extend(esc::barcode(
                    *symbology,
                    data,
                    height,
                    *module_width,
                    *human_readable,
                ));
                out.push(LF);
                out.extend(esc::align(TextAlignment::Left));
            }
            ReceiptItem::Qr {
                data,
                size,
                error_correction,
                align,
            } => {
                out.extend(esc::align(*align));
                out.extend(esc::qr(data.as_bytes(), *size, *error_correction));
                out.push(LF);
                out.extend(esc::align(TextAlignment::Left));
            }
            ReceiptItem::Image {
                data,
                align,
                max_width_dots,
            } => {
                let max_width =
                    u32::from(max_width_dots.unwrap_or(u16::from(receipt.width_chars) * 12));
                out.extend(esc::align(*align));
                out.extend(raster_image(&base64(data)?, max_width)?);
                out.extend(esc::align(TextAlignment::Left));
            }
            ReceiptItem::Cut {
                partial,
                feed_lines,
            } => out.extend(esc::cut(*partial, *feed_lines)),
            ReceiptItem::Drawer { pin } => out.extend(esc::drawer(*pin)),
            ReceiptItem::Raw { data } => out.extend(base64(data)?),
        }
    }
    if receipt.open_drawer {
        out.extend(esc::drawer(0));
    }
    if receipt.cut && !ends_with_cut {
        out.extend(esc::cut(false, 3));
    }
    Ok(out)
}

/// `left` and `right` on one line of `width` characters; the left side is truncated
/// first so amounts stay readable.
fn columns(left: &str, right: &str, width: usize) -> String {
    let right: String = right.chars().take(width).collect();
    let room = width.saturating_sub(right.chars().count() + 1);
    let left: String = left.chars().take(room).collect();
    let gap = width
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(1);
    format!("{left}{}{right}", " ".repeat(gap))
}

/// Decodes, scales to at most `max_width` dots, dithers (Floyd–Steinberg) and encodes as
/// `GS v 0` raster blocks.
fn raster_image(data: &[u8], max_width: u32) -> Result<Vec<u8>> {
    let image = crate::image::decode_limited(data, 20_000_000)?;
    let gray = crate::image::flatten_on_white_gray(image);
    let gray = if gray.width() > max_width {
        let height = (u64::from(gray.height()) * u64::from(max_width) / u64::from(gray.width()))
            .max(1) as u32;
        imageops::resize(&gray, max_width, height, imageops::FilterType::Triangle)
    } else {
        gray
    };
    let bits = dither(&gray);
    let width_bytes = gray.width().div_ceil(8);
    let mut out = Vec::new();
    let mut row = 0;
    while row < gray.height() {
        let rows = RASTER_BAND_ROWS.min(gray.height() - row);
        let start = (row * width_bytes) as usize;
        let end = ((row + rows) * width_bytes) as usize;
        out.extend(esc::raster(
            width_bytes as u16,
            rows as u16,
            &bits[start..end],
        ));
        row += rows;
    }
    Ok(out)
}

/// Floyd–Steinberg dithering to 1 bit per pixel, MSB first, 1 = black.
pub(crate) fn dither(gray: &GrayImage) -> Vec<u8> {
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    let width_bytes = w.div_ceil(8);
    let mut bits = vec![0u8; width_bytes * h];
    let mut current: Vec<f32> = gray.as_raw()[..w].iter().map(|&v| f32::from(v)).collect();
    let mut next: Vec<f32> = vec![0.0; w];
    for y in 0..h {
        if y + 1 < h {
            next = gray.as_raw()[(y + 1) * w..(y + 2) * w]
                .iter()
                .map(|&v| f32::from(v))
                .collect();
        }
        for x in 0..w {
            let old = current[x];
            let black = old < 128.0;
            if black {
                bits[y * width_bytes + x / 8] |= 0x80 >> (x % 8);
            }
            let error = old - if black { 0.0 } else { 255.0 };
            if x + 1 < w {
                current[x + 1] += error * 7.0 / 16.0;
            }
            if y + 1 < h {
                if x > 0 {
                    next[x - 1] += error * 3.0 / 16.0;
                }
                next[x] += error * 5.0 / 16.0;
                if x + 1 < w {
                    next[x + 1] += error / 16.0;
                }
            }
        }
        std::mem::swap(&mut current, &mut next);
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::model::{QrErrorCorrection, Symbology};

    fn doc(items: Vec<ReceiptItem>) -> ReceiptDocument {
        ReceiptDocument {
            width_chars: 16,
            code_page: "ibm858".into(),
            cut: true,
            open_drawer: false,
            items,
        }
    }

    #[test]
    fn golden_receipt() {
        let bytes = encode(&doc(vec![
            ReceiptItem::Columns {
                left: "Coffee large".into(),
                right: "3.50€".into(),
                bold: false,
            },
            ReceiptItem::Separator { character: '=' },
        ]))
        .expect("encode");
        let mut expected = vec![0x1B, b'@', 0x1B, b't', 19, 0x1B, b'E', 0];
        expected.extend(b"Coffee lar 3.50\xD5\n"); // truncated left, euro in IBM858
        expected.extend([0x1B, b'E', 0]);
        expected.extend(b"================\n");
        expected.extend([0x1D, b'V', 65, 3]); // automatic full cut
        assert_eq!(bytes, expected);
    }

    #[test]
    fn explicit_cut_is_not_doubled_and_drawer_opens() {
        let mut receipt = doc(vec![ReceiptItem::Cut {
            partial: true,
            feed_lines: 2,
        }]);
        receipt.open_drawer = true;
        let bytes = encode(&receipt).expect("encode");
        assert!(
            bytes.ends_with(&[0x1D, b'V', 66, 2, 0x1B, b'p', 0, 25, 250]),
            "{bytes:?}"
        );
        assert_eq!(bytes.windows(2).filter(|w| *w == [0x1D, b'V']).count(), 1);
    }

    #[test]
    fn styled_text_resets_afterwards() {
        let bytes = encode(&doc(vec![ReceiptItem::Text {
            text: "TOTAL".into(),
            align: TextAlignment::Center,
            bold: true,
            underline: false,
            double_width: true,
            double_height: true,
            invert: false,
            small: false,
        }]))
        .expect("encode");
        let text = bytes.windows(5).position(|w| w == b"TOTAL").expect("text");
        assert!(bytes[..text].windows(3).any(|w| w == [0x1D, b'!', 0x11]));
        assert!(bytes[text..].windows(3).any(|w| w == [0x1D, b'!', 0x00]));
        assert!(bytes[text..].windows(3).any(|w| w == [0x1B, b'a', 0]));
    }

    #[test]
    fn barcodes_qr_and_unencodable_text() {
        let bytes = encode(&doc(vec![
            ReceiptItem::Barcode {
                symbology: Symbology::Ean13,
                data: "400638133393".into(),
                height_dots: 60,
                module_width: 2,
                human_readable: true,
                align: TextAlignment::Center,
            },
            ReceiptItem::Qr {
                data: "https://x".into(),
                size: 5,
                error_correction: QrErrorCorrection::L,
                align: TextAlignment::Left,
            },
        ]))
        .expect("encode");
        assert!(bytes.windows(4).any(|w| w == [0x1D, b'k', 67, 12]));
        let err = encode(&doc(vec![ReceiptItem::Text {
            text: "日本".into(),
            align: TextAlignment::Left,
            bold: false,
            underline: false,
            double_width: false,
            double_height: false,
            invert: false,
            small: false,
        }]))
        .expect_err("not in IBM858");
        assert_eq!(err.error_code, ErrorCode::InvalidPayload);
    }

    #[test]
    fn dithering_produces_expected_bits() {
        // Left half black, right half white, 10 px wide: bits 0-4 set in each row.
        let gray = GrayImage::from_fn(10, 2, |x, _| image::Luma([if x < 5 { 0 } else { 255 }]));
        assert_eq!(dither(&gray), vec![0b1111_1000, 0, 0b1111_1000, 0]);
        // 50% grey dithers to roughly half the pixels.
        let grey = GrayImage::from_pixel(64, 64, image::Luma([128]));
        let black: u32 = dither(&grey).iter().map(|b| b.count_ones()).sum();
        assert!((1800..2300).contains(&black), "{black}");
    }

    #[test]
    fn logos_are_scaled_to_paper_width() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(GrayImage::from_pixel(1000, 100, image::Luma([0])))
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("png");
        let bytes = raster_image(&png.into_inner(), 384).expect("raster");
        // GS v 0 m xL xH yL yH: 48 bytes wide (384 dots), 38 rows (100 * 384 / 1000).
        assert_eq!(&bytes[..8], &[0x1D, b'v', b'0', 0, 48, 0, 38, 0]);
        assert_eq!(bytes.len(), 8 + 48 * 38);
    }
}
