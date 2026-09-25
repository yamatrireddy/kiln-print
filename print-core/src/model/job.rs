//! Print-job model and lifecycle rules. See `docs/print-job-lifecycle.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{DocumentType, PrinterId};
use crate::error::{ErrorCode, PrintError};

pub type JobId = Uuid;

/// Lifecycle status. Terminal states: `COMPLETED`, `FAILED`, `CANCELLED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobStatus {
    Received,
    Validating,
    /// Waiting in the agent's per-printer queue *or* in the OS spooler — see
    /// [`Job::delivery`] for which.
    Queued,
    Printing,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Allowed forward transitions. Anything else is a bug and is refused.
    pub const fn can_transition_to(self, next: Self) -> bool {
        use JobStatus::*;
        match (self, next) {
            (Completed | Failed | Cancelled, _) => false,
            (_, Failed | Cancelled) => true,
            (Received, Validating) | (Validating, Queued) => true,
            (Queued, Printing | Completed) | (Printing, Completed) => true,
            (a, b) => a as u8 == b as u8,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Received => "RECEIVED",
            Self::Validating => "VALIDATING",
            Self::Queued => "QUEUED",
            Self::Printing => "PRINTING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "RECEIVED" => Self::Received,
            "VALIDATING" => Self::Validating,
            "QUEUED" => Self::Queued,
            "PRINTING" => Self::Printing,
            "COMPLETED" => Self::Completed,
            "FAILED" => Self::Failed,
            "CANCELLED" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// How far the document has travelled towards the device. This is the field that
/// separates "the agent accepted your request" from "the OS spooler owns the job".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DeliveryStage {
    /// Held by the agent; nothing has been sent anywhere yet. Safe to resubmit if lost.
    RequestAccepted,
    /// Being handed to the spooler/device right now. If the agent dies in this stage the
    /// outcome is unknown.
    Submitting,
    /// The OS spooler accepted the complete job and assigned it an id.
    SpoolerAccepted,
    /// Direct transport (TCP/serial) wrote every byte to the device connection.
    DeviceDelivered,
}

impl DeliveryStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestAccepted => "REQUEST_ACCEPTED",
            Self::Submitting => "SUBMITTING",
            Self::SpoolerAccepted => "SPOOLER_ACCEPTED",
            Self::DeviceDelivered => "DEVICE_DELIVERED",
        }
    }
}

/// Why the agent reports `COMPLETED`. None of these is proof that paper came out: most
/// drivers cannot confirm physical output (see `docs/print-job-lifecycle.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompletionEvidence {
    /// The spooler flagged the job as printed (all data sent to the printer).
    SpoolerReportedPrinted,
    /// The job left the spooler queue without an error or deletion being observed.
    SpoolerJobRetired,
    /// Direct transport finished writing all bytes to the device.
    BytesDelivered,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub job_id: JobId,
    pub client_id: String,
    /// Absent only when the request failed before a printer could be resolved.
    pub printer_id: Option<PrinterId>,
    pub printer_name: Option<String>,
    pub document_type: DocumentType,
    pub language: Option<String>,
    pub job_name: Option<String>,
    pub status: JobStatus,
    pub delivery: DeliveryStage,
    pub copies: u32,
    pub size_bytes: u64,
    /// Spooler-assigned id once `delivery` is `SPOOLER_ACCEPTED`.
    pub spooler_job_id: Option<u64>,
    /// Condition currently holding the job (paper out, offline, …). Not a failure: the
    /// spooler keeps the job and resumes when the condition clears.
    pub condition: Option<ErrorCode>,
    pub completion: Option<CompletionEvidence>,
    pub warnings: Vec<String>,
    pub idempotency_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub queued_at: Option<DateTime<Utc>>,
    /// When the spooler/device accepted the job.
    pub submitted_at: Option<DateTime<Utc>>,
    /// When printing was first observed.
    pub started_at: Option<DateTime<Utc>>,
    /// When the job reached a terminal status.
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<PrintError>,
}

