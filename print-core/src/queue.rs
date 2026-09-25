//! Per-printer ordered queues with bounded depth and a global byte budget.
//!
//! Each printer gets its own FIFO channel drained by exactly one worker task, so jobs for
//! one printer keep their submission order while a slow or jammed printer never delays
//! another. Queues are created lazily; a worker is a lightweight async task, not a thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::mpsc;

use crate::model::{JobId, Printer, PrinterId};
use crate::provider::SubmitSpec;

/// A validated, rendered job waiting for its printer's worker.
#[derive(Debug)]
pub struct QueuedJob {
    pub(crate) job_id: JobId,
    pub(crate) printer: Printer,
    pub(crate) spec: SubmitSpec,
    pub(crate) reservation: ByteReservation,
}

/// Caps the total size of payloads held in memory across all queues. When exceeded,
/// submissions are refused with `QUEUE_FULL` (backpressure) instead of growing memory.
#[derive(Debug)]
pub struct ByteBudget {
    used: AtomicU64,
    limit: u64,
}

impl ByteBudget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicU64::new(0),
            limit,
        })
    }

    pub fn try_reserve(self: &Arc<Self>, bytes: u64) -> Option<ByteReservation> {
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let next = current.checked_add(bytes)?;
            if next > self.limit {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ByteReservation {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }
}

/// Returns its bytes to the budget when dropped (after submission finishes or the job is
/// discarded), so accounting cannot leak on any error path.
#[derive(Debug)]
pub struct ByteReservation {
    budget: Arc<ByteBudget>,
    bytes: u64,
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub struct PrinterQueues {
    capacity: usize,
    senders: Mutex<HashMap<PrinterId, mpsc::Sender<QueuedJob>>>,
}

impl PrinterQueues {
    pub fn new(capacity_per_printer: usize) -> Self {
        Self {
            capacity: capacity_per_printer.max(1),
            senders: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the printer's queue, creating it (and its worker via `spawn`) on first use.
    pub fn sender_for(
        &self,
        printer_id: &PrinterId,
        spawn: impl FnOnce(mpsc::Receiver<QueuedJob>),
    ) -> mpsc::Sender<QueuedJob> {
        let mut senders = self.senders.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = senders.get(printer_id).filter(|s| !s.is_closed()) {
            return sender.clone();
        }
        let (tx, rx) = mpsc::channel(self.capacity);
        spawn(rx);
        senders.insert(printer_id.clone(), tx.clone());
        tx
    }

    /// Jobs waiting in the printer's agent-side queue (not counting the one in flight).
    pub fn depth(&self, printer_id: &PrinterId) -> usize {
        let senders = self.senders.lock().unwrap_or_else(PoisonError::into_inner);
        senders
            .get(printer_id)
            .map_or(0, |s| s.max_capacity() - s.capacity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_rejects_over_limit_and_releases_on_drop() {
        let budget = ByteBudget::new(100);
        let a = budget.try_reserve(60).expect("fits");
        assert!(budget.try_reserve(50).is_none());
        let b = budget.try_reserve(40).expect("fits exactly");
        assert_eq!(budget.used(), 100);
        drop(a);
        assert_eq!(budget.used(), 40);
        drop(b);
        assert_eq!(budget.used(), 0);
    }
}
