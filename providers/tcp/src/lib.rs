//! Direct RAW TCP printing (port 9100 / JetDirect / AppSocket).
//!
//! Separate from the OS spooler by design: printers are **only** those an administrator
//! lists in the agent configuration, so a client can never make the agent open a
//! connection to an arbitrary host. Bytes are written unchanged; the job completes when
//! every byte has been handed to the connection (`BYTES_DELIVERED`).
//!
//! Failure semantics (see ADR 0003): a connection failure means nothing was sent
//! (`NOT_PRINTED`, safe to retry); a failure while writing means the printer may have
//! received part of the data (`UNKNOWN`, never retried).

#![forbid(unsafe_code)]

pub mod status;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{ConnectionType, Printer, PrinterCapabilities, PrinterId, PrinterState};
use kiln_core::provider::{
    PayloadKind, PrintPayload, PrintProvider, PrinterDiscoveryProvider, ProviderJobState,
    QueueEntry, SubmitOutcome, SubmitSpec,
};
use serde_json::json;

pub use status::{DeviceStatus, StatusQuery};

pub const PROVIDER_ID: &str = "tcp";
const STATUS_TTL: Duration = Duration::from_secs(10);
const STATUS_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub struct TcpPrinterConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// Command language hint (`ZPL`, `ESC/POS`, …).
    pub language: Option<String>,
    pub status_query: StatusQuery,
    pub connect_timeout: Duration,
    pub write_timeout: Duration,
}

impl TcpPrinterConfig {
    pub fn new(name: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            port,
            language: None,
            status_query: StatusQuery::None,
            connect_timeout: Duration::from_secs(3),
            write_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Default)]
struct Runtime {
    /// A job is being written; status queries would compete for the port.
    busy: bool,
    status: Option<(Instant, Option<DeviceStatus>)>,
}

#[derive(Debug)]
pub struct TcpPrintProvider {
    printers: Vec<TcpPrinterConfig>,
    runtime: Mutex<HashMap<String, Runtime>>,
}

impl TcpPrintProvider {
    pub fn new(printers: Vec<TcpPrinterConfig>) -> Self {
        Self {
            printers,
            runtime: Mutex::new(HashMap::new()),
        }
    }

    fn config(&self, printer: &Printer) -> Result<&TcpPrinterConfig> {
        self.printers
            .iter()
            .find(|c| c.name == printer.name)
            .ok_or_else(|| {
                PrintError::printer_not_found(&printer.name).with_printer(printer.id.as_str())
            })
    }

    fn set_busy(&self, name: &str, busy: bool) {
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = runtime.entry(name.to_owned()).or_default();
        entry.busy = busy;
        if !busy {
            // A finished job says more about reachability than a stale status.
            entry.status = None;
        }
    }

    /// Cached device status, refreshed when stale and the printer is idle.
    fn device_status(&self, config: &TcpPrinterConfig) -> Option<DeviceStatus> {
        if config.status_query == StatusQuery::None {
            return None;
        }
        {
            let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = runtime.get(&config.name) {
                let fresh = entry
                    .status
                    .as_ref()
                    .is_some_and(|(t, _)| t.elapsed() < STATUS_TTL);
                if entry.busy || fresh {
                    return entry.status.as_ref().and_then(|(_, s)| s.clone());
                }
            }
        }
        let status = query_status(config);
        self.runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(config.name.clone())
            .or_default()
            .status = Some((Instant::now(), status.clone()));
        status
    }
}

fn endpoint(config: &TcpPrinterConfig) -> String {
    format!("{}:{}", config.host, config.port)
}

fn connect(config: &TcpPrinterConfig, timeout: Duration) -> std::io::Result<TcpStream> {
    let addresses: Vec<SocketAddr> = (config.host.as_str(), config.port)
        .to_socket_addrs()?
        .collect();
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "host did not resolve");
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Asks the printer for its status. `None` means unreachable or no valid answer.
fn query_status(config: &TcpPrinterConfig) -> Option<DeviceStatus> {
    let mut stream = connect(config, STATUS_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(STATUS_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(STATUS_TIMEOUT)).ok()?;
    match config.status_query {
        StatusQuery::None => None,
        StatusQuery::Zpl => {
            stream.write_all(b"~HS").ok()?;
            let mut response = Vec::new();
            let mut buf = [0u8; 256];
            // Three STX..ETX strings; stop at the third ETX or the timeout.
            while response.iter().filter(|b| **b == 0x03).count() < 3 {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => response.extend_from_slice(&buf[..n]),
                }
                if response.len() > 4096 {
                    break;
                }
            }
            status::parse_zpl_host_status(&response)
        }
        StatusQuery::EscPos => {
            let mut answers = [0u8; 3];
            for (i, query) in status::ESCPOS_QUERIES.iter().enumerate() {
                stream.write_all(query).ok()?;
                let mut byte = [0u8; 1];
                stream.read_exact(&mut byte).ok()?;
                answers[i] = byte[0];
            }
            status::parse_escpos_status(answers)
        }
    }
}

