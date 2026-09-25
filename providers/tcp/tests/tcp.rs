//! Direct TCP provider against local fake printers.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use kiln_core::error::ErrorCode;
use kiln_core::model::{PrinterCondition, PrinterState};
use kiln_core::provider::*;
use kiln_provider_tcp::{StatusQuery, TcpPrintProvider, TcpPrinterConfig};

fn spec(data: &'static [u8], copies: u32) -> SubmitSpec {
    SubmitSpec {
        job_id: uuid::Uuid::new_v4(),
        document_name: "test".into(),
        copies,
        payload: PrintPayload::Raw(RawPayload {
            bytes: Bytes::from_static(data),
            language: None,
        }),
    }
}

/// A fake port-9100 printer that records everything it receives.
fn fake_printer() -> (u16, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut received = Vec::new();
        stream.read_to_end(&mut received).expect("read");
        received
    });
    (port, handle)
}

fn provider(config: TcpPrinterConfig) -> (TcpPrintProvider, kiln_core::model::Printer) {
    let provider = TcpPrintProvider::new(vec![config]);
    let printer = provider.discover().expect("discover").remove(0);
    (provider, printer)
}

#[test]
fn delivers_every_byte_and_copy() {
    let (port, received) = fake_printer();
    let mut config = TcpPrinterConfig::new("Dock Zebra", "127.0.0.1", port);
    config.language = Some("ZPL".into());
    let (provider, printer) = provider(config);
    assert_eq!(printer.language.as_deref(), Some("ZPL"));
    assert_eq!(
        printer.port.as_deref(),
        Some(format!("tcp://127.0.0.1:{port}").as_str())
    );

    let data: &'static [u8] = b"^XA^FDbinary\x00\xff\x1b^FS^XZ";
    let outcome = provider
        .submit(&printer, &spec(data, 3))
        .expect("delivered");
    assert_eq!(outcome, SubmitOutcome::Delivered);
    assert_eq!(
        received.join().expect("printer"),
        [data, data, data].concat()
    );
}

#[test]
fn connection_failure_means_nothing_was_sent() {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    // The listener is dropped: connections are refused.
    let mut config = TcpPrinterConfig::new("Gone", "127.0.0.1", port);
    config.connect_timeout = Duration::from_millis(500);
    let (provider, printer) = provider(config);
    let err = provider
        .submit(&printer, &spec(b"x", 1))
        .expect_err("refused");
    assert_eq!(err.error_code, ErrorCode::PrinterOffline);
    assert!(err.recoverable);
    assert_eq!(
        err.details.as_ref().and_then(|d| d["outcome"].as_str()),
        Some("NOT_PRINTED")
    );
}

#[test]
fn non_raw_payloads_and_cancel_are_refused() {
    let (provider, printer) = provider(TcpPrinterConfig::new("P", "127.0.0.1", 9));
    assert!(provider.supports(&printer, PayloadKind::Raw));
    assert!(!provider.supports(&printer, PayloadKind::Pdf));
    assert_eq!(
        provider.cancel(&printer, 1).expect_err("cancel").error_code,
        ErrorCode::UnsupportedOperation
    );
}

/// A fake status server answering one query connection with `reply`.
fn status_server(expect: &'static [u8], reply: &'static [u8]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; expect.len()];
            if stream.read_exact(&mut buf).is_ok() && buf == expect {
                let _ = stream.write_all(reply);
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
    port
}

#[test]
fn zpl_printers_report_real_device_status() {
    let port = status_server(
        b"~HS",
        b"\x02030,1,0,1245,000,0,0,0,000,0,0,0\x03\r\n\x02000,0,0,0,1,2,6,0,00000000,1,000\x03\r\n\x021234,0\x03\r\n",
    );
    let mut config = TcpPrinterConfig::new("Zebra", "127.0.0.1", port);
    config.status_query = StatusQuery::Zpl;
    let (_, printer) = provider(config);
    assert!(printer.online);
    assert_eq!(printer.status, PrinterState::Error);
    assert_eq!(printer.conditions, vec![PrinterCondition::PaperOut]);
}

#[test]
fn escpos_printers_report_real_device_status() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // DLE EOT 1, 2, 4 → online, cover open, paper near end.
            for reply in [0x12u8, 0x16, 0x1E] {
                let mut query = [0u8; 3];
                if stream.read_exact(&mut query).is_err() {
                    return;
                }
                let _ = stream.write_all(&[reply]);
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
    let mut config = TcpPrinterConfig::new("Receipt", "127.0.0.1", port);
    config.status_query = StatusQuery::EscPos;
    let (_, printer) = provider(config);
    assert_eq!(printer.status, PrinterState::Error);
    assert!(printer.conditions.contains(&PrinterCondition::HeadOpen));
    assert!(printer.conditions.contains(&PrinterCondition::PaperLow));
}

#[test]
fn unreachable_status_printers_are_offline() {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let mut config = TcpPrinterConfig::new("Off", "127.0.0.1", port);
    config.status_query = StatusQuery::Zpl;
    let (_, printer) = provider(config);
    assert!(!printer.online);
    assert_eq!(printer.status, PrinterState::Offline);
}
