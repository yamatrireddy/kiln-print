//! The print engine: job manager, per-printer queues, spooler monitoring and discovery.
//!
//! ```text
//! submit() ─► RECEIVED ─► VALIDATING ─► render ─► QUEUED ──(per-printer FIFO)──► worker
//!                                                                         │
//!             provider.submit() on bounded blocking pool ◄────────────────┘
//!                     │
//!                     ├─ Spooled  ─► delivery=SPOOLER_ACCEPTED ─► monitor ─► PRINTING ─► COMPLETED/FAILED/CANCELLED
//!                     └─ Delivered ─► delivery=DEVICE_DELIVERED ─► COMPLETED (BYTES_DELIVERED)
//! ```
//!
//! All job mutations go through [`Inner::mutate`], which serialises changes, enforces the
//! lifecycle rules, persists the job and emits the event — in that order.

mod discovery;
mod monitor;
mod worker;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Semaphore, broadcast};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

use crate::error::{ErrorCode, PrintError, Result};
use crate::events::{EngineEvent, JobEventKind};
use crate::model::*;
use crate::protocol::{LanguageInfo, PrinterProtocol, ProtocolRegistry};
use crate::provider::{PayloadKind, PrintPayload, PrintProvider, QueueEntry, SubmitSpec};
use crate::queue::{ByteBudget, PrinterQueues, QueuedJob};
use crate::renderer::{DocumentRenderer, RenderTarget, RendererRegistry};
use crate::repository::{JobFilter, JobRepository};

/// Engine tuning. Every limit exists to keep memory and threads bounded under load.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Maximum jobs waiting in one printer's agent-side queue.
    pub queue_capacity_per_printer: usize,
    /// Maximum total payload bytes held in agent queues across all printers.
    pub max_queued_bytes: u64,
    /// Maximum decoded size of a single document.
    pub max_document_bytes: u64,
    pub max_copies: u32,
    /// Concurrent blocking provider submissions/cancellations across all printers.
    pub provider_concurrency: usize,
    /// Concurrent status polls and discovery calls.
    pub monitor_concurrency: usize,
    /// Concurrent document renders.
    pub render_concurrency: usize,
    /// Watchdog for a single provider submission. On expiry the job fails with `TIMEOUT`
    /// and outcome `UNKNOWN`; it is never retried.
    pub submit_timeout: Duration,
    pub monitor_interval: Duration,
    pub monitor_max_interval: Duration,
    pub discovery_interval: Duration,
    /// Minimum gap between on-demand discovery refreshes (e.g. for unknown printer names).
    pub discovery_min_gap: Duration,
    /// Treat printer-language inspection warnings as validation errors.
    pub strict_languages: bool,
    /// Delete terminal jobs older than this. `None` keeps history forever.
    pub job_retention: Option<Duration>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            queue_capacity_per_printer: 128,
            max_queued_bytes: 512 * 1024 * 1024,
            max_document_bytes: 64 * 1024 * 1024,
            max_copies: 999,
            provider_concurrency: 4,
            monitor_concurrency: 4,
            render_concurrency: 2,
            submit_timeout: Duration::from_secs(120),
            monitor_interval: Duration::from_millis(1000),
            monitor_max_interval: Duration::from_secs(10),
            discovery_interval: Duration::from_secs(5),
            discovery_min_gap: Duration::from_secs(2),
            strict_languages: false,
            job_retention: Some(Duration::from_secs(30 * 24 * 3600)),
        }
    }
}

/// Which printers a client may use. Entries match a printer id or, case-insensitively,
/// its native name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrinterScope {
    All,
    Only(Vec<String>),
}

impl PrinterScope {
    pub fn allows(&self, printer: &Printer) -> bool {
        match self {
            Self::All => true,
            Self::Only(entries) => entries
                .iter()
                .any(|e| e == printer.id.as_str() || e.eq_ignore_ascii_case(&printer.name)),
        }
    }
}

/// The authenticated identity a job is submitted under.
#[derive(Debug, Clone)]
pub struct Submitter {
    pub client_id: String,
    pub printers: PrinterScope,
}

