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
