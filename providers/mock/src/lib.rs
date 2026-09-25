//! A scriptable, in-memory [`PrintProvider`].
//!
//! Used by CI (no printers, no spooler) and by `kiln-agent run --mock` for SDK and UI
//! development. Every submission is recorded byte-for-byte so tests can assert exactly
//! what would have reached the device.

#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::*;
use kiln_core::provider::*;

/// What `submit` does for a printer.
#[derive(Debug, Clone)]
pub enum SubmitBehavior {
    /// Accept into a simulated spooler; `job_state` then follows the printer's script.
    Spool,
    /// Behave like a direct transport (TCP/serial): complete on return.
    Deliver,
    /// Fail with this error.
    Fail(PrintError),
    /// Sleep (simulating a slow driver) and then spool.
    Delay(Duration),
}

#[derive(Debug, Clone)]
pub struct MockPrinter {
    pub name: String,
    pub connection: ConnectionType,
    pub default: bool,
    pub online: bool,
    pub raw: bool,
    pub text: bool,
    pub behavior: SubmitBehavior,
    pub language: Option<String>,
    /// States returned by successive `job_state` calls for each spooled job; the last one
    /// repeats. Defaults to `Pending → Printing → Printed`.
    pub script: Vec<ProviderJobState>,
}

impl MockPrinter {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            connection: ConnectionType::Local,
            default: false,
            online: true,
            raw: true,
            text: true,
            behavior: SubmitBehavior::Spool,
            language: None,
            script: vec![
                ProviderJobState::Pending,
                ProviderJobState::Printing,
                ProviderJobState::Printed,
            ],
        }
    }

    pub fn default_printer(mut self) -> Self {
        self.default = true;
        self
    }

    pub fn connection(mut self, connection: ConnectionType) -> Self {
        self.connection = connection;
        self
    }

    pub fn language(mut self, language: &str) -> Self {
        self.language = Some(language.to_owned());
        self
    }

    pub fn raw_only(mut self) -> Self {
        self.text = false;
        self
    }

    pub fn behavior(mut self, behavior: SubmitBehavior) -> Self {
        self.behavior = behavior;
        self
    }

    pub fn script(mut self, script: Vec<ProviderJobState>) -> Self {
        self.script = script;
        self
    }
}

/// A submission as it would have reached the device.
#[derive(Debug, Clone)]
pub struct Submission {
    pub printer: String,
    pub job_id: JobId,
    pub document_name: String,
    pub copies: u32,
    pub payload: PrintPayload,
    pub spooler_job_id: Option<u64>,
}

#[derive(Debug)]
struct SpooledJob {
    printer: String,
    document_name: String,
    remaining: VecDeque<ProviderJobState>,
    last: ProviderJobState,
}

#[derive(Debug, Default)]
struct State {
    printers: Vec<MockPrinter>,
    next_spooler_id: u64,
    jobs: HashMap<u64, SpooledJob>,
    submissions: Vec<Submission>,
    cancelled: Vec<u64>,
    discovery_error: Option<PrintError>,
    /// Maximum submissions kept for inspection (oldest dropped first).
    record_limit: usize,
}

#[derive(Debug)]
pub struct MockProvider {
    id: String,
    state: Mutex<State>,
}

impl MockProvider {
    pub fn new(printers: Vec<MockPrinter>) -> Self {
        Self::with_id("mock", printers)
    }

    pub fn with_id(id: impl Into<String>, printers: Vec<MockPrinter>) -> Self {
        Self {
            id: id.into(),
            state: Mutex::new(State {
                printers,
                next_spooler_id: 1,
                record_limit: usize::MAX,
                ..State::default()
            }),
        }
    }

    /// Keeps at most `limit` recorded submissions, so long-running demo sessions do not
    /// accumulate every payload in memory.
    pub fn with_record_limit(self, limit: usize) -> Self {
        self.lock().record_limit = limit;
        self
    }

