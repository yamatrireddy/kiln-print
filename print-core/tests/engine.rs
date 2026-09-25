//! Engine behaviour against the scriptable mock provider (CI-safe, no printers needed).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use kiln_core::engine::{EngineConfig, PrintEngine, PrinterScope, Submitter};
use kiln_core::error::ErrorCode;
use kiln_core::events::EngineEvent;
use kiln_core::model::*;
use kiln_core::provider::{PrintPayload, ProviderJobState};
use kiln_core::repository::{InMemoryJobRepository, JobFilter, JobRepository};
use kiln_provider_mock::{MockPrinter, MockProvider, SubmitBehavior};

fn config() -> EngineConfig {
    EngineConfig {
        monitor_interval: Duration::from_millis(5),
        monitor_max_interval: Duration::from_millis(20),
        discovery_interval: Duration::from_secs(3600),
        discovery_min_gap: Duration::ZERO,
        ..EngineConfig::default()
    }
}

struct Harness {
    engine: PrintEngine,
    provider: Arc<MockProvider>,
    repo: Arc<InMemoryJobRepository>,
}

async fn harness_with(printers: Vec<MockPrinter>, config: EngineConfig) -> Harness {
    harness_with_repo(printers, config, Arc::new(InMemoryJobRepository::new())).await
}

async fn harness_with_repo(
    printers: Vec<MockPrinter>,
    config: EngineConfig,
    repo: Arc<InMemoryJobRepository>,
) -> Harness {
    let provider = Arc::new(MockProvider::new(printers));
    let mut builder = PrintEngine::builder()
        .config(config)
        .provider(provider.clone())
        .repository(repo.clone());
    for r in kiln_renderers::builtin() {
        builder = builder.renderer(r);
    }
    for p in kiln_protocols::builtin() {
        builder = builder.protocol(p);
    }
    let engine = builder.build().expect("engine");
    engine.start().await.expect("start");
    Harness {
        engine,
        provider,
        repo,
    }
}

async fn harness(printers: Vec<MockPrinter>) -> Harness {
    harness_with(printers, config()).await
}

fn anyone() -> Submitter {
    Submitter {
        client_id: "test-client".into(),
        printers: PrinterScope::All,
    }
}

fn raw(printer: &str, data: &'static [u8]) -> PrintRequest {
    PrintRequest {
        printer: PrinterSelector::Name(printer.into()),
        document: Document::Raw(RawDocument {
            data: Bytes::from_static(data),
            language: None,
        }),
        copies: 1,
        job_name: None,
        idempotency_key: None,
    }
}