/// Per-printer view of the agent queue and the OS spooler queue.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrinterQueueSnapshot {
    pub printer_id: PrinterId,
    pub printer_name: String,
    /// Jobs still held by the agent (not yet handed to the spooler).
    pub agent_queue: Vec<Job>,
    /// Entries in the OS queue, including jobs from other applications.
    pub spooler_queue: Vec<QueueEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    pub printer_id: PrinterId,
    pub printer_name: String,
    pub online: bool,
    pub status: PrinterState,
    /// Jobs waiting in the agent-side queue.
    pub agent_queued: usize,
    /// Agent jobs currently in the spooler (queued or printing).
    pub spooler_active: usize,
    /// Total OS queue length reported at the last discovery, if known.
    pub spooler_total: Option<u32>,
}

#[derive(Default)]
pub struct EngineBuilder {
    config: EngineConfig,
    providers: Vec<Arc<dyn PrintProvider>>,
    renderers: RendererRegistry,
    protocols: ProtocolRegistry,
    repository: Option<Arc<dyn JobRepository>>,
}

impl std::fmt::Debug for EngineBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBuilder")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl EngineBuilder {
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    pub fn provider(mut self, provider: Arc<dyn PrintProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    pub fn renderer(mut self, renderer: Arc<dyn DocumentRenderer>) -> Self {
        self.renderers.register(renderer);
        self
    }

    pub fn protocol(mut self, protocol: Arc<dyn PrinterProtocol>) -> Self {
        self.protocols.register(protocol);
        self
    }

    pub fn repository(mut self, repository: Arc<dyn JobRepository>) -> Self {
        self.repository = Some(repository);
        self
    }

    pub fn build(self) -> Result<PrintEngine> {
        let repo = self
            .repository
            .ok_or_else(|| PrintError::internal("engine built without a job repository"))?;
        let mut providers = HashMap::new();
        for provider in self.providers {
            let id = provider.id().to_owned();
            if providers.insert(id.clone(), provider).is_some() {
                return Err(PrintError::internal(format!(
                    "duplicate provider id '{id}'"
                )));
            }
        }
        let cfg = self.config;
        let (events, _) = broadcast::channel(1024);
        Ok(PrintEngine {
            inner: Arc::new(Inner {
                providers,
                renderers: self.renderers,
                protocols: self.protocols,
                repo,
                events,
                printers: RwLock::new(PrinterCache::default()),
                refresh_lock: tokio::sync::Mutex::new(()),
                active: Mutex::new(HashMap::new()),
                submit_gate: Mutex::new(()),
                cancelling: Mutex::new(Default::default()),
                queues: PrinterQueues::new(cfg.queue_capacity_per_printer),
                budget: ByteBudget::new(cfg.max_queued_bytes),
                provider_permits: Arc::new(Semaphore::new(cfg.provider_concurrency.max(1))),
                monitor_permits: Arc::new(Semaphore::new(cfg.monitor_concurrency.max(1))),
                render_permits: Arc::new(Semaphore::new(cfg.render_concurrency.max(1))),
                shutdown: CancellationToken::new(),
                tasks: TaskTracker::new(),
                config: cfg,
            }),
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct PrinterCache {
    by_id: BTreeMap<PrinterId, Printer>,
    last_refresh: Option<Instant>,
}

pub(crate) struct Inner {
    config: EngineConfig,
    providers: HashMap<String, Arc<dyn PrintProvider>>,
    renderers: RendererRegistry,
    protocols: ProtocolRegistry,
    repo: Arc<dyn JobRepository>,
    events: broadcast::Sender<EngineEvent>,
    printers: RwLock<PrinterCache>,
    refresh_lock: tokio::sync::Mutex<()>,
    /// Non-terminal jobs; the authoritative in-memory copy.
    active: Mutex<HashMap<JobId, Job>>,
    /// Serialises idempotency-key lookup + insert.
    submit_gate: Mutex<()>,
    /// Spooled jobs with a cancellation in flight: if the monitor sees them vanish from
    /// the spooler, that is our deletion, not a completed print.
    cancelling: Mutex<std::collections::HashSet<JobId>>,
    queues: PrinterQueues,
    budget: Arc<ByteBudget>,
    provider_permits: Arc<Semaphore>,
    monitor_permits: Arc<Semaphore>,
    render_permits: Arc<Semaphore>,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

pub(crate) struct Mutated {
    pub job: Job,
    pub event: Option<JobEventKind>,
}

impl Inner {
    /// Applies `change` to an active job, then persists and publishes it. Returns `None`
    /// if the job is not active (unknown or already terminal), in which case nothing runs.
    pub(crate) fn mutate(
        &self,
        job_id: JobId,
        change: impl FnOnce(&mut Job) -> Option<JobEventKind>,
    ) -> Option<Mutated> {
        let (job, event, changed) = {
            let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            let job = active.get_mut(&job_id)?;
            let before = job.clone();
            let event = change(job);
            let changed = *job != before;
            if changed {
                job.updated_at = Utc::now();
                // Persist while holding the lock so the stored order matches the
                // in-memory order. A storage failure must not stop a physical print that
                // is already in progress, so it is logged rather than propagated.
                if let Err(err) = self.repo.update(job) {
                    warn!(target: "kiln::jobs", %job_id, error = %err, "failed to persist job update");
                }
            }
            let snapshot = job.clone();
            if snapshot.status.is_terminal() {
                active.remove(&job_id);
            }
            (snapshot, event, changed)
        };
        if changed {
            log_job(&job, event);
        }
        if let Some(kind) = event {
            let _ = self.events.send(EngineEvent::Job {
                kind,
                job: Box::new(job.clone()),
            });
        }
        Some(Mutated { job, event })
    }

    fn track(&self, job: &Job) -> Result<()> {
        self.repo.insert(job)?;
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job.job_id, job.clone());
        log_job(job, Some(JobEventKind::Created));
        let _ = self.events.send(EngineEvent::Job {
            kind: JobEventKind::Created,
            job: Box::new(job.clone()),
        });
        Ok(())
    }

    pub(crate) fn is_cancelling(&self, job_id: JobId) -> bool {
        self.cancelling
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&job_id)
    }

    pub(crate) fn is_active(&self, job_id: JobId) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&job_id)
    }

    fn active_job(&self, job_id: JobId) -> Option<Job> {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&job_id)
            .cloned()
    }

    fn active_jobs(&self, pred: impl Fn(&Job) -> bool) -> Vec<Job> {
        let active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let mut jobs: Vec<Job> = active.values().filter(|j| pred(j)).cloned().collect();
        jobs.sort_by_key(|j| j.created_at);
        jobs
    }

    pub(crate) fn provider(&self, id: &str) -> Result<Arc<dyn PrintProvider>> {
        self.providers
            .get(id)
            .cloned()
            .ok_or_else(|| PrintError::internal(format!("no provider registered with id '{id}'")))
    }

    pub(crate) fn cached_printer(&self, id: &PrinterId) -> Option<Printer> {
        self.printers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_id
            .get(id)
            .cloned()
    }

    fn find_printer(&self, selector: &PrinterSelector) -> Option<Printer> {
        let cache = self.printers.read().unwrap_or_else(PoisonError::into_inner);
        match selector {
            PrinterSelector::Id(id) => cache.by_id.get(id).cloned(),
            PrinterSelector::Name(name) => cache
                .by_id
                .values()
                .find(|p| &p.name == name)
                .or_else(|| {
                    cache
                        .by_id
                        .values()
                        .find(|p| p.name.eq_ignore_ascii_case(name))
                })
                .cloned(),
        }
    }
}

