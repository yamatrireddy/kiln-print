//! ZPL II encoder.

use std::fmt::Write;

use kiln_core::error::Result;
use kiln_core::model::{LabelDocument, LabelElement, QrErrorCorrection, Symbology};

use super::{Dots, barcode_payload, quarter_turns};

fn orientation(rotation: u16) -> char {
    ['N', 'R', 'I', 'B'][usize::from(quarter_turns(rotation))]
}

/// Field data for `^FH^FD`: `^`, `~` and the escape character `_` become hex escapes,
/// so client text can never terminate the field or start a command.
fn field(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '_' => out.push_str("_5F"),
            '^' => out.push_str("_5E"),
            '~' => out.push_str("_7E"),
            '\r' | '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

pub fn encode(label: &LabelDocument) -> Result<Vec<u8>> {
    let d = Dots(label);
    let mut z = String::new();
    // ^CI28: field data is UTF-8.
    let _ = write!(
        z,
        "^XA\n^CI28\n^PW{}\n^LL{}\n^LH0,0\n",
        d.of(label.width_mm),
        d.of(label.height_mm)
    );
    if let Some(darkness) = label.darkness {
        let _ = writeln!(z, "~SD{darkness:02}");
    }
    if let Some(speed) = label.speed {
        let _ = writeln!(z, "^PR{}", speed.clamp(2, 14));
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
                let h = d.at_least_one(*height_mm);
                let font = font.as_deref().unwrap_or("0");
                let _ = writeln!(
                    z,
                    "^FO{},{}^A{}{},{h},{h}^FH^FD{}^FS",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    font,
                    orientation(*rotation),
                    field(text)
                );
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
                let h = d.at_least_one(*height_mm);
                let o = orientation(*rotation);
                let hr = if *human_readable { 'Y' } else { 'N' };
                let _ = write!(
                    z,
                    "^FO{},{}^BY{},3,{h}",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    module_width
                );
                let command = match symbology {
                    Symbology::Code128 => format!("^BC{o},{h},{hr},N,N"),
                    Symbology::Code39 => format!("^B3{o},N,{h},{hr},N"),
                    Symbology::Ean13 => format!("^BE{o},{h},{hr},N"),
                    Symbology::Ean8 => format!("^B8{o},{h},{hr},N"),
                    Symbology::UpcA => format!("^BU{o},{h},{hr},N,Y"),
                    Symbology::Itf => format!("^B2{o},{h},{hr},N,N"),
                };
                // In ^BC, '>' starts an invocation sequence; ">0" encodes a literal '>'.
                let payload = barcode_payload(*symbology, data);
                let payload = if *symbology == Symbology::Code128 {
                    payload.replace('>', ">0")
                } else {
                    payload.to_owned()
                };
                let _ = writeln!(z, "{command}^FH^FD{}^FS", field(&payload));
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
                let _ = writeln!(
                    z,
                    "^FO{},{}^BQN,2,{magnification}^FH^FD{ecc}A,{}^FS",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    field(data)
                );
            }
            LabelElement::DataMatrix {
                x_mm,
                y_mm,
                data,
                module_size,
            } => {
                let _ = writeln!(
                    z,
                    "^FO{},{}^BXN,{module_size},200^FH^FD{}^FS",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    field(data)
                );
            }
            LabelElement::Box {
                x_mm,
                y_mm,
                width_mm,
                height_mm,
                thickness_mm,
            } => {
                let t = d.at_least_one(*thickness_mm);
                let (w, h) = (
                    d.at_least_one(*width_mm).max(t),
                    d.at_least_one(*height_mm).max(t),
                );
                let _ = writeln!(z, "^FO{},{}^GB{w},{h},{t}^FS", d.of(*x_mm), d.of(*y_mm));
            }
            LabelElement::Raw { data } => {
                z.push_str(data);
                z.push('\n');
            }
        }
    }
    z.push_str("^XZ\n");
    Ok(z.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(elements: Vec<LabelElement>) -> LabelDocument {
        LabelDocument {
            width_mm: 100.0,
            height_mm: 50.0,
            dpi: 203,
            language: None,
            gap_mm: None,
            darkness: Some(20),
            speed: Some(4),
            elements,
        }
    }

    #[test]
    fn golden_label() {
        let doc = label(vec![
            LabelElement::Text {
                x_mm: 5.0,
                y_mm: 5.0,
                text: "Ship to: Jane".into(),
                height_mm: 5.0,
                rotation: 0,
                font: None,
            },
            LabelElement::Barcode {
                x_mm: 5.0,
                y_mm: 15.0,
                symbology: Symbology::Code128,
                data: "AB>12".into(),
                height_mm: 10.0,
                module_width: 2,
                human_readable: true,
                rotation: 90,
            },
            LabelElement::Qr {
                x_mm: 70.0,
                y_mm: 5.0,
                data: "https://x".into(),
                magnification: 4,
                error_correction: QrErrorCorrection::H,
            },
            LabelElement::Box {
                x_mm: 1.0,
                y_mm: 1.0,
                width_mm: 98.0,
                height_mm: 48.0,
                thickness_mm: 0.3,
            },
        ]);
        let out = String::from_utf8(encode(&doc).expect("encode")).expect("utf8");
        assert_eq!(
            out,
            "^XA\n^CI28\n^PW799\n^LL400\n^LH0,0\n~SD20\n^PR4\n\
             ^FO40,40^A0N,40,40^FH^FDShip to: Jane^FS\n\
             ^FO40,120^BY2,3,80^BCR,80,Y,N,N^FH^FDAB>012^FS\n\
             ^FO559,40^BQN,2,4^FH^FDHA,https://x^FS\n\
             ^FO8,8^GB783,384,2^FS\n\
             ^XZ\n"
        );
    }

    #[test]
    fn field_data_cannot_inject_commands() {
        let doc = label(vec![LabelElement::Text {
            x_mm: 1.0,
            y_mm: 1.0,
            text: "^XZ~JR_evil\nline".into(),
            height_mm: 3.0,
            rotation: 0,
            font: None,
        }]);
        let out = String::from_utf8(encode(&doc).expect("encode")).expect("utf8");
        assert!(out.contains("^FD_5EXZ_7EJR_5Fevil line^FS"), "{out}");
        assert_eq!(out.matches("^XZ").count(), 1, "only the real terminator");
    }
}
