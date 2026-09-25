//! Tests against the real Windows spooler.
//!
//! * Non-ignored tests are read-only (enumeration, capabilities) and safe anywhere.
//! * `#[ignore]` tests submit real spool jobs. They only use virtual printers and redirect
//!   output to a temp file, but they are kept out of the default run like the physical
//!   hardware tests. Run with:
//!
//! ```text
//! cargo test -p kiln-provider-windows --test spooler -- --ignored --test-threads=1
//! ```
//!
//! Environment overrides:
//! * `KILN_TEST_PDF_PRINTER` — GDI target (default `Microsoft Print to PDF`).
//! * `KILN_TEST_RAW_PRINTER` — RAW target; should be a v3 driver such as
//!   "Generic / Text Only" (see `tests/hardware/README.md` for setup).
#![cfg(windows)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bytes::Bytes;
use kiln_core::model::{Printer, TextAlignment, TextOptions};
use kiln_core::provider::*;
use kiln_provider_windows::WindowsPrintProvider;

fn job_id() -> kiln_core::model::JobId {
    uuid::Uuid::new_v4()
}

fn find(provider: &WindowsPrintProvider, name: &str) -> Option<Printer> {
    provider
        .discover()
        .expect("discover")
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
}

fn temp_output(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("kiln-spooler-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn wait_for_file(path: &PathBuf) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                // Give the spooler a moment to finish writing.
                std::thread::sleep(Duration::from_millis(500));
                return std::fs::read(path).expect("read output");
            }
        }
        assert!(
            Instant::now() < deadline,
            "spooler did not produce {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn enumerates_printers_with_stable_unique_ids() {
    let provider = WindowsPrintProvider::new();
    let first = provider.discover().expect("EnumPrinters succeeds");
    let second = provider.discover().expect("EnumPrinters succeeds twice");
    let ids: HashSet<_> = first.iter().map(|p| p.id.clone()).collect();
    assert_eq!(ids.len(), first.len(), "ids are unique");
    assert_eq!(
        first.iter().map(|p| &p.id).collect::<Vec<_>>(),
        second.iter().map(|p| &p.id).collect::<Vec<_>>(),
        "ids are stable"
    );
    assert!(first.iter().filter(|p| p.default).count() <= 1);
    for p in &first {
        assert_eq!(p.provider, "windows");
        println!(
            "{:<40} {:?} online={} status={:?} port={:?}",
            p.name, p.connection, p.online, p.status, p.port
        );
    }
}

#[test]
fn reads_capabilities_and_queues_of_every_printer() {
    let provider = WindowsPrintProvider::new();
    for printer in provider.discover().expect("discover") {
        let caps = provider.capabilities(&printer).expect("capabilities");
        println!(
            "{}: raw={:?} datatypes={:?} papers={}",
            printer.name,
            caps.raw,
            caps.datatypes,
            caps.paper_sizes.as_ref().map_or(0, Vec::len)
        );
        provider.queue(&printer).expect("queue listing");
    }
}

#[test]
fn unknown_printer_maps_to_printer_not_found() {
    let provider = WindowsPrintProvider::new();
    let mut ghost = provider
        .discover()
        .expect("discover")
        .into_iter()
        .next()
        .unwrap_or_else(placeholder_printer);
    ghost.name = "Kiln Printer That Does Not Exist".into();
    let err = provider.job_state(&ghost, 1).expect_err("no such printer");
    assert_eq!(err.error_code, kiln_core::ErrorCode::PrinterNotFound);
}

fn placeholder_printer() -> Printer {
    Printer {
        id: kiln_core::model::PrinterId::from("x"),
        name: String::new(),
        display_name: String::new(),
        provider: "windows".into(),
        connection: kiln_core::model::ConnectionType::Local,
        driver: None,
        port: None,
        location: None,
        default: false,
        online: true,
        status: kiln_core::model::PrinterState::Unknown,
        conditions: vec![],
        queued_jobs: None,
        language: None,
        capabilities: None,
    }
}

