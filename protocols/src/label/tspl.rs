//! TSPL/TSPL2 encoder (TSC and compatible printers).

use kiln_core::error::Result;
use kiln_core::model::{LabelDocument, LabelElement, QrErrorCorrection, Symbology};

use super::{Dots, barcode_payload, encode_text, pick_font, quarter_turns, single_line};

/// Resident bitmap fonts 1-5 with heights in dots at 203 dpi.
const FONTS: [(&str, u32); 5] = [("1", 12), ("2", 20), ("3", 24), ("4", 32), ("5", 48)];

/// TSPL string content: a double quote is written as `\["]`.
fn quoted(text: &str) -> String {
    single_line(text).replace('"', "\\[\"]")
}

fn mm(value: f32) -> String {
    let s = format!("{value:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

pub fn encode(label: &LabelDocument) -> Result<Vec<u8>> {
    let d = Dots(label);
    let mut out: Vec<u8> = Vec::new();
    let push = |out: &mut Vec<u8>, bytes: &[u8]| {
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
    };
    push(
        &mut out,
        format!("SIZE {} mm,{} mm", mm(label.width_mm), mm(label.height_mm)).as_bytes(),
    );
    if let Some(gap) = label.gap_mm {
        push(&mut out, format!("GAP {} mm,0 mm", mm(gap)).as_bytes());
    }
    if let Some(darkness) = label.darkness {
        push(
            &mut out,
            format!("DENSITY {}", u32::from(darkness) * 15 / 30).as_bytes(),
        );
    }
    if let Some(speed) = label.speed {
        push(&mut out, format!("SPEED {speed}").as_bytes());
    }
    // Text below is encoded as Windows-1252.
    push(&mut out, b"CODEPAGE 1252");
    push(&mut out, b"CLS");
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
                let (font, mult) = match font {
                    Some(f) => (f.as_str(), 1),
                    None => pick_font(&FONTS, d.at_least_one(*height_mm), label.dpi, 10),
                };
                let mut c = format!(
                    "TEXT {},{},\"{font}\",{},{mult},{mult},\"",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    u32::from(quarter_turns(*rotation)) * 90
                )
                .into_bytes();
                c.extend(encode_text(&quoted(text), "windows-1252")?);
                c.push(b'"');
                push(&mut out, &c);
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
                let (kind, wide) = match symbology {
                    Symbology::Code128 => ("128", *module_width),
                    Symbology::Code39 => ("39", module_width * 3),
                    Symbology::Ean13 => ("EAN13", *module_width),
                    Symbology::Ean8 => ("EAN8", *module_width),
                    Symbology::UpcA => ("UPCA", *module_width),
                    Symbology::Itf => ("25", module_width * 3),
                };
                let line = format!(
                    "BARCODE {},{},\"{kind}\",{},{},{},{},{wide},\"{}\"",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    d.at_least_one(*height_mm),
                    u8::from(*human_readable),
                    u32::from(quarter_turns(*rotation)) * 90,
                    module_width,
                    quoted(barcode_payload(*symbology, data))
                );
                push(&mut out, line.as_bytes());
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
                let mut c = format!(
                    "QRCODE {},{},{ecc},{magnification},A,0,\"",
                    d.of(*x_mm),
                    d.of(*y_mm)
                )
                .into_bytes();
                c.extend(encode_text(&quoted(data), "windows-1252")?);
                c.push(b'"');
                push(&mut out, &c);
            }
            LabelElement::DataMatrix {
                x_mm,
                y_mm,
                data,
                module_size,
            } => {
                let (x, y) = (d.of(*x_mm), d.of(*y_mm));
                // The symbol is placed inside this box; bound it by the label.
                let (w, h) = (
                    d.of(label.width_mm).saturating_sub(x),
                    d.of(label.height_mm).saturating_sub(y),
                );
                let mut c = format!("DMATRIX {x},{y},{w},{h},x{module_size},\"").into_bytes();
                c.extend(encode_text(&quoted(data), "windows-1252")?);
                c.push(b'"');
                push(&mut out, &c);
            }
            LabelElement::Box {
                x_mm,
                y_mm,
                width_mm,
                height_mm,
                thickness_mm,
            } => {
                let (x, y) = (d.of(*x_mm), d.of(*y_mm));
                let line = format!(
                    "BOX {x},{y},{},{},{}",
                    x + d.at_least_one(*width_mm),
                    y + d.at_least_one(*height_mm),
                    d.at_least_one(*thickness_mm)
                );
                push(&mut out, line.as_bytes());
            }
            LabelElement::Raw { data } => push(&mut out, data.as_bytes()),
        }
    }
    push(&mut out, b"PRINT 1");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_label() {
        let doc = LabelDocument {
            width_mm: 60.0,
            height_mm: 40.0,
            dpi: 203,
            language: None,
            gap_mm: Some(2.0),
            darkness: Some(16),
            speed: Some(4),
            elements: vec![
                LabelElement::Text {
                    x_mm: 2.5,
                    y_mm: 2.5,
                    text: "12\" pipe".into(),
                    height_mm: 3.0,
                    rotation: 0,
                    font: None,
                },
                LabelElement::Barcode {
                    x_mm: 2.5,
                    y_mm: 10.0,
                    symbology: Symbology::Code39,
                    data: "ABC-1".into(),
                    height_mm: 8.0,
                    module_width: 2,
                    human_readable: false,
                    rotation: 0,
                },
                LabelElement::Qr {
                    x_mm: 40.0,
                    y_mm: 2.5,
                    data: "x".into(),
                    magnification: 4,
                    error_correction: QrErrorCorrection::M,
                },
            ],
        };
        let out = String::from_utf8(encode(&doc).expect("encode")).expect("ascii");
        assert_eq!(
            out,
            "SIZE 60 mm,40 mm\r\nGAP 2 mm,0 mm\r\nDENSITY 8\r\nSPEED 4\r\nCODEPAGE 1252\r\nCLS\r\n\
             TEXT 20,20,\"3\",0,1,1,\"12\\[\"] pipe\"\r\n\
             BARCODE 20,80,\"39\",64,0,0,2,6,\"ABC-1\"\r\n\
             QRCODE 320,20,M,4,A,0,\"x\"\r\n\
             PRINT 1\r\n"
        );
    }
}