impl Job {
    pub fn new(
        client_id: impl Into<String>,
        document_type: DocumentType,
        copies: u32,
        size_bytes: u64,
    ) -> Self {
        let now = Utc::now();
        Self {
            job_id: Uuid::new_v4(),
            client_id: client_id.into(),
            printer_id: None,
            printer_name: None,
            document_type,
            language: None,
            job_name: None,
            status: JobStatus::Received,
            delivery: DeliveryStage::RequestAccepted,
            copies,
            size_bytes,
            spooler_job_id: None,
            condition: None,
            completion: None,
            warnings: Vec::new(),
            idempotency_key: None,
            created_at: now,
            updated_at: now,
            queued_at: None,
            submitted_at: None,
            started_at: None,
            completed_at: None,
            error: None,
        }
    }

    /// Moves to `next` if the lifecycle allows it, stamping the matching timestamp.
    /// Returns `false` (and changes nothing) for an illegal transition.
    pub fn transition(&mut self, next: JobStatus) -> bool {
        if !self.status.can_transition_to(next) {
            return false;
        }
        let now = Utc::now();
        match next {
            JobStatus::Queued if self.queued_at.is_none() => self.queued_at = Some(now),
            JobStatus::Printing if self.started_at.is_none() => self.started_at = Some(now),
            s if s.is_terminal() => {
                self.completed_at = Some(now);
                self.condition = None;
            }
            _ => {}
        }
        self.status = next;
        self.updated_at = now;
        true
    }

    pub fn fail(&mut self, error: PrintError) -> bool {
        let error = error.with_job(self.job_id);
        let error = match &self.printer_id {
            Some(p) if error.printer_id.is_none() => error.with_printer(p.as_str()),
            _ => error,
        };
        if self.transition(JobStatus::Failed) {
            self.error = Some(error);
            true
        } else {
            false
        }
    }

    pub fn complete(&mut self, evidence: CompletionEvidence) -> bool {
        if self.transition(JobStatus::Completed) {
            self.completion = Some(evidence);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job::new("client", DocumentType::Raw, 1, 10)
    }

    #[test]
    fn happy_path_transitions() {
        let mut j = job();
        assert!(j.transition(JobStatus::Validating));
        assert!(j.transition(JobStatus::Queued));
        assert!(j.queued_at.is_some());
        assert!(j.transition(JobStatus::Printing));
        assert!(j.started_at.is_some());
        assert!(j.complete(CompletionEvidence::SpoolerReportedPrinted));
        assert!(j.completed_at.is_some());
    }

    #[test]
    fn terminal_states_are_final() {
        let mut j = job();
        assert!(j.transition(JobStatus::Cancelled));
        assert!(!j.transition(JobStatus::Queued));
        assert!(!j.fail(PrintError::internal("late failure")));
        assert_eq!(j.status, JobStatus::Cancelled);
        assert!(j.error.is_none());
    }

    #[test]
    fn cannot_skip_validation_or_go_backwards() {
        let mut j = job();
        assert!(!j.transition(JobStatus::Queued));
        assert!(!j.transition(JobStatus::Completed));
        j.transition(JobStatus::Validating);
        j.transition(JobStatus::Queued);
        j.transition(JobStatus::Printing);
        assert!(!j.transition(JobStatus::Queued));
    }

    #[test]
    fn failure_is_annotated_with_job_and_printer() {
        let mut j = job();
        j.printer_id = Some(PrinterId::from("p1"));
        j.fail(PrintError::new(ErrorCode::SpoolerError, "x"));
        let err = j.error.expect("error recorded");
        assert_eq!(err.job_id, Some(j.job_id));
        assert_eq!(err.printer_id.as_deref(), Some("p1"));
    }

    #[test]
    fn status_parse_round_trips() {
        for s in [
            JobStatus::Received,
            JobStatus::Printing,
            JobStatus::Cancelled,
        ] {
            assert_eq!(JobStatus::parse(s.as_str()), Some(s));
        }
    }
}