    /// A small fleet covering each printer category, for demos and SDK development.
    pub fn demo() -> Self {
        Self::new(vec![
            MockPrinter::new("Mock Office Laser")
                .default_printer()
                .connection(ConnectionType::Network),
            MockPrinter::new("Mock Zebra ZD421")
                .raw_only()
                .language("ZPL")
                .connection(ConnectionType::Usb),
            MockPrinter::new("Mock ESC/POS Receipt")
                .raw_only()
                .language("ESC/POS")
                .connection(ConnectionType::Usb),
            MockPrinter::new("Mock Epson LQ-590 Dot Matrix")
                .connection(ConnectionType::Local)
                .language("ESC/P"),
            MockPrinter::new("Mock Direct TCP Label")
                .raw_only()
                .language("TSPL")
                .connection(ConnectionType::Network)
                .behavior(SubmitBehavior::Deliver),
        ])
        .with_record_limit(16)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn submissions(&self) -> Vec<Submission> {
        self.lock().submissions.clone()
    }

    pub fn cancelled(&self) -> Vec<u64> {
        self.lock().cancelled.clone()
    }

    pub fn add_printer(&self, printer: MockPrinter) {
        self.lock().printers.push(printer);
    }

    pub fn remove_printer(&self, name: &str) {
        self.lock().printers.retain(|p| p.name != name);
    }

    pub fn set_online(&self, name: &str, online: bool) {
        if let Some(p) = self.lock().printers.iter_mut().find(|p| p.name == name) {
            p.online = online;
        }
    }

    pub fn set_behavior(&self, name: &str, behavior: SubmitBehavior) {
        if let Some(p) = self.lock().printers.iter_mut().find(|p| p.name == name) {
            p.behavior = behavior;
        }
    }

    /// Makes discovery fail until cleared (simulates a spooler outage).
    pub fn set_discovery_error(&self, error: Option<PrintError>) {
        self.lock().discovery_error = error;
    }

    /// Overrides the state a spooled job reports from now on.
    pub fn set_job_state(&self, spooler_job_id: u64, state: ProviderJobState) {
        if let Some(job) = self.lock().jobs.get_mut(&spooler_job_id) {
            job.remaining.clear();
            job.last = state;
        }
    }

    fn to_printer(&self, p: &MockPrinter, queued: u32) -> Printer {
        Printer {
            id: PrinterId::derive(&self.id, &p.name),
            name: p.name.clone(),
            display_name: p.name.clone(),
            provider: self.id.clone(),
            connection: p.connection,
            driver: Some("Kiln Mock Driver".into()),
            port: Some("MOCK:".into()),
            location: None,
            default: p.default,
            online: p.online,
            status: if p.online {
                PrinterState::Ready
            } else {
                PrinterState::Offline
            },
            conditions: if p.online {
                vec![]
            } else {
                vec![PrinterCondition::Offline]
            },
            queued_jobs: Some(queued),
            language: p.language.clone(),
            capabilities: None,
        }
    }

    fn find<'a>(state: &'a State, printer: &Printer) -> Result<&'a MockPrinter> {
        state
            .printers
            .iter()
            .find(|p| p.name == printer.name)
            .ok_or_else(|| {
                PrintError::printer_not_found(&printer.name).with_printer(printer.id.as_str())
            })
    }
}

impl PrinterDiscoveryProvider for MockProvider {
    fn discover(&self) -> Result<Vec<Printer>> {
        let state = self.lock();
        if let Some(err) = &state.discovery_error {
            return Err(err.clone());
        }
        Ok(state
            .printers
            .iter()
            .map(|p| {
                let queued = state.jobs.values().filter(|j| j.printer == p.name).count() as u32;
                self.to_printer(p, queued)
            })
            .collect())
    }
}