#[test]
#[ignore = "submits a real spool job (virtual printer, output to a temp file)"]
fn gdi_text_job_renders_through_the_spooler() {
    let name =
        std::env::var("KILN_TEST_PDF_PRINTER").unwrap_or_else(|_| "Microsoft Print to PDF".into());
    let provider = WindowsPrintProvider::new();
    let Some(printer) = find(&provider, &name) else {
        eprintln!("skipping: printer '{name}' not installed");
        return;
    };
    let output = temp_output("text.pdf");
    let layout = TextLayout {
        pages: vec![
            vec!["Kiln Print — GDI text test".into(), "Übergrößenträger ½ € 日本".into(), String::new(),
                 "A long line that should wrap because it is considerably wider than a sheet of paper at this font size, repeated twice. ".repeat(2)],
            vec!["Second page".into()],
        ],
        font_family: None,
        font_size_pt: 11.0,
        bold: false,
        alignment: TextAlignment::Left,
        margins_mm: TextOptions::default().margins_mm,
        orientation: Some(kiln_core::model::Orientation::Landscape),
        wrap: true,
    };
    let spec = SubmitSpec {
        job_id: job_id(),
        document_name: "Kiln GDI test".into(),
        copies: 2,
        payload: PrintPayload::Text(layout),
    };
    let outcome = provider
        .submit_to_file(&printer, &spec, &output)
        .expect("spooled");
    let SubmitOutcome::Spooled { spooler_job_id } = outcome else {
        panic!("expected spooled")
    };
    assert!(spooler_job_id > 0);
    let pdf = wait_for_file(&output);
    assert!(pdf.starts_with(b"%PDF"), "output is a PDF");
    // Two logical pages x two copies (the long line wraps but still fits on page one).
    let pages = pdf.windows(10).filter(|w| w == b"/Type/Page").count()
        + pdf.windows(11).filter(|w| w == b"/Type /Page").count()
        - pdf.windows(11).filter(|w| w == b"/Type/Pages").count()
        - pdf.windows(12).filter(|w| w == b"/Type /Pages").count();
    assert_eq!(pages, 4, "expected 4 pages in the PDF");
    let _ = std::fs::remove_file(&output);
}

#[test]
#[ignore = "submits a real RAW spool job; needs a v3 driver printer"]
fn raw_job_reaches_the_port_byte_for_byte() {
    let Ok(name) = std::env::var("KILN_TEST_RAW_PRINTER") else {
        eprintln!("skipping: set KILN_TEST_RAW_PRINTER (see tests/hardware/README.md)");
        return;
    };
    let provider = WindowsPrintProvider::new();
    let printer = find(&provider, &name).expect("KILN_TEST_RAW_PRINTER is installed");
    assert!(
        provider.supports(&printer, PayloadKind::Raw),
        "printer must accept RAW"
    );
    let output = temp_output("raw.bin");
    // Every byte value, including NUL, ESC, CR/LF, FF and 0xFF.
    let data: Vec<u8> = (0u8..=255).cycle().take(200_000).collect();
    let spec = SubmitSpec {
        job_id: job_id(),
        document_name: "Kiln RAW test".into(),
        copies: 2,
        payload: PrintPayload::Raw(RawPayload {
            bytes: Bytes::from(data.clone()),
            language: None,
        }),
    };
    provider
        .submit_to_file(&printer, &spec, &output)
        .expect("spooled");
    let written = wait_for_file(&output);
    let expected: Vec<u8> = data.iter().chain(data.iter()).copied().collect();
    assert_eq!(written.len(), expected.len());
    assert!(
        written == expected,
        "spooled bytes differ from the submitted payload"
    );
    let _ = std::fs::remove_file(&output);
}

// ------------------------------------------------------------------ Phase 2

use kiln_core::model::{Align, Orientation, PageSetup, PaperRequest, Placement};

fn pdf_printer(provider: &WindowsPrintProvider) -> Option<Printer> {
    let name =
        std::env::var("KILN_TEST_PDF_PRINTER").unwrap_or_else(|_| "Microsoft Print to PDF".into());
    let printer = find(provider, &name);
    if printer.is_none() {
        eprintln!("skipping: printer '{name}' not installed");
    }
    printer
}

