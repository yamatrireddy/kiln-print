//! HTML rendering through a real headless browser. Skipped (with a message) when no
//! Edge/Chrome/Chromium is installed.

use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kiln_core::model::*;
use kiln_core::provider::{PayloadKind, PrintPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};
use kiln_renderers::html::{HtmlConfig, HtmlRenderer, find_browser};

fn target() -> RenderTarget {
    RenderTarget {
        printer: Printer {
            id: "t".into(),
            name: "t".into(),
            display_name: "t".into(),
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
        accepted: vec![PayloadKind::Pdf],
        default_paper_mm: Some((215.9, 279.4)),
    }
}

/// Chrome's sandbox often cannot start on CI Linux runners (restricted user namespaces),
/// and the agent never disables it, so Linux runs these tests only on request.
fn browser_tests_enabled() -> bool {
    if cfg!(target_os = "linux") && std::env::var_os("KILN_HTML_TESTS").is_none() {
        eprintln!("skipping: set KILN_HTML_TESTS=1 to run browser tests on Linux");
        return false;
    }
    if find_browser().is_none() {
        eprintln!("skipping: no Edge/Chrome/Chromium installed");
        return false;
    }
    true
}

fn render(html: &str, options: HtmlOptions) -> Option<Vec<u8>> {
    if !browser_tests_enabled() {
        return None;
    }
    let renderer = HtmlRenderer::new(HtmlConfig {
        timeout: Duration::from_secs(60),
        ..HtmlConfig::default()
    });
    let doc = Document::Html(HtmlDocument {
        html: html.into(),
        options,
    });
    renderer.validate(&doc).expect("valid");
    match renderer.render(doc, &target()).expect("rendered") {
        PrintPayload::Pdf(pdf) => {
            assert_eq!(pdf.placement, Placement::ActualSize);
            Some(pdf.bytes.to_vec())
        }
        other => panic!("expected PDF, got {other:?}"),
    }
}

fn page_count(pdf: &[u8]) -> usize {
    let count = |pat: &[u8]| pdf.windows(pat.len()).filter(|w| *w == pat).count();
    count(b"/Type /Page") + count(b"/Type/Page") - count(b"/Type /Pages") - count(b"/Type/Pages")
}

#[test]
fn renders_css_pages_and_page_breaks() {
    let html = r#"<!doctype html><html><head><style>
        @page { size: A5; margin: 12mm; }
        body { font-family: sans-serif; }
        h1 { color: #036; }
        .break { break-before: page; }
    </style></head><body>
        <h1>Kiln Print</h1><p>Page one.</p>
        <svg width="120" height="40"><rect width="120" height="40" fill="black"/></svg>
        <div class="break"><p>Page two.</p></div>
    </body></html>"#;
    let Some(pdf) = render(html, HtmlOptions::default()) else {
        return;
    };
    assert!(pdf.starts_with(b"%PDF"));
    assert_eq!(page_count(&pdf), 2);
    // A5 is 419.53 x 595.28 pt.
    // A5 is 419.53 x 595.28 pt; Chrome rounds to whole CSS pixels.
    let text = String::from_utf8_lossy(&pdf);
    let boxes: Vec<Vec<f64>> = text
        .match_indices("/MediaBox [")
        .map(|(i, m)| {
            let rest = &text[i + m.len()..];
            rest[..rest.find(']').unwrap_or(0)]
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        })
        .collect();
    assert!(
        boxes
            .iter()
            .all(|b| b.len() == 4 && (b[2] - 419.53).abs() < 1.0 && (b[3] - 595.28).abs() < 1.0),
        "A5 page size from @page is honoured: {boxes:?}"
    );
}

#[test]
fn headers_footers_and_page_ranges() {
    let html =
        "<p>one</p><p style='break-before:page'>two</p><p style='break-before:page'>three</p>";
    let options = HtmlOptions {
        page_range: Some("2-3".into()),
        footer_html: Some(
            "<div style='font-size:8px'>Page <span class=pageNumber></span></div>".into(),
        ),
        prefer_css_page_size: false,
        paper_size: Some(PaperRequest::Named("Letter".into())),
        ..HtmlOptions::default()
    };
    let Some(pdf) = render(html, options) else {
        return;
    };
    assert_eq!(page_count(&pdf), 2);
}

#[test]
fn documents_cannot_reach_the_network_or_loopback() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    let watcher = std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            if let Ok((mut s, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let html = format!(
        r#"<html><head>
            <link rel="stylesheet" href="http://127.0.0.1:{port}/css">
            <style>@font-face {{ font-family: x; src: url(http://localhost:{port}/font); }}</style>
        </head><body style="font-family:x">
            <img src="http://127.0.0.1:{port}/img">
            <img src="https://example.com/remote.png">
            <iframe src="http://127.0.0.1:{port}/frame"></iframe>
            <script>fetch("http://127.0.0.1:{port}/js")</script>
            <p>isolated</p>
        </body></html>"#
    );
    let rendered = render(&html, HtmlOptions::default());
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    watcher.join().expect("watcher");
    if rendered.is_some() {
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the HTML document reached a local port"
        );
    }
}