async fn wait_for(engine: &PrintEngine, job_id: JobId, pred: impl Fn(&Job) -> bool) -> Job {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let job = engine.job(job_id).expect("job exists");
        if pred(&job) {
            return job;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out; last state: {job:#?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_terminal(engine: &PrintEngine, job_id: JobId) -> Job {
    wait_for(engine, job_id, |j| j.status.is_terminal()).await
}

#[tokio::test]
async fn raw_job_runs_full_spooler_lifecycle_byte_for_byte() {
    let h = harness(vec![MockPrinter::new("Zebra")]).await;
    let mut events = h.engine.subscribe();
    let data: &'static [u8] = b"^XA^FO10,10^FDHi\x00\xff^FS^XZ";
    let mut request = raw("Zebra", data);
    if let Document::Raw(r) = &mut request.document {
        r.language = Some("zpl".into());
    }

    let job = h.engine.submit(&anyone(), request).await.expect("accepted");
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.delivery, DeliveryStage::RequestAccepted);
    assert_eq!(
        job.language.as_deref(),
        Some("ZPL"),
        "language is canonicalised"
    );

    let done = wait_terminal(&h.engine, job.job_id).await;
    assert_eq!(done.status, JobStatus::Completed);
    assert_eq!(done.delivery, DeliveryStage::SpoolerAccepted);
    assert_eq!(
        done.completion,
        Some(CompletionEvidence::SpoolerReportedPrinted)
    );
    assert!(
        done.submitted_at.is_some() && done.started_at.is_some() && done.completed_at.is_some()
    );

    let subs = h.provider.submissions();
    assert_eq!(subs.len(), 1);
    let PrintPayload::Raw(payload) = &subs[0].payload else {
        panic!("raw payload expected")
    };
    assert_eq!(
        &payload.bytes[..],
        data,
        "bytes must reach the provider unchanged"
    );

    let mut names = Vec::new();
    while let Ok(ev) = events.try_recv() {
        if let EngineEvent::Job { .. } = &ev {
            names.push(ev.name());
        }
    }
    assert_eq!(
        names,
        [
            "job.created",
            "job.queued",
            "job.spooled",
            "job.printing",
            "job.completed"
        ]
    );
}

#[tokio::test]
async fn direct_delivery_completes_with_bytes_delivered_evidence() {
    let h = harness(vec![
        MockPrinter::new("TCP").behavior(SubmitBehavior::Deliver),
    ])
    .await;
    let job = h
        .engine
        .submit(&anyone(), raw("TCP", b"x"))
        .await
        .expect("accepted");
    let done = wait_terminal(&h.engine, job.job_id).await;
    assert_eq!(done.status, JobStatus::Completed);
    assert_eq!(done.delivery, DeliveryStage::DeviceDelivered);
    assert_eq!(done.completion, Some(CompletionEvidence::BytesDelivered));
}

#[tokio::test]
async fn validation_failures_still_create_a_failed_job() {
    let h = harness(vec![MockPrinter::new("P")]).await;
    let err = h
        .engine
        .submit(&anyone(), raw("Nope", b"x"))
        .await
        .expect_err("unknown printer");
    assert_eq!(err.error_code, ErrorCode::PrinterNotFound);
    let job_id = err.job_id.expect("error carries job id");
    let job = h.engine.job(job_id).expect("job recorded");
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(
        job.error.as_ref().map(|e| e.error_code),
        Some(ErrorCode::PrinterNotFound)
    );
}

#[tokio::test]
async fn payload_limits_and_copies_are_enforced() {
    let cfg = EngineConfig {
        max_document_bytes: 4,
        max_copies: 5,
        ..config()
    };
    let h = harness_with(vec![MockPrinter::new("P")], cfg).await;
    let err = h
        .engine
        .submit(&anyone(), raw("P", b"12345"))
        .await
        .expect_err("too big");
    assert_eq!(err.error_code, ErrorCode::PayloadTooLarge);
    let err = h
        .engine
        .submit(&anyone(), raw("P", b""))
        .await
        .expect_err("empty");
    assert_eq!(err.error_code, ErrorCode::InvalidPayload);
    let mut req = raw("P", b"1");
    req.copies = 6;
    assert_eq!(
        h.engine
            .submit(&anyone(), req)
            .await
            .expect_err("copies")
            .error_code,
        ErrorCode::InvalidPayload
    );
    assert!(h.provider.submissions().is_empty());
}

#[tokio::test]
async fn printer_scope_is_enforced() {
    let h = harness(vec![
        MockPrinter::new("Allowed"),
        MockPrinter::new("Secret"),
    ])
    .await;
    let who = Submitter {
        client_id: "c".into(),
        printers: PrinterScope::Only(vec!["allowed".into()]),
    };
    h.engine
        .submit(&who, raw("Allowed", b"x"))
        .await
        .expect("allowed printer");
    let err = h
        .engine
        .submit(&who, raw("Secret", b"x"))
        .await
        .expect_err("denied");
    assert_eq!(err.error_code, ErrorCode::AccessDenied);
}

#[tokio::test]
async fn unknown_language_and_strict_mode() {
    let h = harness(vec![MockPrinter::new("P")]).await;
    let mut req = raw("P", b"x");
    if let Document::Raw(r) = &mut req.document {
        r.language = Some("PCL9000".into());
    }
    let err = h
        .engine
        .submit(&anyone(), req)
        .await
        .expect_err("unknown language");
    assert_eq!(err.error_code, ErrorCode::InvalidPayload);
    assert!(err.details.is_some());

    // Lenient mode records warnings but prints.
    let mut req = raw("P", b"^XA^FDtruncated");
    if let Document::Raw(r) = &mut req.document {
        r.language = Some("ZPL".into());
    }
    let job = h
        .engine
        .submit(&anyone(), req.clone())
        .await
        .expect("lenient");
    assert!(!job.warnings.is_empty());

    let strict = harness_with(
        vec![MockPrinter::new("P")],
        EngineConfig {
            strict_languages: true,
            ..config()
        },
    )
    .await;
    let err = strict
        .engine
        .submit(&anyone(), req)
        .await
        .expect_err("strict");
    assert_eq!(err.error_code, ErrorCode::InvalidPayload);
}

#[tokio::test]
async fn provider_failure_fails_the_job_without_retry() {
    let failure = kiln_core::PrintError::new(ErrorCode::SpoolerError, "spooler said no");
    let h = harness(vec![
        MockPrinter::new("P").behavior(SubmitBehavior::Fail(failure)),
    ])
    .await;
    let job = h
        .engine
        .submit(&anyone(), raw("P", b"x"))
        .await
        .expect("accepted");
    let done = wait_terminal(&h.engine, job.job_id).await;
    assert_eq!(done.status, JobStatus::Failed);
    assert_eq!(
        done.error.as_ref().map(|e| e.error_code),
        Some(ErrorCode::SpoolerError)
    );
    assert!(h.provider.submissions().is_empty());
}

#[tokio::test]
async fn slow_printer_does_not_block_other_printers() {
    let h = harness(vec![
        MockPrinter::new("Slow").behavior(SubmitBehavior::Delay(Duration::from_millis(400))),
        MockPrinter::new("Fast"),
    ])
    .await;
    let slow = h
        .engine
        .submit(&anyone(), raw("Slow", b"s"))
        .await
        .expect("slow");
    let fast = h
        .engine
        .submit(&anyone(), raw("Fast", b"f"))
        .await
        .expect("fast");
    let fast_done = wait_terminal(&h.engine, fast.job_id).await;
    assert_eq!(fast_done.status, JobStatus::Completed);
    let slow_now = h.engine.job(slow.job_id).expect("slow job");
    assert!(
        !slow_now.status.is_terminal(),
        "slow printer should still be busy"
    );
    wait_terminal(&h.engine, slow.job_id).await;
}

#[tokio::test]
async fn jobs_for_one_printer_keep_submission_order() {
    let h = harness(vec![
        MockPrinter::new("P").behavior(SubmitBehavior::Delay(Duration::from_millis(5))),
    ])
    .await;
    let payloads: [&'static [u8]; 6] = [b"1", b"2", b"3", b"4", b"5", b"6"];
    let mut ids = Vec::new();
    for p in payloads {
        ids.push(
            h.engine
                .submit(&anyone(), raw("P", p))
                .await
                .expect("accepted")
                .job_id,
        );
    }
    for id in &ids {
        wait_terminal(&h.engine, *id).await;
    }
    let order: Vec<_> = h
        .provider
        .submissions()
        .iter()
        .map(|s| match &s.payload {
            PrintPayload::Raw(r) => r.bytes.clone(),
            PrintPayload::Text(_) => unreachable!(),
        })
        .collect();
    assert_eq!(order, payloads.map(Bytes::from_static).to_vec());
}

#[tokio::test]
async fn full_queue_applies_backpressure() {
    let cfg = EngineConfig {
        queue_capacity_per_printer: 1,
        ..config()
    };
    let h = harness_with(
        vec![MockPrinter::new("P").behavior(SubmitBehavior::Delay(Duration::from_millis(300)))],
        cfg,
    )
    .await;
    let first = h
        .engine
        .submit(&anyone(), raw("P", b"1"))
        .await
        .expect("in flight");
    wait_for(&h.engine, first.job_id, |j| {
        j.delivery == DeliveryStage::Submitting
    })
    .await;
    h.engine
        .submit(&anyone(), raw("P", b"2"))
        .await
        .expect("queued");
    let err = h
        .engine
        .submit(&anyone(), raw("P", b"3"))
        .await
        .expect_err("queue full");
    assert_eq!(err.error_code, ErrorCode::QueueFull);
    assert!(err.recoverable);
}

#[tokio::test]
async fn byte_budget_applies_backpressure() {
    let cfg = EngineConfig {
        max_queued_bytes: 3,
        ..config()
    };
    let h = harness_with(
        vec![MockPrinter::new("P").behavior(SubmitBehavior::Delay(Duration::from_millis(200)))],
        cfg,
    )
    .await;
    h.engine
        .submit(&anyone(), raw("P", b"12"))
        .await
        .expect("fits");
    let err = h
        .engine
        .submit(&anyone(), raw("P", b"34"))
        .await
        .expect_err("over budget");
    assert_eq!(err.error_code, ErrorCode::QueueFull);
}

#[tokio::test]
async fn cancelling_a_queued_job_never_prints_it() {
    let h = harness(vec![
        MockPrinter::new("P").behavior(SubmitBehavior::Delay(Duration::from_millis(200))),
    ])
    .await;
    let first = h
        .engine
        .submit(&anyone(), raw("P", b"first"))
        .await
        .expect("first");
    wait_for(&h.engine, first.job_id, |j| {
        j.delivery == DeliveryStage::Submitting
    })
    .await;
    let second = h
        .engine
        .submit(&anyone(), raw("P", b"second"))
        .await
        .expect("second");

    let cancelled = h.engine.cancel(second.job_id).await.expect("cancel");
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    let err = h
        .engine
        .cancel(first.job_id)
        .await
        .expect_err("in-flight job");
    assert_eq!(err.error_code, ErrorCode::InvalidJobState);
    assert!(err.recoverable);

    wait_terminal(&h.engine, first.job_id).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let printed: Vec<_> = h.provider.submissions().iter().map(|s| s.job_id).collect();
    assert_eq!(printed, vec![first.job_id]);
    assert_eq!(
        h.engine
            .cancel(second.job_id)
            .await
            .expect_err("terminal")
            .error_code,
        ErrorCode::InvalidJobState
    );
}

#[tokio::test]
async fn cancelling_a_spooled_job_deletes_it_from_the_spooler() {
    let h = harness(vec![
        MockPrinter::new("P").script(vec![ProviderJobState::Pending]),
    ])
    .await;
    let job = h
        .engine
        .submit(&anyone(), raw("P", b"x"))
        .await
        .expect("accepted");
    let spooled = wait_for(&h.engine, job.job_id, |j| {
        j.delivery == DeliveryStage::SpoolerAccepted
    })
    .await;
    let cancelled = h.engine.cancel(job.job_id).await.expect("cancel");
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert_eq!(
        h.provider.cancelled(),
        vec![spooled.spooler_job_id.expect("spooler id")]
    );
}

#[tokio::test]
async fn idempotency_key_prevents_duplicate_prints() {
    let h = harness(vec![MockPrinter::new("P")]).await;
    let mut req = raw("P", b"label");
    req.idempotency_key = Some("order-7/label-1".into());
    let a = h
        .engine
        .submit(&anyone(), req.clone())
        .await
        .expect("first");
    let b = h
        .engine
        .submit(&anyone(), req.clone())
        .await
        .expect("resubmission");
    assert_eq!(a.job_id, b.job_id);
    wait_terminal(&h.engine, a.job_id).await;
    let c = h
        .engine
        .submit(&anyone(), req.clone())
        .await
        .expect("after completion");
    assert_eq!(c.job_id, a.job_id);
    assert_eq!(h.provider.submissions().len(), 1);

    // Keys are scoped per client.
    let other = Submitter {
        client_id: "other".into(),
        printers: PrinterScope::All,
    };
    let d = h.engine.submit(&other, req).await.expect("other client");
    assert_ne!(d.job_id, a.job_id);
}

#[tokio::test]
async fn blocking_conditions_are_reported_without_failing() {
    let h = harness(vec![MockPrinter::new("P").script(vec![
        ProviderJobState::Blocked {
            condition: ErrorCode::PaperOut,
            message: None,
        },
        ProviderJobState::Blocked {
            condition: ErrorCode::PaperOut,
            message: None,
        },
        ProviderJobState::Printing,
        ProviderJobState::Printed,
    ])])
    .await;
    let mut events = h.engine.subscribe();
    let job = h
        .engine
        .submit(&anyone(), raw("P", b"x"))
        .await
        .expect("accepted");
    let done = wait_terminal(&h.engine, job.job_id).await;
    assert_eq!(done.status, JobStatus::Completed);
    assert_eq!(
        done.condition, None,
        "condition clears when printing resumes"
    );

    let mut saw_paper_out = false;
    while let Ok(ev) = events.try_recv() {
        if let EngineEvent::Job { job, .. } = &ev {
            saw_paper_out |=
                ev.name() == "job.updated" && job.condition == Some(ErrorCode::PaperOut);
        }
    }
    assert!(saw_paper_out);
}

#[tokio::test]
async fn external_deletion_is_reported_as_cancelled() {
    let h = harness(vec![
        MockPrinter::new("P").script(vec![ProviderJobState::Pending]),
    ])
    .await;
    let job = h
        .engine
        .submit(&anyone(), raw("P", b"x"))
        .await
        .expect("accepted");
    let spooled = wait_for(&h.engine, job.job_id, |j| j.spooler_job_id.is_some()).await;
    h.provider.set_job_state(
        spooled.spooler_job_id.expect("id"),
        ProviderJobState::Cancelled,
    );
    let done = wait_terminal(&h.engine, job.job_id).await;
    assert_eq!(done.status, JobStatus::Cancelled);
}

#[tokio::test]
async fn retired_job_completes_with_weaker_evidence() {
    let h = harness(vec![
        MockPrinter::new("P").script(vec![ProviderJobState::Printing, ProviderJobState::Gone]),
    ])
    .await;
    let job = h
        .engine
        .submit(&anyone(), raw("P", b"x"))
        .await
        .expect("accepted");
    let done = wait_terminal(&h.engine, job.job_id).await;
    assert_eq!(done.completion, Some(CompletionEvidence::SpoolerJobRetired));
}

#[tokio::test]
async fn submission_timeout_reports_unknown_outcome() {
    let cfg = EngineConfig {
        submit_timeout: Duration::from_millis(50),
        ..config()
    };
    let h = harness_with(
        vec![MockPrinter::new("P").behavior(SubmitBehavior::Delay(Duration::from_millis(300)))],
        cfg,
    )
    .await;
    let job = h
        .engine
        .submit(&anyone(), raw("P", b"x"))
        .await
        .expect("accepted");
    let done = wait_terminal(&h.engine, job.job_id).await;
    let err = done.error.expect("error");
    assert_eq!(err.error_code, ErrorCode::Timeout);
    assert!(
        !err.recoverable,
        "unknown outcome must not invite blind retries"
    );
    assert_eq!(
        err.details.as_ref().and_then(|d| d["outcome"].as_str()),
        Some("UNKNOWN")
    );
}

#[tokio::test]
async fn restart_reconciles_unfinished_jobs_without_reprinting() {
    let repo = Arc::new(InMemoryJobRepository::new());
    let printer_id = PrinterId::derive("mock", "P");
    let mk = |delivery, spooler: Option<u64>| {
        let mut j = Job::new("c", DocumentType::Raw, 1, 1);
        j.printer_id = Some(printer_id.clone());
        j.printer_name = Some("P".into());
        j.status = JobStatus::Queued;
        j.delivery = delivery;
        j.spooler_job_id = spooler;
        repo.insert(&j).expect("insert");
        j.job_id
    };
    let never_sent = mk(DeliveryStage::RequestAccepted, None);
    let mid_submit = mk(DeliveryStage::Submitting, None);
    // The mock has no job 999, so the resumed monitor sees it as gone (retired).
    let spooled = mk(DeliveryStage::SpoolerAccepted, Some(999));

    let h = harness_with_repo(vec![MockPrinter::new("P")], config(), repo.clone()).await;

    let a = h.engine.job(never_sent).expect("job");
    assert_eq!(a.status, JobStatus::Failed);
    let a_err = a.error.expect("error");
    assert!(a_err.recoverable);
    assert_eq!(
        a_err.details.as_ref().and_then(|d| d["outcome"].as_str()),
        Some("NOT_PRINTED")
    );

    let b = h.engine.job(mid_submit).expect("job");
    assert_eq!(b.status, JobStatus::Failed);
    let b_err = b.error.expect("error");
    assert!(!b_err.recoverable);
    assert_eq!(
        b_err.details.as_ref().and_then(|d| d["outcome"].as_str()),
        Some("UNKNOWN")
    );

    let c = wait_terminal(&h.engine, spooled).await;
    assert_eq!(c.completion, Some(CompletionEvidence::SpoolerJobRetired));
    assert!(
        h.provider.submissions().is_empty(),
        "nothing is resent after a restart"
    );
    assert!(h.repo.non_terminal().expect("query").is_empty());
}

#[tokio::test]
async fn shutdown_fails_jobs_that_never_left_the_agent() {
    let h = harness(vec![
        MockPrinter::new("P").behavior(SubmitBehavior::Delay(Duration::from_millis(150))),
    ])
    .await;
    let first = h
        .engine
        .submit(&anyone(), raw("P", b"1"))
        .await
        .expect("first");
    wait_for(&h.engine, first.job_id, |j| {
        j.delivery == DeliveryStage::Submitting
    })
    .await;
    let second = h
        .engine
        .submit(&anyone(), raw("P", b"2"))
        .await
        .expect("second");
    h.engine.shutdown().await;
    let second = h.engine.job(second.job_id).expect("job");
    assert_eq!(second.status, JobStatus::Failed);
    assert_eq!(h.provider.submissions().len(), 1);
}

#[tokio::test]
async fn discovery_emits_printer_events() {
    let h = harness(vec![MockPrinter::new("A")]).await;
    let mut events = h.engine.subscribe();
    h.provider.add_printer(MockPrinter::new("B"));
    h.provider.set_online("A", false);
    h.engine.refresh_printers().await.expect("refresh");
    h.provider.remove_printer("B");
    h.engine.refresh_printers().await.expect("refresh");

    let mut names = Vec::new();
    while let Ok(ev) = events.try_recv() {
        names.push(ev.name());
    }
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "printer.connected",
            "printer.disconnected",
            "printer.status.changed"
        ]
    );
}

