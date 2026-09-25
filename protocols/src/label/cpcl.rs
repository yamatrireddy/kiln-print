//! CPCL encoder (Zebra/Comtec mobile printers).

use kiln_core::error::Result;
use kiln_core::model::{LabelDocument, LabelElement, QrErrorCorrection, Symbology};

use super::{Dots, barcode_payload, encode_text, quarter_turns, single_line, unsupported};

/// Font 7, size 0 is a 24-dot font at 203 dpi; larger text uses SETMAG.
const BASE_FONT_DOTS: u32 = 24;

pub fn encode(label: &LabelDocument) -> Result<Vec<u8>> {
    let d = Dots(label);
    let mut out: Vec<u8> = Vec::new();
    let mut push = |bytes: &[u8]| {
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
    };
    push(format!("! 0 {0} {0} {1} 1", label.dpi, d.of(label.height_mm)).as_bytes());
    push(format!("PAGE-WIDTH {}", d.of(label.width_mm)).as_bytes());
    if let Some(speed) = label.speed {
        push(format!("SPEED {}", speed.min(5)).as_bytes());
    }
    if let Some(darkness) = label.darkness {
        push(format!("CONTRAST {}", u32::from(darkness) * 3 / 30).as_bytes());
    }
    for element in &label.elements {
        match element {
            LabelElement::Text {
                x_mm,
                y_mm,
                text,
                height_mm,
                rotation,
                font,
            } => {
                let command =
                    ["TEXT", "TEXT90", "TEXT180", "TEXT270"][usize::from(quarter_turns(*rotation))];
                let (font, mag) = match font {
                    Some(f) => (f.clone(), 1),
                    None => {
                        let base = (BASE_FONT_DOTS * label.dpi / 203).max(1);
                        (
                            "7 0".to_owned(),
                            ((d.at_least_one(*height_mm) + base / 2) / base).clamp(1, 16),
                        )
                    }
                };
                if mag > 1 {
                    push(format!("SETMAG {mag} {mag}").as_bytes());
                }
                let mut line =
                    format!("{command} {font} {} {} ", d.of(*x_mm), d.of(*y_mm)).into_bytes();
                line.extend(encode_text(&single_line(text), "ibm437")?);
                push(&line);
                if mag > 1 {
                    push(b"SETMAG 0 0");
                }
            }
            LabelElement::Barcode {
                x_mm,
                y_mm,
                symbology,
                data,
                height_mm,
                module_width,
                human_readable,
                rotation,
            } => {
                let command = match quarter_turns(*rotation) {
                    0 => "BARCODE",
                    1 => "VBARCODE",
                    _ => return Err(unsupported("CPCL", "barcode rotation of 180/270 degrees")),
                };
                let kind = match symbology {
                    Symbology::Code128 => "128",
                    Symbology::Code39 => "39",
                    Symbology::Ean13 => "EAN13",
                    Symbology::Ean8 => "EAN8",
                    Symbology::UpcA => "UPCA",
                    Symbology::Itf => "I2OF5",
                };
                if *human_readable {
                    push(b"BARCODE-TEXT 7 0 5");
                }
                let line = format!(
                    "{command} {kind} {module_width} 1 {} {} {} {}",
                    d.at_least_one(*height_mm),
                    d.of(*x_mm),
                    d.of(*y_mm),
                    single_line(barcode_payload(*symbology, data))
                );
                push(line.as_bytes());
                if *human_readable {
                    push(b"BARCODE-TEXT OFF");
                }
            }
            LabelElement::Qr {
                x_mm,
                y_mm,
                data,
                magnification,
                error_correction,
            } => {
                let ecc = match error_correction {
                    QrErrorCorrection::L => 'L',
                    QrErrorCorrection::M => 'M',
                    QrErrorCorrection::Q => 'Q',
                    QrErrorCorrection::H => 'H',
                };
                push(
                    format!(
                        "BARCODE QR {} {} M 2 U {magnification}",
                        d.of(*x_mm),
                        d.of(*y_mm)
                    )
                    .as_bytes(),
                );
                let mut line = format!("{ecc}A,").into_bytes();
                line.extend(encode_text(&single_line(data), "ibm437")?);
                push(&line);
                push(b"ENDQR");
            }
            LabelElement::DataMatrix { .. } => return Err(unsupported("CPCL", "Data Matrix")),
            LabelElement::Box {
                x_mm,
                y_mm,
                width_mm,
                height_mm,
                thickness_mm,
            } => {
                let (x, y) = (d.of(*x_mm), d.of(*y_mm));
                push(
                    format!(
                        "BOX {x} {y} {} {} {}",
                        x + d.at_least_one(*width_mm),
                        y + d.at_least_one(*height_mm),
                        d.at_least_one(*thickness_mm)
                    )
                    .as_bytes(),
                );
            }
            LabelElement::Raw { data } => push(data.as_bytes()),
        }
    }
    push(b"FORM");
    push(b"PRINT");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_label() {
        let doc = LabelDocument {
            width_mm: 50.0,
            height_mm: 30.0,
            dpi: 203,
            language: None,
            gap_mm: None,
            darkness: None,
            speed: None,
            elements: vec![
                LabelElement::Text {
                    x_mm: 2.0,
                    y_mm: 2.0,
                    text: "Big".into(),
                    height_mm: 6.0,
                    rotation: 0,
                    font: None,
                },
                LabelElement::Barcode {
                    x_mm: 2.0,
                    y_mm: 12.0,
                    symbology: Symbology::Code128,
                    data: "A1".into(),
                    height_mm: 5.0,
                    module_width: 1,
                    human_readable: false,
                    rotation: 0,
                },
                LabelElement::Qr {
                    x_mm: 30.0,
                    y_mm: 2.0,
                    data: "q".into(),
                    magnification: 3,
                    error_correction: QrErrorCorrection::L,
                },
            ],
        };
        let out = String::from_utf8(encode(&doc).expect("encode")).expect("ascii");
        assert_eq!(
            out,
            "! 0 203 203 240 1\r\nPAGE-WIDTH 400\r\n\
             SETMAG 2 2\r\nTEXT 7 0 16 16 Big\r\nSETMAG 0 0\r\n\
             BARCODE 128 1 1 40 16 96 A1\r\n\
             BARCODE QR 240 16 M 2 U 3\r\nLA,q\r\nENDQR\r\n\
             FORM\r\nPRINT\r\n"
        );
    }

    #[test]
    fn upside_down_barcodes_are_refused() {
        let doc = LabelDocument {
            width_mm: 50.0,
            height_mm: 30.0,
            dpi: 203,
            language: None,
            gap_mm: None,
            darkness: None,
            speed: None,
            elements: vec![LabelElement::Barcode {
                x_mm: 2.0,
                y_mm: 2.0,
                symbology: Symbology::Code128,
                data: "A".into(),
                height_mm: 5.0,
                module_width: 1,
                human_readable: false,
                rotation: 180,
            }],
        };
        assert!(encode(&doc).is_err());
    }
}