/// A minimal, valid PDF: page 1 Letter portrait, page 2 Letter landscape.
fn two_page_pdf() -> Vec<u8> {
    let content =
        |text: &str| format!("BT /F1 36 Tf 72 400 Td ({text}) Tj ET 0 0 1 rg 72 72 200 100 re f");
    let c1 = content("Kiln page one");
    let c2 = content("Kiln page two");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 7 0 R >> >> >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 792 612] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{c1}\nendstream", c1.len()),
        format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// Page count and MediaBoxes of a PDF written by Microsoft Print to PDF.
fn pdf_pages(pdf: &[u8]) -> (usize, Vec<Vec<f64>>) {
    let text = String::from_utf8_lossy(pdf);
    let count = |pat: &str| text.matches(pat).count();
    let pages =
        count("/Type /Page") + count("/Type/Page") - count("/Type /Pages") - count("/Type/Pages");
    let boxes = text
        .match_indices("/MediaBox")
        .map(|(i, m)| {
            let rest = &text[i + m.len()..];
            let rest = &rest[rest.find('[').map_or(0, |p| p + 1)..];
            rest[..rest.find(']').unwrap_or(0)]
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        })
        .collect();
    (pages, boxes)
}

fn wait_for_terminal(
    provider: &WindowsPrintProvider,
    printer: &Printer,
    id: u64,
) -> ProviderJobState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = provider.job_state(printer, id).expect("job state");
        if matches!(
            state,
            ProviderJobState::Printed
                | ProviderJobState::Gone
                | ProviderJobState::Cancelled
                | ProviderJobState::Failed(_)
        ) {
            return state;
        }
        assert!(Instant::now() < deadline, "job never finished: {state:?}");
        // Deliberately slow polling: the watcher must remember what happened in between.
        std::thread::sleep(Duration::from_millis(1500));
    }
}

fn pdf_spec(bytes: Vec<u8>, pages: Option<&str>, setup: PageSetup, copies: u32) -> SubmitSpec {
    SubmitSpec {
        job_id: job_id(),
        document_name: "Kiln PDF test".into(),
        copies,
        payload: PrintPayload::Pdf(PdfPayload {
            bytes: Bytes::from(bytes),
            pages: pages.map(|p| kiln_core::model::PageRanges::parse(p).expect("range")),
            placement: Placement::ShrinkToFit,
            setup,
            max_dpi: 150,
        }),
    }
}

#[test]
#[ignore = "submits a real spool job (virtual printer, output to a temp file)"]
fn pdf_job_rasterises_through_the_spooler_and_completion_is_observed() {
    let provider = WindowsPrintProvider::new();
    let Some(printer) = pdf_printer(&provider) else {
        return;
    };
    let output = temp_output("pdf.pdf");
    let spec = pdf_spec(two_page_pdf(), None, PageSetup::default(), 2);
    let SubmitOutcome::Spooled { spooler_job_id } = provider
        .submit_to_file(&printer, &spec, &output)
        .expect("spooled")
    else {
        panic!("expected spooled")
    };
    let (pages, _) = pdf_pages(&wait_for_file(&output));
    assert_eq!(pages, 4, "2 pages x 2 copies");
    // Let Windows remove the finished job from the queue. From here on only the
    // change-notification history can tell "printed" apart from "vanished".
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        provider
            .queue(&printer)
            .expect("queue")
            .iter()
            .all(|e| e.spooler_job_id != spooler_job_id),
        "job should have left the queue"
    );
    assert_eq!(
        wait_for_terminal(&provider, &printer, spooler_job_id),
        ProviderJobState::Printed
    );
    let _ = std::fs::remove_file(&output);
}

