//! Printer discovery: periodic refresh plus change events.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, PoisonError};
use std::time::Instant;

use tracing::{info, warn};

use super::{Inner, blocking};
use crate::error::Result;
use crate::events::{EngineEvent, PrinterEventKind};
use crate::model::Printer;

pub(super) async fn run_loop(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(inner.config.discovery_interval) => {}
        }
        if let Err(err) = refresh(&inner).await {
            warn!(target: "kiln::discovery", error = %err, "printer discovery failed");
        }
    }
}

/// Refreshes unless another refresh happened within `discovery_min_gap`.
pub(super) async fn refresh_if_stale(inner: &Arc<Inner>) {
    let stale = inner
        .printers
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .last_refresh
        .is_none_or(|t| t.elapsed() >= inner.config.discovery_min_gap);
    if stale {
        if let Err(err) = refresh(inner).await {
            warn!(target: "kiln::discovery", error = %err, "on-demand printer discovery failed");
        }
    }
}

pub(super) async fn refresh(inner: &Arc<Inner>) -> Result<Vec<Printer>> {
    // Coalesce concurrent refreshes: whoever waits here gets the fresh result.
    let _guard = inner.refresh_lock.lock().await;

    let mut discovered = Vec::new();
    let mut failed: HashSet<String> = HashSet::new();
    for (id, provider) in &inner.providers {
        let p = provider.clone();
        match blocking(&inner.monitor_permits, move || p.discover()).await {
            Ok(printers) => discovered.extend(printers),
            Err(err) => {
                // Keep the previous snapshot for this provider rather than reporting
                // every printer as disconnected because of one transient failure.
                warn!(target: "kiln::discovery", provider = %id, error = %err, "provider discovery failed");
                failed.insert(id.clone());
            }
        }
    }

    let events = {
        let mut cache = inner
            .printers
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let mut next: BTreeMap<_, _> = cache
            .by_id
            .iter()
            .filter(|(_, p)| failed.contains(&p.provider))
            .map(|(id, p)| (id.clone(), p.clone()))
            .collect();
        for printer in discovered {
            next.insert(printer.id.clone(), printer);
        }

        let mut events = Vec::new();
        if cache.last_refresh.is_some() {
            for (id, printer) in &next {
                match cache.by_id.get(id) {
                    None => events.push((PrinterEventKind::Connected, printer.clone())),
                    Some(old) if old.status_differs(printer) => {
                        events.push((PrinterEventKind::StatusChanged, printer.clone()))
                    }
                    Some(_) => {}
                }
            }
            for (id, printer) in &cache.by_id {
                if !next.contains_key(id) {
                    events.push((PrinterEventKind::Disconnected, printer.clone()));
                }
            }
        } else {
            info!(target: "kiln::discovery", count = next.len(), "initial printer discovery complete");
        }
        cache.by_id = next;
        cache.last_refresh = Some(Instant::now());
        events
    };

    for (kind, printer) in events {
        info!(
            target: "kiln::discovery",
            printer_id = %printer.id,
            online = printer.online,
            status = ?printer.status,
            event = ?kind,
            "printer changed"
        );
        let _ = inner.events.send(EngineEvent::Printer {
            kind,
            printer: Box::new(printer),
        });
    }

    let cache = inner
        .printers
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    Ok(cache.by_id.values().cloned().collect())
}
