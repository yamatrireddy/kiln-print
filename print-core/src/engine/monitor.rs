//! Tracks a spooled job until the spooler reports a terminal state.
//!
//! Polling uses exponential backoff that resets whenever the state changes, and all polls
//! share a small permit pool, so thousands of spooled jobs cost a bounded amount of work.

use std::sync::Arc;

use tracing::{debug, warn};

use super::{Inner, blocking};
use crate::events::JobEventKind;
use crate::model::{CompletionEvidence, JobId, JobStatus, Printer};
use crate::provider::ProviderJobState;

pub(super) async fn run(inner: Arc<Inner>, job_id: JobId, printer: Printer, spooler_job_id: u64) {
    let Ok(provider) = inner.provider(&printer.provider) else {
        return;
    };
    let base = inner.config.monitor_interval;
    let max = inner.config.monitor_max_interval.max(base);
    let mut interval = base;
    let mut failures: u32 = 0;

    loop {
        tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(interval) => {}
        }
        if !inner.is_active(job_id) {
            return; // cancelled through the API, or settled elsewhere
        }

        let (p, pr) = (provider.clone(), printer.clone());
        let state = match blocking(&inner.monitor_permits, move || {
            p.job_state(&pr, spooler_job_id)
        })
        .await
        {
            Ok(state) => {
                failures = 0;
                state
            }
            Err(err) => {
                // A failing status query says nothing about the print itself; keep the
                // job as-is and keep watching at the slowest rate.
                failures += 1;
                if failures == 1 || failures % 30 == 0 {
                    warn!(target: "kiln::monitor", %job_id, spooler_job_id, failures, error = %err, "job status query failed");
                }
                interval = max;
                continue;
            }
        };

        match apply(&inner, job_id, state) {
            Step::Done => return,
            Step::Changed => interval = base,
            Step::Unchanged => interval = (interval * 3 / 2).min(max),
        }
    }
}

enum Step {
    Done,
    Changed,
    Unchanged,
}

fn apply(inner: &Inner, job_id: JobId, state: ProviderJobState) -> Step {
    debug!(target: "kiln::monitor", %job_id, ?state, "spooler state");
    let mutated = match state {
        ProviderJobState::Pending => inner.mutate(job_id, |j| {
            j.condition.take().map(|_| JobEventKind::Updated)
        }),
        ProviderJobState::Printing => inner.mutate(job_id, |j| {
            let cleared = j.condition.take().is_some();
            if j.status == JobStatus::Queued && j.transition(JobStatus::Printing) {
                Some(JobEventKind::Printing)
            } else {
                cleared.then_some(JobEventKind::Updated)
            }
        }),
        ProviderJobState::Blocked { condition, .. } => inner.mutate(job_id, |j| {
            (j.condition != Some(condition)).then(|| {
                j.condition = Some(condition);
                JobEventKind::Updated
            })
        }),
        ProviderJobState::Printed => inner.mutate(job_id, |j| {
            j.complete(CompletionEvidence::SpoolerReportedPrinted)
                .then_some(JobEventKind::Completed)
        }),
        // A job we are deleting disappears from the queue: that is the cancellation
        // taking effect, not a completed print.
        ProviderJobState::Gone if inner.is_cancelling(job_id) => inner.mutate(job_id, |j| {
            j.transition(JobStatus::Cancelled)
                .then_some(JobEventKind::Cancelled)
        }),
        ProviderJobState::Gone => inner.mutate(job_id, |j| {
            j.complete(CompletionEvidence::SpoolerJobRetired)
                .then_some(JobEventKind::Completed)
        }),
        ProviderJobState::Failed(error) => {
            inner.mutate(job_id, |j| j.fail(error).then_some(JobEventKind::Failed))
        }
        ProviderJobState::Cancelled => inner.mutate(job_id, |j| {
            j.transition(JobStatus::Cancelled).then(|| {
                j.warnings
                    .push("the job was deleted from the OS print queue".into());
                JobEventKind::Cancelled
            })
        }),
    };
    match mutated {
        None => Step::Done,
        Some(m) if m.job.status.is_terminal() => Step::Done,
        Some(m) if m.event.is_some() => Step::Changed,
        Some(_) => Step::Unchanged,
    }
}
