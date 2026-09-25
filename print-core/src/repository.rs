//! Job persistence interface plus an in-memory implementation for tests and ephemeral use.
//!
//! Implementations are synchronous: the reference implementation is embedded SQLite in
//! WAL mode, whose individual statements complete in well under a millisecond.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};

use crate::error::{PrintError, Result};
use crate::model::{Job, JobId, JobStatus, PrinterId};

#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub client_id: Option<String>,
    pub printer_id: Option<PrinterId>,
    /// Empty means any status.
    pub statuses: Vec<JobStatus>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Newest first; capped by the implementation.
    pub limit: usize,
    pub offset: usize,
}

impl JobFilter {
    pub const MAX_LIMIT: usize = 500;

    pub fn effective_limit(&self) -> usize {
        match self.limit {
            0 => 100,
            n => n.min(Self::MAX_LIMIT),
        }
    }

    pub fn matches(&self, job: &Job) -> bool {
        self.client_id.as_ref().is_none_or(|c| &job.client_id == c)
            && self
                .printer_id
                .as_ref()
                .is_none_or(|p| job.printer_id.as_ref() == Some(p))
            && (self.statuses.is_empty() || self.statuses.contains(&job.status))
            && self.since.is_none_or(|s| job.created_at >= s)
            && self.until.is_none_or(|u| job.created_at < u)
    }
}

pub trait JobRepository: Send + Sync {
    fn insert(&self, job: &Job) -> Result<()>;
    fn update(&self, job: &Job) -> Result<()>;
    fn get(&self, job_id: JobId) -> Result<Option<Job>>;
    fn list(&self, filter: &JobFilter) -> Result<Vec<Job>>;
    fn find_by_idempotency_key(&self, client_id: &str, key: &str) -> Result<Option<Job>>;
    /// Jobs that were not terminal when last persisted; used for restart reconciliation.
    fn non_terminal(&self) -> Result<Vec<Job>>;
    /// Deletes terminal jobs created before `cutoff`. Returns the number removed.
    fn purge_terminal_before(&self, cutoff: DateTime<Utc>) -> Result<u64>;
}

#[derive(Debug, Default)]
pub struct InMemoryJobRepository {
    jobs: Mutex<HashMap<JobId, Job>>,
}

impl InMemoryJobRepository {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<JobId, Job>>> {
        self.jobs
            .lock()
            .map_err(|_| PrintError::internal("job repository mutex poisoned"))
    }
}

impl JobRepository for InMemoryJobRepository {
    fn insert(&self, job: &Job) -> Result<()> {
        self.lock()?.insert(job.job_id, job.clone());
        Ok(())
    }

    fn update(&self, job: &Job) -> Result<()> {
        self.lock()?.insert(job.job_id, job.clone());
        Ok(())
    }

    fn get(&self, job_id: JobId) -> Result<Option<Job>> {
        Ok(self.lock()?.get(&job_id).cloned())
    }

    fn list(&self, filter: &JobFilter) -> Result<Vec<Job>> {
        let jobs = self.lock()?;
        let mut matched: Vec<Job> = jobs
            .values()
            .filter(|j| filter.matches(j))
            .cloned()
            .collect();
        matched.sort_by_key(|j| std::cmp::Reverse(j.created_at));
        Ok(matched
            .into_iter()
            .skip(filter.offset)
            .take(filter.effective_limit())
            .collect())
    }

    fn find_by_idempotency_key(&self, client_id: &str, key: &str) -> Result<Option<Job>> {
        Ok(self
            .lock()?
            .values()
            .find(|j| j.client_id == client_id && j.idempotency_key.as_deref() == Some(key))
            .cloned())
    }

    fn non_terminal(&self) -> Result<Vec<Job>> {
        Ok(self
            .lock()?
            .values()
            .filter(|j| !j.status.is_terminal())
            .cloned()
            .collect())
    }

    fn purge_terminal_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let mut jobs = self.lock()?;
        let before = jobs.len();
        jobs.retain(|_, j| !(j.status.is_terminal() && j.created_at < cutoff));
        Ok((before - jobs.len()) as u64)
    }
}
