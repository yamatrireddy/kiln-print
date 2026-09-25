//! Per-printer submission worker: drains one printer's queue strictly in order.

use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use super::{Inner, monitor};
use crate::error::{ErrorCode, PrintError};
use crate::events::JobEventKind;
use crate::model::{CompletionEvidence, DeliveryStage, JobId, Printer, PrinterId};
use crate::provider::SubmitOutcome;
use crate::queue::QueuedJob;

pub(super) async fn run(
    inner: Arc<Inner>,
    printer_id: PrinterId,
    mut rx: mpsc::Receiver<QueuedJob>,
) {
    debug!(target: "kiln::queue", %printer_id, "printer worker started");
    loop {
        let item = tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => break,
            item = rx.recv() => match item {
                Some(item) => item,
                None => break,
            },
        };
        process(&inner, item).await;
    }
    debug!(target: "kiln::queue", %printer_id, "printer worker stopped");
}

async fn process(inner: &Arc<Inner>, item: QueuedJob) {
    let QueuedJob {
        job_id,
        printer,
        spec,
        reservation,
    } = item;

    // Claim the job. If it was cancelled while queued it is no longer active and we
    // drop it here without touching the printer.
    let claimed = inner.mutate(job_id, |j| {
        j.delivery = DeliveryStage::Submitting;
        None
    });
    if claimed.is_none() {
        debug!(target: "kiln::queue", %job_id, "skipping job that is no longer active");
        return;
    }

    // Prefer the latest discovery snapshot (driver/port may have changed since queueing).
    let printer = inner.cached_printer(&printer.id).unwrap_or(printer);
    let provider = match inner.provider(&printer.provider) {
        Ok(p) => p,
        Err(err) => return fail(inner, job_id, err),
    };
    let permit = match inner.provider_permits.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => return fail(inner, job_id, PrintError::internal("provider pool closed")),
    };

    let submit_printer = printer.clone();
    let mut handle = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // Payload memory is released as soon as the provider is done with it.
        let _reservation = reservation;
        provider.submit(&submit_printer, &spec)
    });

    let timeout = inner.config.submit_timeout;
    let result = tokio::select! {
        r = &mut handle => Some(r),
        _ = tokio::time::sleep(timeout) => None,
    };

    match result {
        Some(Ok(Ok(outcome))) => submitted(inner, job_id, printer, outcome),
        // Providers set `recoverable` themselves: only they know whether any byte left.
        Some(Ok(Err(err))) => fail(inner, job_id, err),
        Some(Err(join_err)) => fail(
            inner,
            job_id,
            PrintError::internal(format!("provider panicked during submission: {join_err}"))
                .with_details(json!({ "outcome": "UNKNOWN" })),
        ),
        None => {
            fail(
                inner,
                job_id,
                PrintError::new(
                    ErrorCode::Timeout,
                    format!(
                        "the printer did not accept the job within {}s; it may still print",
                        timeout.as_secs()
                    ),
                )
                .recoverable(false)
                .with_details(json!({ "outcome": "UNKNOWN", "timeoutSeconds": timeout.as_secs() })),
            );
            // Keep this printer's output ordered: never start the next job while the
            // previous submission is still writing to the device.
            match handle.await {
                Ok(Ok(outcome)) => warn!(
                    target: "kiln::queue", %job_id, ?outcome,
                    "submission completed after its timeout; the job may have printed"
                ),
                Ok(Err(err)) => {
                    warn!(target: "kiln::queue", %job_id, error = %err, "late submission failure")
                }
                Err(err) => {
                    error!(target: "kiln::queue", %job_id, error = %err, "late submission panic")
                }
            }
        }
    }
}

fn submitted(inner: &Arc<Inner>, job_id: JobId, printer: Printer, outcome: SubmitOutcome) {
    match outcome {
        SubmitOutcome::Delivered => {
            inner.mutate(job_id, |j| {
                j.delivery = DeliveryStage::DeviceDelivered;
                j.submitted_at = Some(Utc::now());
                j.complete(CompletionEvidence::BytesDelivered)
                    .then_some(JobEventKind::Completed)
            });
        }
        SubmitOutcome::Spooled { spooler_job_id } => {
            inner.mutate(job_id, |j| {
                j.delivery = DeliveryStage::SpoolerAccepted;
                j.spooler_job_id = Some(spooler_job_id);
                j.submitted_at = Some(Utc::now());
                Some(JobEventKind::Spooled)
            });
            inner
                .tasks
                .spawn(monitor::run(inner.clone(), job_id, printer, spooler_job_id));
        }
    }
}

fn fail(inner: &Inner, job_id: JobId, error: PrintError) {
    inner.mutate(job_id, |j| j.fail(error).then_some(JobEventKind::Failed));
}