#[test]
#[ignore = "submits a real spool job (virtual printer, output to a temp file)"]
fn pdf_page_range_and_page_setup_are_applied() {
    let provider = WindowsPrintProvider::new();
    let Some(printer) = pdf_printer(&provider) else {
        return;
    };
    let output = temp_output("pdf-a5.pdf");
    let setup = PageSetup {
        paper_size: Some(PaperRequest::Named("A5".into())),
        orientation: Some(Orientation::Landscape),
        ..PageSetup::default()
    };
    let spec = pdf_spec(two_page_pdf(), Some("2"), setup, 1);
    provider
        .submit_to_file(&printer, &spec, &output)
        .expect("spooled");
    let (pages, boxes) = pdf_pages(&wait_for_file(&output));
    assert_eq!(pages, 1, "only page 2");
    // A5 landscape is 595 x 420 pt.
    assert!(
        boxes
            .iter()
            .any(|b| b.len() == 4 && (b[2] - 595.0).abs() < 3.0 && (b[3] - 420.0).abs() < 3.0),
        "A5 landscape page expected, got {boxes:?}"
    );
    let _ = std::fs::remove_file(&output);
}

#[test]
#[ignore = "submits a real spool job (virtual printer, output to a temp file)"]
fn image_job_prints_through_the_spooler() {
    let provider = WindowsPrintProvider::new();
    let Some(printer) = pdf_printer(&provider) else {
        return;
    };
    let output = temp_output("image.pdf");
    let (w, h) = (300u32, 150u32);
    let rgb = (0..w * h)
        .flat_map(|i| {
            if (i % w) < w / 2 {
                [200, 0, 0]
            } else {
                [0, 0, 200]
            }
        })
        .collect();
    let spec = SubmitSpec {
        job_id: job_id(),
        document_name: "Kiln image test".into(),
        copies: 2,
        payload: PrintPayload::Image(ImagePayload {
            image: std::sync::Arc::new(RasterImage {
                width: w,
                height: h,
                rgb,
                dpi_x: 100,
                dpi_y: 100,
            }),
            placement: Placement::ActualSize,
            align: Align::Center,
            setup: PageSetup::default(),
        }),
    };
    provider
        .submit_to_file(&printer, &spec, &output)
        .expect("spooled");
    let (pages, boxes) = pdf_pages(&wait_for_file(&output));
    assert_eq!(pages, 2);
    assert!(
        boxes.iter().all(|b| b.len() == 4 && b[2] > b[3]),
        "3x1.5 in image auto-selects landscape: {boxes:?}"
    );
    let _ = std::fs::remove_file(&output);
}

#[test]
fn unsupported_paper_is_rejected_before_printing() {
    let provider = WindowsPrintProvider::new();
    let Some(printer) = pdf_printer(&provider) else {
        return;
    };
    let setup = PageSetup {
        paper_size: Some(PaperRequest::Named("Imperial Foolscap".into())),
        ..PageSetup::default()
    };
    let output = temp_output("never.pdf");
    let err = provider
        .submit_to_file(&printer, &pdf_spec(two_page_pdf(), None, setup, 1), &output)
        .expect_err("unknown paper");
    assert_eq!(err.error_code, kiln_core::ErrorCode::InvalidPayload);
    assert!(err.message.contains("available"), "{}", err.message);
    assert!(!output.exists(), "nothing was spooled");
}

#[test]
fn corrupt_pdf_is_rejected_before_printing() {
    let provider = WindowsPrintProvider::new();
    let Some(printer) = pdf_printer(&provider) else {
        return;
    };
    let output = temp_output("never2.pdf");
    let err = provider
        .submit_to_file(
            &printer,
            &pdf_spec(b"%PDF-1.7 garbage".to_vec(), None, PageSetup::default(), 1),
            &output,
        )
        .expect_err("corrupt");
    assert_eq!(err.error_code, kiln_core::ErrorCode::InvalidPayload);
}

#[test]
fn default_paper_is_reported() {
    let provider = WindowsPrintProvider::new();
    let Some(printer) = pdf_printer(&provider) else {
        return;
    };
    let (w, h) = provider.default_paper_mm(&printer).expect("default paper");
    assert!(w > 50.0 && h >= w, "{w} x {h}");
}
