//! Transport-independent operations with authorisation. WebSocket methods and REST routes
//! both call into this module, so permission checks exist in exactly one place.

use kiln_core::engine::{PrinterQueueSnapshot, QueueSummary};
use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::model::{
    Job, JobId, PrintRequest, Printer, PrinterCapabilities, PrinterId, PrinterSelector,
};
use kiln_core::repository::JobFilter;
use serde::Serialize;
use serde_json::{Value, json};

use super::AppState;
use super::sessions::SessionInfo;
use super::wire::*;
use crate::security::{Permission, Principal, PrincipalKind};

type Result<T> = std::result::Result<T, PrintError>;

/// Routes a WebSocket method call.
pub async fn dispatch(
    state: &AppState,
    who: &Principal,
    method: &str,
    params: Value,
) -> Result<Value> {
    let max = state.config.limits.max_document_bytes;
    match method {
        "session.ping" => Ok(json!({ "time": chrono::Utc::now() })),
        "printers.list" => to_json(list_printers(state, who).await?),
        "printers.default" => to_json(default_printer(state, who).await?),
        "printers.get" => {
            let p: PrinterParams = parse_params(params)?;
            to_json(get_printer(state, who, &selector(p.printer_id, p.printer)?).await?)
        }
        "printers.capabilities" => {
            let p: PrinterParams = parse_params(params)?;
            to_json(capabilities(state, who, &selector(p.printer_id, p.printer)?).await?)
        }
        "print.submit" => {
            who.require(Permission::Print)?;
            match parse_params::<GenericPrintParams>(params)?.into_typed(max)? {
                TypedPrint::Ready(request) => to_json(submit(state, who, request).await?),
                TypedPrint::Pending(pending) => to_json(submit_pending(state, who, pending).await?),
            }
        }
        "print.raw" => to_json(
            submit(
                state,
                who,
                parse_params::<RawPrintParams>(params)?.into_request(max)?,
            )
            .await?,
        ),
        "print.text" => to_json(
            submit(
                state,
                who,
                parse_params::<TextPrintParams>(params)?.into_request(max)?,
            )
            .await?,
        ),
        "print.html" => to_json(
            submit(
                state,
                who,
                parse_params::<HtmlPrintParams>(params)?.into_request(max)?,
            )
            .await?,
        ),
        "print.pdf" => {
            let pending = parse_params::<PdfPrintParams>(params)?.into_pending()?;
            to_json(submit_pending(state, who, pending).await?)
        }
        "print.image" => {
            let pending = parse_params::<ImagePrintParams>(params)?.into_pending()?;
            to_json(submit_pending(state, who, pending).await?)
        }
        "jobs.list" => to_json(list_jobs(state, who, parse_params(params)?)?),
        "jobs.get" => to_json(get_job(
            state,
            who,
            parse_params::<JobIdParams>(params)?.job_id()?,
        )?),
        "jobs.cancel" => {
            to_json(cancel_job(state, who, parse_params::<JobIdParams>(params)?.job_id()?).await?)
        }
        "queue.list" => to_json(queue_summary(state, who).await?),
        "queue.get" => {
            let p: PrinterParams = parse_params(params)?;
            to_json(queue(state, who, &selector(p.printer_id, p.printer)?).await?)
        }
        "clients.list" => to_json(list_clients(state, who)?),
        "session.hello" => Err(PrintError::new(
            ErrorCode::InvalidPayload,
            "session is already authenticated",
        )),
        other => Err(PrintError::new(
            ErrorCode::UnsupportedOperation,
            format!("unknown method '{other}'"),
        )),
    }
}

fn to_json(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(PrintError::internal)
}

// ------------------------------------------------------------------ printers

pub async fn list_printers(state: &AppState, who: &Principal) -> Result<Vec<Printer>> {
    who.require(Permission::PrintersRead)?;
    let scope = who.printer_scope();
    Ok(state
        .engine
        .printers()
        .await?
        .into_iter()
        .filter(|p| scope.allows(p))
        .collect())
}

/// Resolves a printer the caller may see. Printers outside the caller's scope are
/// reported as not found so their existence is not disclosed.
async fn visible_printer(
    state: &AppState,
    who: &Principal,
    selector: &PrinterSelector,
) -> Result<Printer> {
    let printer = state.engine.resolve_printer(selector).await?;
    if who.printer_scope().allows(&printer) {
        Ok(printer)
    } else {
        Err(match selector {
            PrinterSelector::Id(id) => PrintError::printer_not_found(id),
            PrinterSelector::Name(name) => PrintError::printer_not_found(name),
        })
    }
}

pub async fn get_printer(
    state: &AppState,
    who: &Principal,
    selector: &PrinterSelector,
) -> Result<Printer> {
    who.require(Permission::PrintersRead)?;
    let mut printer = visible_printer(state, who, selector).await?;
    printer.capabilities = state.engine.capabilities(&printer.id).await.ok();
    Ok(printer)
}

pub async fn default_printer(state: &AppState, who: &Principal) -> Result<Option<Printer>> {
    who.require(Permission::PrintersRead)?;
    let scope = who.printer_scope();
    Ok(state
        .engine
        .default_printer()
        .await?
        .filter(|p| scope.allows(p)))
}

pub async fn capabilities(
    state: &AppState,
    who: &Principal,
    selector: &PrinterSelector,
) -> Result<PrinterCapabilities> {
    who.require(Permission::PrintersRead)?;
    let printer = visible_printer(state, who, selector).await?;
    state.engine.capabilities(&printer.id).await
}

// ------------------------------------------------------------------ printing