impl PrinterDiscoveryProvider for TcpPrintProvider {
    fn discover(&self) -> Result<Vec<Printer>> {
        Ok(self
            .printers
            .iter()
            .map(|config| {
                let (online, state, conditions) =
                    match (config.status_query, self.device_status(config)) {
                        // Without a status protocol we only learn reachability by printing.
                        (StatusQuery::None, _) => (true, PrinterState::Unknown, Vec::new()),
                        (_, Some(status)) => (
                            status.state != PrinterState::Offline,
                            status.state,
                            status.conditions,
                        ),
                        (_, None) => (
                            false,
                            PrinterState::Offline,
                            vec![kiln_core::model::PrinterCondition::Offline],
                        ),
                    };
                Printer {
                    id: PrinterId::derive(PROVIDER_ID, &config.name),
                    name: config.name.clone(),
                    display_name: config.name.clone(),
                    provider: PROVIDER_ID.into(),
                    connection: ConnectionType::Network,
                    driver: Some("Direct TCP (RAW)".into()),
                    port: Some(format!("tcp://{}", endpoint(config))),
                    location: None,
                    default: false,
                    online,
                    status: state,
                    conditions,
                    queued_jobs: None,
                    language: config.language.clone(),
                    capabilities: None,
                }
            })
            .collect())
    }
}

impl PrintProvider for TcpPrintProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn capabilities(&self, printer: &Printer) -> Result<PrinterCapabilities> {
        self.config(printer)?;
        Ok(PrinterCapabilities {
            raw: Some(true),
            ..PrinterCapabilities::default()
        })
    }

    fn supports(&self, _printer: &Printer, kind: PayloadKind) -> bool {
        kind == PayloadKind::Raw
    }

    fn submit(&self, printer: &Printer, spec: &SubmitSpec) -> Result<SubmitOutcome> {
        let config = self.config(printer)?;
        let PrintPayload::Raw(payload) = &spec.payload else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                "direct TCP printers accept RAW data only",
            ));
        };
        self.set_busy(&config.name, true);
        let result = deliver(config, &payload.bytes, spec.copies);
        self.set_busy(&config.name, false);
        result.map_err(|e| e.with_printer(printer.id.as_str()))?;
        tracing::info!(
            target: "kiln::tcp",
            printer = %config.name,
            bytes = payload.bytes.len() * spec.copies as usize,
            "delivered to printer"
        );
        Ok(SubmitOutcome::Delivered)
    }

    fn job_state(&self, _printer: &Printer, _spooler_job_id: u64) -> Result<ProviderJobState> {
        // Jobs complete on delivery; there is no queue to ask.
        Ok(ProviderJobState::Gone)
    }

    fn cancel(&self, _printer: &Printer, _spooler_job_id: u64) -> Result<()> {
        Err(PrintError::new(
            ErrorCode::UnsupportedOperation,
            "direct TCP jobs are written immediately and cannot be cancelled afterwards",
        ))
    }

    fn queue(&self, printer: &Printer) -> Result<Vec<QueueEntry>> {
        self.config(printer)?;
        Ok(Vec::new())
    }
}

fn deliver(config: &TcpPrinterConfig, data: &[u8], copies: u32) -> Result<()> {
    let target = endpoint(config);
    let mut stream = connect(config, config.connect_timeout).map_err(|e| {
        PrintError::new(
            ErrorCode::PrinterOffline,
            format!("could not connect to {target}: {e}"),
        )
        .recoverable(true)
        .with_details(json!({ "outcome": "NOT_PRINTED" }))
    })?;
    let partial = |e: std::io::Error| {
        let code = if e.kind() == std::io::ErrorKind::TimedOut
            || e.kind() == std::io::ErrorKind::WouldBlock
        {
            ErrorCode::Timeout
        } else {
            ErrorCode::ConnectionError
        };
        PrintError::new(
            code,
            format!("the connection to {target} failed while sending: {e}"),
        )
        .recoverable(false)
        .with_details(json!({ "outcome": "UNKNOWN" }))
    };
    stream.set_nodelay(true).map_err(partial)?;
    stream
        .set_write_timeout(Some(config.write_timeout))
        .map_err(partial)?;
    for _ in 0..copies {
        stream.write_all(data).map_err(partial)?;
    }
    stream.flush().map_err(partial)?;
    // Half-close so the printer sees end-of-job, then give it a moment to read everything
    // before the socket is dropped (closing early can reset the connection and lose data).
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut sink = [0u8; 512];
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Ok(())
}