impl PrintProvider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, printer: &Printer) -> Result<PrinterCapabilities> {
        let state = self.lock();
        let p = Self::find(&state, printer)?;
        Ok(PrinterCapabilities {
            paper_sizes: p.text.then(|| {
                vec![PaperSize {
                    id: "9".into(),
                    name: "A4".into(),
                    width_mm: 210.0,
                    height_mm: 297.0,
                }]
            }),
            color: Some(false),
            duplex: None,
            resolutions: Some(vec![Resolution {
                x_dpi: 203,
                y_dpi: 203,
            }]),
            max_copies: Some(999),
            collate: None,
            orientations: Some(vec![Orientation::Portrait, Orientation::Landscape]),
            trays: None,
            raw: Some(p.raw),
            datatypes: Some(vec!["RAW".into()]),
            document_types: vec![],
        })
    }

    fn supports(&self, printer: &Printer, kind: PayloadKind) -> bool {
        let state = self.lock();
        Self::find(&state, printer).is_ok_and(|p| match kind {
            PayloadKind::Raw => p.raw,
            // `text` marks a driver-backed printer that accepts graphics.
            PayloadKind::Text | PayloadKind::Pdf | PayloadKind::Image => p.text,
        })
    }

    fn submit(&self, printer: &Printer, spec: &SubmitSpec) -> Result<SubmitOutcome> {
        let behavior = {
            let state = self.lock();
            let p = Self::find(&state, printer)?;
            if !p.online {
                return Err(
                    PrintError::new(ErrorCode::PrinterOffline, "mock printer is offline")
                        .with_printer(printer.id.as_str()),
                );
            }
            p.behavior.clone()
        };
        if let SubmitBehavior::Delay(delay) = behavior {
            std::thread::sleep(delay);
        }
        let mut state = self.lock();
        let record = |state: &mut State, spooler_job_id| {
            if state.submissions.len() >= state.record_limit {
                state.submissions.remove(0);
            }
            state.submissions.push(Submission {
                printer: printer.name.clone(),
                job_id: spec.job_id,
                document_name: spec.document_name.clone(),
                copies: spec.copies,
                payload: spec.payload.clone(),
                spooler_job_id,
            })
        };
        match behavior {
            SubmitBehavior::Fail(err) => Err(err),
            SubmitBehavior::Deliver => {
                record(&mut state, None);
                Ok(SubmitOutcome::Delivered)
            }
            SubmitBehavior::Spool | SubmitBehavior::Delay(_) => {
                let id = state.next_spooler_id;
                state.next_spooler_id += 1;
                let script = Self::find(&state, printer)?.script.clone();
                let mut remaining: VecDeque<_> = script.into();
                let last = remaining.back().cloned().unwrap_or(ProviderJobState::Gone);
                if remaining.len() > 1 {
                    remaining.pop_back();
                } else {
                    remaining.clear();
                }
                state.jobs.insert(
                    id,
                    SpooledJob {
                        printer: printer.name.clone(),
                        document_name: spec.document_name.clone(),
                        remaining,
                        last,
                    },
                );
                record(&mut state, Some(id));
                Ok(SubmitOutcome::Spooled { spooler_job_id: id })
            }
        }
    }

    fn job_state(&self, _printer: &Printer, spooler_job_id: u64) -> Result<ProviderJobState> {
        let mut state = self.lock();
        let Some(job) = state.jobs.get_mut(&spooler_job_id) else {
            return Ok(ProviderJobState::Gone);
        };
        let next = job
            .remaining
            .pop_front()
            .unwrap_or_else(|| job.last.clone());
        if matches!(
            next,
            ProviderJobState::Printed
                | ProviderJobState::Gone
                | ProviderJobState::Cancelled
                | ProviderJobState::Failed(_)
        ) {
            state.jobs.remove(&spooler_job_id);
        }
        Ok(next)
    }

    fn cancel(&self, _printer: &Printer, spooler_job_id: u64) -> Result<()> {
        let mut state = self.lock();
        if state.jobs.remove(&spooler_job_id).is_none() {
            return Err(PrintError::new(
                ErrorCode::InvalidJobState,
                "job is no longer in the spooler",
            ));
        }
        state.cancelled.push(spooler_job_id);
        Ok(())
    }

    fn queue(&self, printer: &Printer) -> Result<Vec<QueueEntry>> {
        let state = self.lock();
        let mut ids: Vec<_> = state
            .jobs
            .iter()
            .filter(|(_, j)| j.printer == printer.name)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        Ok(ids
            .into_iter()
            .enumerate()
            .map(|(pos, id)| QueueEntry {
                spooler_job_id: id,
                document_name: state.jobs.get(&id).map(|j| j.document_name.clone()),
                status: vec![],
                status_text: None,
                position: Some(pos as u32 + 1),
                total_pages: None,
                pages_printed: None,
                size_bytes: None,
                submitted_at: None,
                kiln_job: None,
            })
            .collect())
    }
}
