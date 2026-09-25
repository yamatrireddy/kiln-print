//! EPL2 encoder.

use kiln_core::error::Result;
use kiln_core::model::{LabelDocument, LabelElement, QrErrorCorrection, Symbology};

use super::{
    Dots, barcode_payload, encode_text, pick_font, quarter_turns, single_line, unsupported,
};

/// Resident fonts 1-5 with heights in dots at 203 dpi.
const FONTS: [(&str, u32); 5] = [("1", 12), ("2", 16), ("3", 20), ("4", 24), ("5", 48)];

/// Quoted EPL data: backslash and quote are escaped, line breaks flattened.
fn quoted(text: &str) -> String {
    single_line(text).replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn encode(label: &LabelDocument) -> Result<Vec<u8>> {
    let d = Dots(label);
    let mut out: Vec<u8> = Vec::new();
    let mut line = |s: &str| {
        out.extend_from_slice(s.as_bytes());
        out.push(b'\n');
    };
    // Leading newline flushes any partial command; N clears the image buffer;
    // I8,0,001 selects the 8-bit DOS 437 character set used for text below.
    line("");
    line("N");
    line("I8,0,001");
    line(&format!("q{}", d.of(label.width_mm)));
    if let Some(gap) = label.gap_mm {
        line(&format!("Q{},{}", d.of(label.height_mm), d.of(gap)));
    }
    if let Some(darkness) = label.darkness {
        line(&format!("D{}", u32::from(darkness) * 15 / 30));
    }
    if let Some(speed) = label.speed {
        line(&format!("S{}", speed.clamp(1, 6)));
    }
    let mut body: Vec<Vec<u8>> = Vec::new();
    for element in &label.elements {
        let command: Vec<u8> = match element {
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
                    None => pick_font(&FONTS, d.at_least_one(*height_mm), label.dpi, 9),
                };
                let mut c = format!(
                    "A{},{},{},{font},{mult},{mult},N,\"",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    quarter_turns(*rotation)
                )
                .into_bytes();
                c.extend(encode_text(&quoted(text), "ibm437")?);
                c.push(b'"');
                c
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
                    Symbology::Code128 => ("1", *module_width),
                    Symbology::Code39 => ("3", module_width * 3),
                    Symbology::Ean13 => ("E30", *module_width),
                    Symbology::Ean8 => ("E80", *module_width),
                    Symbology::UpcA => ("UA0", *module_width),
                    Symbology::Itf => ("2", module_width * 3),
                };
                format!(
                    "B{},{},{},{kind},{},{wide},{},{},\"{}\"",
                    d.of(*x_mm),
                    d.of(*y_mm),
                    quarter_turns(*rotation),
                    module_width,
                    d.at_least_one(*height_mm),
                    if *human_readable { 'B' } else { 'N' },
                    quoted(barcode_payload(*symbology, data))
                )
                .into_bytes()
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
                    "b{},{},Q,s{magnification},e{ecc},\"",
                    d.of(*x_mm),
                    d.of(*y_mm)
                )
                .into_bytes();
                c.extend(encode_text(&quoted(data), "ibm437")?);
                c.push(b'"');
                c
            }
            LabelElement::DataMatrix { .. } => return Err(unsupported("EPL", "Data Matrix")),
            LabelElement::Box {
                x_mm,
                y_mm,
                width_mm,
                height_mm,
                thickness_mm,
            } => {
                let (x, y) = (d.of(*x_mm), d.of(*y_mm));
                format!(
                    "X{x},{y},{},{},{}",
                    d.at_least_one(*thickness_mm),
                    x + d.at_least_one(*width_mm),
                    y + d.at_least_one(*height_mm)
                )
                .into_bytes()
            }
            LabelElement::Raw { data } => data.as_bytes().to_vec(),
        };
        body.push(command);
    }
    for command in body {
        out.extend(command);
        out.push(b'\n');
    }
    out.extend_from_slice(b"P1\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_label() {
        let doc = LabelDocument {
            width_mm: 100.0,
            height_mm: 50.0,
            dpi: 203,
            language: None,
            gap_mm: Some(3.0),
            darkness: None,
            speed: None,
            elements: vec![
                LabelElement::Text {
                    x_mm: 5.0,
                    y_mm: 5.0,
                    text: "Say \"hi\"".into(),
                    height_mm: 6.0,
                    rotation: 0,
                    font: None,
                },
                LabelElement::Barcode {
                    x_mm: 5.0,
                    y_mm: 15.0,
                    symbology: Symbology::Ean13,
                    data: "4006381333931".into(),
                    height_mm: 10.0,
                    module_width: 2,
                    human_readable: true,
                    rotation: 0,
                },
                LabelElement::Box {
                    x_mm: 1.0,
                    y_mm: 1.0,
                    width_mm: 10.0,
                    height_mm: 10.0,
                    thickness_mm: 0.3,
                },
            ],
        };
        let out = String::from_utf8(encode(&doc).expect("encode")).expect("ascii");
        assert_eq!(
            out,
            "\nN\nI8,0,001\nq799\nQ400,24\n\
             A40,40,0,5,1,1,N,\"Say \\\"hi\\\"\"\n\
             B40,120,0,E30,2,2,80,B,\"400638133393\"\n\
             X8,8,2,88,88\n\
             P1\n"
        );
    }

    #[test]
    fn data_matrix_is_reported_unsupported() {
        let doc = LabelDocument {
            width_mm: 50.0,
            height_mm: 50.0,
            dpi: 203,
            language: None,
            gap_mm: None,
            darkness: None,
            speed: None,
            elements: vec![LabelElement::DataMatrix {
                x_mm: 1.0,
                y_mm: 1.0,
                data: "x".into(),
                module_size: 4,
            }],
        };
        assert!(encode(&doc).is_err());
    }
}
