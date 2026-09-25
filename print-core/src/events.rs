//! Engine events, fanned out to connected clients (filtered by permission) and to logs.

use serde::Serialize;

use crate::model::{Job, Printer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEventKind {
    Created,
    Queued,
    /// The OS spooler accepted the job (`delivery = SPOOLER_ACCEPTED`).
    Spooled,
    Printing,
    /// Non-status change, e.g. a blocking condition appeared or cleared.
    Updated,
    Completed,
    Failed,
    Cancelled,
}

impl JobEventKind {
    /// Wire event name, e.g. `job.completed`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Created => "job.created",
            Self::Queued => "job.queued",
            Self::Spooled => "job.spooled",
            Self::Printing => "job.printing",
            Self::Updated => "job.updated",
            Self::Completed => "job.completed",
            Self::Failed => "job.failed",
            Self::Cancelled => "job.cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterEventKind {
    Connected,
    Disconnected,
    StatusChanged,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EngineEvent {
    Job {
        #[serde(skip)]
        kind: JobEventKind,
        job: Box<Job>,
    },
    Printer {
        #[serde(skip)]
        kind: PrinterEventKind,
        printer: Box<Printer>,
    },
}

impl EngineEvent {
    /// Wire event name, e.g. `job.completed`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Job { kind, .. } => kind.name(),
            Self::Printer { kind, .. } => match kind {
                PrinterEventKind::Connected => "printer.connected",
                PrinterEventKind::Disconnected => "printer.disconnected",
                PrinterEventKind::StatusChanged => "printer.status.changed",
            },
        }
    }
}
