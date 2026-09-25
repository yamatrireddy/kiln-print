//! ESC/POS command builders (Epson TM series and compatibles).
//!
//! Each function returns the exact bytes of one command, so documents are assembled from
//! auditable pieces and golden-tested byte for byte.

use kiln_core::model::{QrErrorCorrection, Symbology, TextAlignment};

const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;

/// `ESC @`: reset to power-on settings.
pub const INITIALIZE: [u8; 2] = [ESC, b'@'];

pub fn align(alignment: TextAlignment) -> [u8; 3] {
    let n = match alignment {
        TextAlignment::Left => 0,
        TextAlignment::Center => 1,
        TextAlignment::Right => 2,
    };
    [ESC, b'a', n]
}

pub fn bold(on: bool) -> [u8; 3] {
    [ESC, b'E', u8::from(on)]
}

pub fn underline(on: bool) -> [u8; 3] {
    [ESC, b'-', u8::from(on)]
}

/// `GS !`: character width/height multiplier (1x or 2x each).
pub fn size(double_width: bool, double_height: bool) -> [u8; 3] {
    [
        GS,
        b'!',
        (u8::from(double_width) << 4) | u8::from(double_height),
    ]
}

/// `GS B`: white-on-black.
pub fn invert(on: bool) -> [u8; 3] {
    [GS, b'B', u8::from(on)]
}

/// `ESC M`: font A (normal) or B (small).
pub fn small_font(on: bool) -> [u8; 3] {
    [ESC, b'M', u8::from(on)]
}

/// `ESC t`: select the character code table.
pub fn code_table(n: u8) -> [u8; 3] {
    [ESC, b't', n]
}

/// `ESC t` table number for an encoding (Epson standard numbering).
pub fn code_table_for(canonical_encoding: &str) -> Option<u8> {
    Some(match canonical_encoding {
        "IBM437" => 0,
        "IBM850" => 2,
        "windows-1252" => 16,
        "IBM866" => 17,
        "IBM858" => 19,
        _ => return None,
    })
}

/// `ESC d n`: print and feed `n` lines.
pub fn feed(lines: u8) -> [u8; 3] {
    [ESC, b'd', lines]
}

/// `GS V 65/66 n`: feed `n` lines, then full or partial cut.
pub fn cut(partial: bool, feed_lines: u8) -> [u8; 4] {
    [GS, b'V', if partial { 66 } else { 65 }, feed_lines]
}

/// `ESC p m t1 t2`: kick the cash drawer on connector pin 2 (0) or 5 (1).
pub fn drawer(pin: u8) -> [u8; 5] {
    [ESC, b'p', pin.min(1), 25, 250]
}

/// Barcode with height (dots), module width and human-readable text below.
pub fn barcode(symbology: Symbology, data: &str, height: u8, module: u8, hri: bool) -> Vec<u8> {
    let mut out = vec![
        GS,
        b'h',
        height,
        GS,
        b'w',
        module,
        GS,
        b'H',
        if hri { 2 } else { 0 },
    ];
    // Function B (`GS k m n d1..dn`); CODE128 data is prefixed with `{B` (code set B).
    let (m, payload): (u8, Vec<u8>) = match symbology {
        Symbology::UpcA => (65, data.as_bytes().to_vec()),
        Symbology::Ean13 => (67, data.as_bytes().to_vec()),
        Symbology::Ean8 => (68, data.as_bytes().to_vec()),
        Symbology::Code39 => (69, data.as_bytes().to_vec()),
        Symbology::Itf => (70, data.as_bytes().to_vec()),
        Symbology::Code128 => {
            // '{' is the function-code escape in CODE128 data; "{{" is a literal '{'.
            let mut p = b"{B".to_vec();
            p.extend(data.replace('{', "{{").bytes());
            (73, p)
        }
    };
    out.extend([GS, b'k', m, payload.len() as u8]);
    out.extend(payload);
    out
}

/// QR code via `GS ( k`: model 2, module size, error correction, store, print.
pub fn qr(data: &[u8], size: u8, ecc: QrErrorCorrection) -> Vec<u8> {
    let fun = |cn: u8, fn_: u8, params: &[u8]| {
        let len = params.len() + 2;
        let mut v = vec![
            GS,
            b'(',
            b'k',
            (len & 0xFF) as u8,
            (len >> 8) as u8,
            cn,
            fn_,
        ];
        v.extend_from_slice(params);
        v
    };
    let level = match ecc {
        QrErrorCorrection::L => 48,
        QrErrorCorrection::M => 49,
        QrErrorCorrection::Q => 50,
        QrErrorCorrection::H => 51,
    };
    let mut out = fun(49, 65, &[50, 0]); // model 2
    out.extend(fun(49, 67, &[size]));
    out.extend(fun(49, 69, &[level]));
    let mut store = vec![48];
    store.extend_from_slice(data);
    out.extend(fun(49, 80, &store));
    out.extend(fun(49, 81, &[48]));
    out
}

/// `GS v 0`: 1-bit raster image. `bits` holds `height` rows of `width_bytes` bytes,
/// most significant bit first, 1 = black.
pub fn raster(width_bytes: u16, height: u16, bits: &[u8]) -> Vec<u8> {
    let mut out = vec![
        GS,
        b'v',
        b'0',
        0,
        (width_bytes & 0xFF) as u8,
        (width_bytes >> 8) as u8,
        (height & 0xFF) as u8,
        (height >> 8) as u8,
    ];
    out.extend_from_slice(bits);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_commands() {
        assert_eq!(align(TextAlignment::Center), [0x1B, b'a', 1]);
        assert_eq!(size(true, true), [0x1D, b'!', 0x11]);
        assert_eq!(cut(true, 3), [0x1D, b'V', 66, 3]);
        assert_eq!(drawer(1), [0x1B, b'p', 1, 25, 250]);
        assert_eq!(
            barcode(Symbology::Code128, "A{1", 80, 3, true),
            [
                0x1D, b'h', 80, 0x1D, b'w', 3, 0x1D, b'H', 2, 0x1D, b'k', 73, 6, b'{', b'B', b'A',
                b'{', b'{', b'1'
            ]
        );
        let q = qr(b"hi", 6, QrErrorCorrection::M);
        assert_eq!(&q[..9], &[0x1D, b'(', b'k', 4, 0, 49, 65, 50, 0]);
        assert!(q.ends_with(&[0x1D, b'(', b'k', 3, 0, 49, 81, 48]));
        assert_eq!(
            raster(1, 2, &[0x80, 0x01]),
            [0x1D, b'v', b'0', 0, 1, 0, 2, 0, 0x80, 0x01]
        );
        assert_eq!(code_table_for("IBM858"), Some(19));
    }
}