fn log_job(job: &Job, event: Option<JobEventKind>) {
    // Never log payloads or document names: job names can contain personal data.
    info!(
        target: "kiln::jobs",
        job_id = %job.job_id,
        client_id = %job.client_id,
        printer_id = job.printer_id.as_ref().map(PrinterId::as_str),
        status = job.status.as_str(),
        delivery = job.delivery.as_str(),
        spooler_job_id = job.spooler_job_id,
        condition = job.condition.map(ErrorCode::as_str),
        error_code = job.error.as_ref().map(|e| e.error_code.as_str()),
        event = event.map(JobEventKind::name),
        "job updated"
    );
}

/// Runs a blocking closure on the blocking pool while holding a permit from `permits`.
pub(crate) async fn blocking<T, F>(permits: &Arc<Semaphore>, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let permit = permits.clone().acquire_owned().await.map_err(|_| {
        PrintError::new(
            ErrorCode::UnsupportedOperation,
            "the agent is shutting down",
        )
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(PrintError::internal)?
}

/// Cheap, cloneable handle to the engine.
#[derive(Clone)]
pub struct PrintEngine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PrintEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintEngine")
            .field("providers", &self.inner.providers.keys())
            .finish()
    }
}

impl PrintEngine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    pub fn config(&self) -> &EngineConfig {
        &self.inner.config
    }

    /// Discovers printers, reconciles jobs left over from a previous run and starts the
    /// background discovery and retention loops.
    pub async fn start(&self) -> Result<()> {
        if let Err(err) = self.refresh_printers().await {
            warn!(target: "kiln::discovery", error = %err, "initial printer discovery failed");
        }
        self.reconcile_after_restart()?;
        self.inner
            .tasks
            .spawn(discovery::run_loop(self.inner.clone()));
        if let Some(retention) = self.inner.config.job_retention {
            self.inner
                .tasks
                .spawn(retention_loop(self.inner.clone(), retention));
        }
        Ok(())
    }

    /// Stops accepting work, fails jobs that never left the agent (they were not printed)
    /// and waits briefly for in-flight submissions to finish.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        for job in self
            .inner
            .active_jobs(|j| j.delivery == DeliveryStage::RequestAccepted)
        {
            self.inner.mutate(job.job_id, |j| {
                j.fail(not_printed(
                    "the agent shut down before this job was sent to the printer",
                ))
                .then_some(JobEventKind::Failed)
            });
        }
        self.inner.tasks.close();
        if tokio::time::timeout(Duration::from_secs(10), self.inner.tasks.wait())
            .await
            .is_err()
        {
            warn!(target: "kiln::engine", "timed out waiting for in-flight print operations");
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.inner.events.subscribe()
    }

    pub fn languages(&self) -> Vec<LanguageInfo> {
        self.inner.protocols.languages()
    }

    pub fn document_types(&self) -> Vec<DocumentType> {
        self.inner.renderers.document_types()
    }

    // ---------------------------------------------------------------- printers

    pub async fn refresh_printers(&self) -> Result<Vec<Printer>> {
        discovery::refresh(&self.inner).await
    }

    /// Cached printer list, refreshed by the discovery loop.
    pub async fn printers(&self) -> Result<Vec<Printer>> {
        let refreshed = self
            .inner
            .printers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .last_refresh
            .is_some();
        if !refreshed {
            return self.refresh_printers().await;
        }
        let cache = self
            .inner
            .printers
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(cache.by_id.values().cloned().collect())
    }

    /// Resolves a printer, refreshing discovery once (rate limited) if it is unknown, so
    /// newly installed printers work without waiting for the next discovery tick.
    pub async fn resolve_printer(&self, selector: &PrinterSelector) -> Result<Printer> {
        if let Some(printer) = self.inner.find_printer(selector) {
            return Ok(printer);
        }
        discovery::refresh_if_stale(&self.inner).await;
        self.inner
            .find_printer(selector)
            .ok_or_else(|| match selector {
                PrinterSelector::Id(id) => {
                    PrintError::printer_not_found(id).with_printer(id.as_str())
                }
                PrinterSelector::Name(name) => PrintError::printer_not_found(name),
            })
    }

    pub async fn default_printer(&self) -> Result<Option<Printer>> {
        Ok(self.printers().await?.into_iter().find(|p| p.default))
    }

    pub async fn capabilities(&self, id: &PrinterId) -> Result<PrinterCapabilities> {
        let printer = self
            .resolve_printer(&PrinterSelector::Id(id.clone()))
            .await?;
        let provider = self.inner.provider(&printer.provider)?;
        let mut caps = {
            let (provider, printer) = (provider.clone(), printer.clone());
            blocking(&self.inner.monitor_permits, move || {
                provider.capabilities(&printer)
            })
            .await?
        };
        caps.document_types = self
            .inner
            .renderers
            .document_types()
            .into_iter()
            .filter(|t| {
                self.inner.renderers.get(*t).is_some_and(|r| {
                    r.output_kinds()
                        .iter()
                        .any(|k| provider.supports(&printer, *k))
                })
            })
            .collect();
        Ok(caps)
    }

    // ---------------------------------------------------------------- jobs

    /// Creates a job for `request`. Every call creates (or, with a repeated idempotency
    /// key, returns) a job record — including requests that fail validation, which are
    /// stored as `FAILED` and returned as an error carrying the job id.
    pub async fn submit(&self, who: &Submitter, request: PrintRequest) -> Result<Job> {
        if let Some(key) = &request.idempotency_key {
            validate_idempotency_key(key)?;
        }
        let mut job = Job::new(
            &who.client_id,
            request.document.document_type(),
            request.copies,
            request.document.size_bytes(),
        );
        job.job_name = request.job_name.as_deref().map(sanitise_job_name);
        job.language = request.document.language().map(str::to_owned);
        job.idempotency_key = request.idempotency_key.clone();

        {
            let _gate = self
                .inner
                .submit_gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(key) = &job.idempotency_key {
                if let Some(existing) = self
                    .inner
                    .repo
                    .find_by_idempotency_key(&who.client_id, key)?
                {
                    info!(target: "kiln::jobs", job_id = %existing.job_id, "idempotent resubmission; returning existing job");
                    return Ok(self.inner.active_job(existing.job_id).unwrap_or(existing));
                }
            }
            self.inner.track(&job)?;
        }

        let job_id = job.job_id;
        let result = match self.prepare(who, request, job_id).await {
            Ok(prepared) => self.enqueue(prepared),
            Err(err) => Err(err),
        };
        result.map_err(|err| {
            match self.inner.mutate(job_id, |j| {
                j.fail(err.clone()).then_some(JobEventKind::Failed)
            }) {
                Some(m) => m.job.error.unwrap_or(err),
                None => err.with_job(job_id),
            }
        })
    }

    async fn prepare(
        &self,
        who: &Submitter,
        request: PrintRequest,
        job_id: JobId,
    ) -> Result<Prepared> {
        let inner = &self.inner;
        let cfg = &inner.config;
        inner.mutate(job_id, |j| {
            j.transition(JobStatus::Validating);
            None
        });

        let size = request.document.size_bytes();
        if size == 0 {
            return Err(PrintError::invalid_payload("document is empty"));
        }
        if size > cfg.max_document_bytes {
            return Err(PrintError::new(
                ErrorCode::PayloadTooLarge,
                format!(
                    "document is {size} bytes; the limit is {} bytes",
                    cfg.max_document_bytes
                ),
            )
            .with_details(json!({ "sizeBytes": size, "limitBytes": cfg.max_document_bytes })));
        }
        if request.copies == 0 || request.copies > cfg.max_copies {
            return Err(PrintError::invalid_payload(format!(
                "copies must be between 1 and {}",
                cfg.max_copies
            )));
        }

        let printer = self.resolve_printer(&request.printer).await?;
        if !who.printers.allows(&printer) {
            return Err(PrintError::new(
                ErrorCode::AccessDenied,
                "this client is not permitted to use the requested printer",
            )
            .with_printer(printer.id.as_str()));
        }
        inner.mutate(job_id, |j| {
            j.printer_id = Some(printer.id.clone());
            j.printer_name = Some(printer.name.clone());
            None
        });
        let provider = inner.provider(&printer.provider)?;

        let mut warnings = Vec::new();
        let mut language = None;
        if let Document::Raw(raw) = &request.document {
            if let Some(name) = &raw.language {
                let protocol = inner.protocols.resolve(name).ok_or_else(|| {
                    let supported: Vec<_> =
                        inner.protocols.languages().iter().map(|l| l.id).collect();
                    PrintError::invalid_payload(format!("unknown printer language '{name}'"))
                        .with_details(json!({ "supportedLanguages": supported }))
                })?;
                language = Some(protocol.info().id.to_owned());
                // Inspection scans the whole payload (up to max_document_bytes): keep it
                // off the async workers.
                let (inspector, data) = (protocol.clone(), raw.data.clone());
                warnings = blocking(&inner.render_permits, move || {
                    Ok(inspector.inspect(&data).warnings)
                })
                .await?;
                if cfg.strict_languages && !warnings.is_empty() {
                    return Err(PrintError::invalid_payload(format!(
                        "{} data failed validation: {}",
                        protocol.info().id,
                        warnings.join("; ")
                    )));
                }
            }
        }

        let document_type = request.document.document_type();
        let renderer = inner.renderers.get(document_type).cloned().ok_or_else(|| {
            PrintError::new(
                ErrorCode::UnsupportedDocument,
                format!("document type {document_type:?} is not supported by this agent"),
            )
        })?;
        renderer.validate(&request.document)?;

        let target = RenderTarget {
            accepted: PayloadKind::ALL
                .into_iter()
                .filter(|k| provider.supports(&printer, *k))
                .collect(),
            printer: printer.clone(),
        };
        let document = request.document;
        let payload: PrintPayload = blocking(&inner.render_permits, move || {
            renderer.render(document, &target)
        })
        .await?;
        if !provider.supports(&printer, payload.kind()) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                format!(
                    "printer '{}' cannot accept {:?} output",
                    printer.name,
                    payload.kind()
                ),
            )
            .with_printer(printer.id.as_str()));
        }

        let document_name = inner
            .active_job(job_id)
            .and_then(|j| j.job_name)
            .unwrap_or_else(|| format!("Kiln job {job_id}"));
        Ok(Prepared {
            job_id,
            size,
            warnings,
            language,
            spec: SubmitSpec {
                job_id,
                document_name,
                copies: request.copies,
                payload,
            },
            printer,
        })
    }

    fn enqueue(&self, prepared: Prepared) -> Result<Job> {
        let inner = &self.inner;
        let Prepared {
            job_id,
            size,
            warnings,
            language,
            spec,
            printer,
        } = prepared;
        let reservation = inner.budget.try_reserve(size).ok_or_else(|| {
            PrintError::new(
                ErrorCode::QueueFull,
                "the agent print buffer is full; retry later",
            )
            .with_printer(printer.id.as_str())
        })?;

        // Mark QUEUED before handing over so the worker can never observe an earlier state.
        let queued = inner.mutate(job_id, |j| {
            j.warnings = warnings;
            if language.is_some() {
                j.language = language;
            }
            j.transition(JobStatus::Queued)
                .then_some(JobEventKind::Queued)
        });
        let Some(Mutated { job, .. }) = queued else {
            // Cancelled while validating; report the stored (terminal) job.
            return self
                .inner
                .repo
                .get(job_id)?
                .ok_or_else(|| PrintError::job_not_found(job_id));
        };

        let worker_inner = inner.clone();
        let printer_id = printer.id.clone();
        let sender = inner.queues.sender_for(&printer.id, |rx| {
            inner.tasks.spawn(worker::run(worker_inner, printer_id, rx));
        });
        let item = QueuedJob {
            job_id,
            printer: printer.clone(),
            spec,
            reservation,
        };
        match sender.try_send(item) {
            Ok(()) => Ok(job),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Err(PrintError::new(
                ErrorCode::QueueFull,
                format!(
                    "the queue for printer '{}' is full; retry later",
                    printer.name
                ),
            )
            .with_printer(printer.id.as_str())),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                "the agent is shutting down",
            )),
        }
    }

    pub fn job(&self, job_id: JobId) -> Result<Job> {
        if let Some(job) = self.inner.active_job(job_id) {
            return Ok(job);
        }
        self.inner
            .repo
            .get(job_id)?
            .ok_or_else(|| PrintError::job_not_found(job_id))
    }

    pub fn jobs(&self, filter: &JobFilter) -> Result<Vec<Job>> {
        self.inner.repo.list(filter)
    }

    /// Cancels a job wherever it is. Jobs still in the agent queue are dropped without
    /// printing; jobs in the spooler are deleted there (pages already transmitted to the
    /// device may still print).
    pub async fn cancel(&self, job_id: JobId) -> Result<Job> {
        let job = self.job(job_id)?;
        let invalid =
            |message: String| PrintError::new(ErrorCode::InvalidJobState, message).with_job(job_id);
        if job.status.is_terminal() {
            return Err(invalid(format!("job is already {}", job.status.as_str())));
        }
        match job.delivery {
            DeliveryStage::RequestAccepted => self
                .inner
                .mutate(job_id, |j| {
                    (j.delivery == DeliveryStage::RequestAccepted
                        && j.transition(JobStatus::Cancelled))
                    .then_some(JobEventKind::Cancelled)
                })
                .filter(|m| m.job.status == JobStatus::Cancelled)
                .map(|m| m.job)
                .ok_or_else(|| {
                    invalid("job moved on while being cancelled; retry".into()).recoverable(true)
                }),
            DeliveryStage::Submitting => Err(invalid(
                "job is being handed to the spooler; retry the cancellation shortly".into(),
            )
            .recoverable(true)),
            DeliveryStage::SpoolerAccepted => {
                let spooler_job_id = job
                    .spooler_job_id
                    .ok_or_else(|| PrintError::internal("spooled job without spooler id"))?;
                let printer_id = job
                    .printer_id
                    .clone()
                    .ok_or_else(|| PrintError::internal("spooled job without printer"))?;
                let printer = self
                    .resolve_printer(&PrinterSelector::Id(printer_id))
                    .await?;
                let provider = self.inner.provider(&printer.provider)?;
                self.inner
                    .cancelling
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(job_id);
                let cancelled = blocking(&self.inner.provider_permits, move || {
                    provider.cancel(&printer, spooler_job_id)
                })
                .await;
                let settled = cancelled.is_ok().then(|| self.inner.mutate(job_id, |j| {
                    j.transition(JobStatus::Cancelled).then(|| {
                        j.warnings.push(
                            "cancelled after reaching the spooler; data already sent to the printer may still print"
                                .into(),
                        );
                        JobEventKind::Cancelled
                    })
                }));
                self.inner
                    .cancelling
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&job_id);
                cancelled.map_err(|e| e.with_job(job_id))?;
                match settled.flatten() {
                    Some(m) => Ok(m.job),
                    // The monitor observed a terminal state first; report that.
                    None => self.job(job_id),
                }
            }
            DeliveryStage::DeviceDelivered => {
                Err(invalid("job was already delivered to the device".into()))
            }
        }
    }

    // ---------------------------------------------------------------- queues

    pub async fn queue(&self, printer_id: &PrinterId) -> Result<PrinterQueueSnapshot> {
        let printer = self
            .resolve_printer(&PrinterSelector::Id(printer_id.clone()))
            .await?;
        let provider = self.inner.provider(&printer.provider)?;
        let mut spooler_queue = {
            let printer = printer.clone();
            blocking(&self.inner.monitor_permits, move || {
                provider.queue(&printer)
            })
            .await?
        };
        let ours = self
            .inner
            .active_jobs(|j| j.printer_id.as_ref() == Some(&printer.id));
        for entry in &mut spooler_queue {
            entry.kiln_job = ours
                .iter()
                .find(|j| j.spooler_job_id == Some(entry.spooler_job_id))
                .map(|j| Box::new(j.clone()));
        }
        let agent_queue = ours
            .into_iter()
            .filter(|j| {
                matches!(
                    j.delivery,
                    DeliveryStage::RequestAccepted | DeliveryStage::Submitting
                )
            })
            .collect();
        Ok(PrinterQueueSnapshot {
            printer_id: printer.id,
            printer_name: printer.name,
            agent_queue,
            spooler_queue,
        })
    }

    pub async fn queue_summary(&self) -> Result<Vec<QueueSummary>> {
        let printers = self.printers().await?;
        let active = self.inner.active_jobs(|_| true);
        Ok(printers
            .into_iter()
            .map(|p| {
                let mine = active
                    .iter()
                    .filter(|j| j.printer_id.as_ref() == Some(&p.id));
                let (mut agent_queued, mut spooler_active) = (0, 0);
                for job in mine {
                    match job.delivery {
                        DeliveryStage::RequestAccepted | DeliveryStage::Submitting => {
                            agent_queued += 1
                        }
                        DeliveryStage::SpoolerAccepted => spooler_active += 1,
                        DeliveryStage::DeviceDelivered => {}
                    }
                }
                QueueSummary {
                    agent_queued,
                    spooler_active,
                    spooler_total: p.queued_jobs,
                    online: p.online,
                    status: p.status,
                    printer_name: p.name,
                    printer_id: p.id,
                }
            })
            .collect())
    }

    /// Jobs waiting in agent queues right now (diagnostics).
    pub fn agent_queue_depth(&self, printer_id: &PrinterId) -> usize {
        self.inner.queues.depth(printer_id)
    }

    pub fn queued_bytes(&self) -> u64 {
        self.inner.budget.used()
    }

    // ---------------------------------------------------------------- restart

    /// Settles jobs a previous agent process left non-terminal. The engine never resends
    /// a job after a restart: payloads are not persisted, and resending one whose outcome
    /// is unknown could print a duplicate.
    fn reconcile_after_restart(&self) -> Result<()> {
        let inner = &self.inner;
        for job in inner.repo.non_terminal()? {
            let job_id = job.job_id;
            let delivery = job.delivery;
            let spooler_job_id = job.spooler_job_id;
            let printer_id = job.printer_id.clone();
            inner
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(job_id, job);
            match (
                delivery,
                spooler_job_id,
                printer_id.and_then(|id| inner.cached_printer(&id)),
            ) {
                (DeliveryStage::SpoolerAccepted, Some(spooler_id), Some(printer)) => {
                    info!(target: "kiln::jobs", %job_id, spooler_job_id = spooler_id, "resuming monitoring after restart");
                    inner
                        .tasks
                        .spawn(monitor::run(inner.clone(), job_id, printer, spooler_id));
                }
                (DeliveryStage::SpoolerAccepted, ..) => {
                    inner.mutate(job_id, |j| {
                        j.fail(
                            PrintError::new(
                                ErrorCode::PrinterNotFound,
                                "the printer disappeared while the agent was stopped; the job's outcome is unknown",
                            )
                            .recoverable(false)
                            .with_details(json!({ "outcome": "UNKNOWN" })),
                        )
                        .then_some(JobEventKind::Failed)
                    });
                }
                (DeliveryStage::Submitting, ..) => {
                    inner.mutate(job_id, |j| {
                        j.fail(
                            PrintError::new(
                                ErrorCode::PrintFailed,
                                "the agent stopped while handing this job to the printer; it may or may not have printed",
                            )
                            .recoverable(false)
                            .with_details(json!({ "outcome": "UNKNOWN" })),
                        )
                        .then_some(JobEventKind::Failed)
                    });
                }
                (DeliveryStage::RequestAccepted, ..) => {
                    inner.mutate(job_id, |j| {
                        j.fail(not_printed(
                            "the agent restarted before this job was sent to the printer",
                        ))
                        .then_some(JobEventKind::Failed)
                    });
                }
                (DeliveryStage::DeviceDelivered, ..) => {
                    inner.mutate(job_id, |j| {
                        j.complete(CompletionEvidence::BytesDelivered)
                            .then_some(JobEventKind::Completed)
                    });
                }
            }
        }
        Ok(())
    }
}