#[tokio::test]
async fn discovery_failure_keeps_last_known_printers() {
    let h = harness(vec![MockPrinter::new("A")]).await;
    h.provider
        .set_discovery_error(Some(kiln_core::PrintError::new(
            ErrorCode::SpoolerError,
            "down",
        )));
    let printers = h
        .engine
        .refresh_printers()
        .await
        .expect("refresh tolerates provider failure");
    assert_eq!(printers.len(), 1);
}

#[tokio::test]
async fn text_documents_render_in_both_modes() {
    let h = harness(vec![
        MockPrinter::new("Laser"),
        MockPrinter::new("Receipt").raw_only(),
    ])
    .await;
    let text = |printer: &str, mode| PrintRequest {
        printer: PrinterSelector::Name(printer.into()),
        document: Document::Text(TextDocument {
            text: "Hello\nWorld".into(),
            options: TextOptions {
                mode,
                ..TextOptions::default()
            },
        }),
        copies: 2,
        job_name: Some("Greeting".into()),
        idempotency_key: None,
    };
    let a = h
        .engine
        .submit(&anyone(), text("Laser", TextMode::Rendered))
        .await
        .expect("rendered");
    let b = h
        .engine
        .submit(&anyone(), text("Receipt", TextMode::Raw))
        .await
        .expect("raw text");
    let err = h
        .engine
        .submit(&anyone(), text("Receipt", TextMode::Rendered))
        .await
        .expect_err("no GDI");
    assert_eq!(err.error_code, ErrorCode::UnsupportedOperation);
    wait_terminal(&h.engine, a.job_id).await;
    wait_terminal(&h.engine, b.job_id).await;

    let subs = h.provider.submissions();
    assert!(matches!(subs[0].payload, PrintPayload::Text(_)));
    assert_eq!(subs[0].copies, 2);
    assert_eq!(subs[0].document_name, "Greeting");
    let PrintPayload::Raw(r) = &subs[1].payload else {
        panic!("raw")
    };
    assert_eq!(&r.bytes[..], b"Hello\r\nWorld\r\n\x0c");
}