pub async fn submit(state: &AppState, who: &Principal, request: PrintRequest) -> Result<Job> {
    who.require(Permission::Print)?;
    state.engine.submit(&who.submitter(), request).await
}

/// Resolves a `path`/`url`/inline source, then submits. Permission is checked first so an
/// unprivileged client can never make the agent read files or fetch URLs.
pub async fn submit_pending(
    state: &AppState,
    who: &Principal,
    pending: PendingRequest,
) -> Result<Job> {
    who.require(Permission::Print)?;
    let max = state.config.limits.max_document_bytes;
    let data = crate::sources::resolve(&state.config.sources, pending.source.clone(), max).await?;
    submit(state, who, pending.complete(data)).await
}

// ------------------------------------------------------------------ jobs

fn can_see(who: &Principal, job: &Job) -> bool {
    who.has(Permission::JobsReadAll)
        || (who.has(Permission::JobsRead) && job.client_id == who.client_id)
}

pub fn list_jobs(state: &AppState, who: &Principal, params: JobsListParams) -> Result<Vec<Job>> {
    if !who.has(Permission::JobsRead) && !who.has(Permission::JobsReadAll) {
        who.require(Permission::JobsRead)?;
    }
    let client_id = if who.has(Permission::JobsReadAll) {
        params.client_id
    } else {
        // Clients only ever see their own jobs, whatever they ask for.
        Some(who.client_id.clone())
    };
    let filter = JobFilter {
        client_id,
        printer_id: params.printer_id.map(PrinterId),
        statuses: params.status.parse()?,
        since: params.since,
        until: params.until,
        limit: params.limit.unwrap_or(100),
        offset: params.offset.unwrap_or(0),
    };
    state.engine.jobs(&filter)
}

pub fn get_job(state: &AppState, who: &Principal, job_id: JobId) -> Result<Job> {
    match state.engine.job(job_id) {
        Ok(job) if can_see(who, &job) => Ok(job),
        // Same answer for "missing" and "not yours": job ids are not an oracle.
        Ok(_) | Err(_) => Err(PrintError::job_not_found(job_id)),
    }
}

pub async fn cancel_job(state: &AppState, who: &Principal, job_id: JobId) -> Result<Job> {
    let job = get_job(state, who, job_id)?;
    if job.client_id == who.client_id {
        if !who.has(Permission::JobsCancel) {
            who.require(Permission::JobsCancelAll)?;
        }
    } else {
        who.require(Permission::JobsCancelAll)?;
    }
    state.engine.cancel(job_id).await
}

// ------------------------------------------------------------------ queues

pub async fn queue_summary(state: &AppState, who: &Principal) -> Result<Vec<QueueSummary>> {
    who.require(Permission::QueueRead)?;
    let visible: Vec<PrinterId> = list_printers_unchecked(state, who).await?;
    Ok(state
        .engine
        .queue_summary()
        .await?
        .into_iter()
        .filter(|q| visible.contains(&q.printer_id))
        .collect())
}

async fn list_printers_unchecked(state: &AppState, who: &Principal) -> Result<Vec<PrinterId>> {
    let scope = who.printer_scope();
    Ok(state
        .engine
        .printers()
        .await?
        .into_iter()
        .filter(|p| scope.allows(p))
        .map(|p| p.id)
        .collect())
}

pub async fn queue(
    state: &AppState,
    who: &Principal,
    selector: &PrinterSelector,
) -> Result<PrinterQueueSnapshot> {
    who.require(Permission::QueueRead)?;
    let printer = visible_printer(state, who, selector).await?;
    let mut snapshot = state.engine.queue(&printer.id).await?;
    if !who.has(Permission::JobsReadAll) {
        // OS queues also hold other applications' and users' documents. Clients see that
        // those entries exist (queue position matters) but not what they are.
        snapshot
            .agent_queue
            .retain(|j| j.client_id == who.client_id);
        for entry in &mut snapshot.spooler_queue {
            let ours = entry
                .kiln_job
                .as_ref()
                .is_some_and(|j| j.client_id == who.client_id);
            if !ours {
                entry.document_name = None;
                entry.status_text = None;
                entry.kiln_job = None;
            }
        }
    }
    Ok(snapshot)
}

// ------------------------------------------------------------------ clients

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSummary {
    pub client_id: String,
    pub name: String,
    pub kind: PrincipalKind,
    pub origins: Vec<String>,
    pub permissions: Vec<Permission>,
    pub printers: Vec<String>,
    pub connected: bool,
    pub sessions: Vec<SessionInfo>,
}

pub fn list_clients(state: &AppState, who: &Principal) -> Result<Vec<ClientSummary>> {
    who.require(Permission::ClientsRead)?;
    let sessions = state.sessions.list();
    let for_client = |id: &str| {
        sessions
            .iter()
            .filter(|s| s.client_id == id)
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut out = vec![{
        let s = for_client(crate::security::ADMIN_CLIENT_ID);
        ClientSummary {
            client_id: crate::security::ADMIN_CLIENT_ID.into(),
            name: "Local administrator".into(),
            kind: PrincipalKind::Admin,
            origins: state.config.security.admin_origins.clone(),
            permissions: Permission::ALL.to_vec(),
            printers: vec!["*".into()],
            connected: !s.is_empty(),
            sessions: s,
        }
    }];
    for client in &state.config.security.clients {
        let s = for_client(&client.id);
        out.push(ClientSummary {
            client_id: client.id.clone(),
            name: client.name.clone(),
            kind: PrincipalKind::Client,
            origins: client.origins.clone(),
            permissions: client.permissions.clone(),
            printers: client.printers.clone(),
            connected: !s.is_empty(),
            sessions: s,
        });
    }
    Ok(out)
}