struct Prepared {
    job_id: JobId,
    size: u64,
    warnings: Vec<String>,
    language: Option<String>,
    spec: SubmitSpec,
    printer: Printer,
}

fn not_printed(message: &str) -> PrintError {
    PrintError::new(ErrorCode::PrintFailed, message)
        .recoverable(true)
        .with_details(json!({ "outcome": "NOT_PRINTED" }))
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 128 || !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(PrintError::invalid_payload(
            "idempotencyKey must be 1-128 printable ASCII characters",
        ));
    }
    Ok(())
}

/// Job names are shown in OS queue UIs: strip control characters and cap the length.
fn sanitise_job_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned()
}

async fn retention_loop(inner: Arc<Inner>, retention: Duration) {
    let Ok(retention) = chrono::Duration::from_std(retention) else {
        return;
    };
    loop {
        let cutoff = Utc::now() - retention;
        match inner.repo.purge_terminal_before(cutoff) {
            Ok(0) => {}
            Ok(n) => info!(target: "kiln::jobs", purged = n, "purged expired job history"),
            Err(err) => warn!(target: "kiln::jobs", error = %err, "job history purge failed"),
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names_are_sanitised() {
        assert_eq!(sanitise_job_name("  Label\u{7}\r\n 42 "), "Label 42");
        assert_eq!(sanitise_job_name(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn idempotency_keys_are_bounded() {
        assert!(validate_idempotency_key("order-42/label-1").is_ok());
        assert!(validate_idempotency_key("").is_err());
        assert!(validate_idempotency_key("has space").is_err());
        assert!(validate_idempotency_key(&"k".repeat(129)).is_err());
    }

    #[test]
    fn scope_matches_id_or_name() {
        let printer = Printer {
            id: PrinterId::from("windows-1"),
            name: "Zebra ZD421".into(),
            display_name: "Zebra ZD421".into(),
            provider: "windows".into(),
            connection: ConnectionType::Usb,
            driver: None,
            port: None,
            location: None,
            default: false,
            online: true,
            status: PrinterState::Ready,
            conditions: vec![],
            queued_jobs: None,
            capabilities: None,
        };
        assert!(PrinterScope::All.allows(&printer));
        assert!(PrinterScope::Only(vec!["zebra zd421".into()]).allows(&printer));
        assert!(PrinterScope::Only(vec!["windows-1".into()]).allows(&printer));
        assert!(!PrinterScope::Only(vec!["Other".into()]).allows(&printer));
    }
}