#[tokio::test]
async fn capabilities_list_deliverable_document_types() {
    let h = harness(vec![MockPrinter::new("Label").raw_only()]).await;
    let caps = h
        .engine
        .capabilities(&PrinterId::derive("mock", "Label"))
        .await
        .expect("caps");
    // TEXT stays available through RAW text mode.
    assert_eq!(
        caps.document_types,
        vec![DocumentType::Raw, DocumentType::Text]
    );
    assert_eq!(caps.raw, Some(true));
}

#[tokio::test]
async fn job_listing_filters() {
    let h = harness(vec![MockPrinter::new("A"), MockPrinter::new("B")]).await;
    let a = h.engine.submit(&anyone(), raw("A", b"1")).await.expect("a");
    let b = h.engine.submit(&anyone(), raw("B", b"2")).await.expect("b");
    wait_terminal(&h.engine, a.job_id).await;
    wait_terminal(&h.engine, b.job_id).await;
    let only_a = h
        .engine
        .jobs(&JobFilter {
            printer_id: Some(PrinterId::derive("mock", "A")),
            ..JobFilter::default()
        })
        .expect("list");
    assert_eq!(
        only_a.iter().map(|j| j.job_id).collect::<Vec<_>>(),
        vec![a.job_id]
    );
    let failed = h
        .engine
        .jobs(&JobFilter {
            statuses: vec![JobStatus::Failed],
            ..JobFilter::default()
        })
        .expect("list");
    assert!(failed.is_empty());
}
