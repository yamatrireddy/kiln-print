//! Embedded SQLite storage: job metadata and the audit log.
//!
//! Only job *metadata* is stored — never document payloads. Each job row keeps the full
//! JSON document for forward compatibility plus indexed columns for filtering.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, SecondsFormat, Utc};
use kiln_core::error::{PrintError, Result};
use kiln_core::model::{Job, JobId, JobStatus};
use kiln_core::repository::{JobFilter, JobRepository};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};
use serde::Serialize;

const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE jobs (
        job_id          TEXT PRIMARY KEY,
        client_id       TEXT NOT NULL,
        printer_id      TEXT,
        status          TEXT NOT NULL,
        created_at      TEXT NOT NULL,
        updated_at      TEXT NOT NULL,
        idempotency_key TEXT,
        data            TEXT NOT NULL
    );
    CREATE INDEX jobs_created_at ON jobs (created_at);
    CREATE INDEX jobs_status ON jobs (status);
    CREATE INDEX jobs_client ON jobs (client_id, created_at);
    CREATE INDEX jobs_printer ON jobs (printer_id, created_at);
    CREATE UNIQUE INDEX jobs_idempotency ON jobs (client_id, idempotency_key)
        WHERE idempotency_key IS NOT NULL;

    CREATE TABLE audit_log (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        ts        TEXT NOT NULL,
        event     TEXT NOT NULL,
        client_id TEXT,
        origin    TEXT,
        details   TEXT
    );
    CREATE INDEX audit_ts ON audit_log (ts);
    "#,
];

/// Timestamps are stored as fixed-width RFC 3339 UTC so text order equals time order.
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn db_error(err: impl std::fmt::Display) -> PrintError {
    PrintError::internal(format!("database error: {err}"))
}

#[derive(Debug)]
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(usize::MAX);
        anyhow::ensure!(
            version <= MIGRATIONS.len(),
            "database schema v{version} is newer than this agent supports (v{})",
            MIGRATIONS.len()
        );
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn audit(
        &self,
        event: &str,
        client_id: Option<&str>,
        origin: Option<&str>,
        details: &serde_json::Value,
    ) {
        let result = self.conn().execute(
            "INSERT INTO audit_log (ts, event, client_id, origin, details) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts(Utc::now()), event, client_id, origin, details.to_string()],
        );
        if let Err(err) = result {
            tracing::warn!(target: "kiln::audit", error = %err, event, "failed to persist audit record");
        }
    }

    pub fn audit_entries(&self, limit: usize) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, ts, event, client_id, origin, details FROM audit_log ORDER BY id DESC LIMIT ?1")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([limit.min(1000) as i64], |r| {
                Ok(AuditEntry {
                    id: r.get(0)?,
                    timestamp: r.get(1)?,
                    event: r.get(2)?,
                    client_id: r.get(3)?,
                    origin: r.get(4)?,
                    details: r
                        .get::<_, Option<String>>(5)?
                        .and_then(|d| serde_json::from_str(&d).ok()),
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<_, _>>().map_err(db_error)
    }

    pub fn purge_audit_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.conn()
            .execute("DELETE FROM audit_log WHERE ts < ?1", [ts(cutoff)])
            .map(|n| n as u64)
            .map_err(db_error)
    }

    fn upsert(&self, job: &Job) -> Result<()> {
        let data = serde_json::to_string(job).map_err(db_error)?;
        self.conn()
            .execute(
                "INSERT INTO jobs (job_id, client_id, printer_id, status, created_at, updated_at, idempotency_key, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(job_id) DO UPDATE SET
                    printer_id = excluded.printer_id, status = excluded.status,
                    updated_at = excluded.updated_at, data = excluded.data",
                params![
                    job.job_id.to_string(),
                    job.client_id,
                    job.printer_id.as_ref().map(|p| p.as_str()),
                    job.status.as_str(),
                    ts(job.created_at),
                    ts(job.updated_at),
                    job.idempotency_key,
                    data
                ],
            )
            .map(|_| ())
            .map_err(db_error)
    }

    fn query_jobs(&self, sql: &str, args: Vec<Value>) -> Result<Vec<Job>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql).map_err(db_error)?;
        let rows = stmt
            .query_map(params_from_iter(args), |r| r.get::<_, String>(0))
            .map_err(db_error)?;
        let mut jobs = Vec::new();
        for row in rows {
            let data = row.map_err(db_error)?;
            match serde_json::from_str::<Job>(&data) {
                Ok(job) => jobs.push(job),
                Err(err) => {
                    tracing::warn!(target: "kiln::jobs", error = %err, "skipping unreadable job record")
                }
            }
        }
        Ok(jobs)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub timestamp: String,
    pub event: String,
    pub client_id: Option<String>,
    pub origin: Option<String>,
    pub details: Option<serde_json::Value>,
}

impl JobRepository for Database {
    fn insert(&self, job: &Job) -> Result<()> {
        self.upsert(job)
    }

    fn update(&self, job: &Job) -> Result<()> {
        self.upsert(job)
    }

    fn get(&self, job_id: JobId) -> Result<Option<Job>> {
        let data: Option<String> = self
            .conn()
            .query_row(
                "SELECT data FROM jobs WHERE job_id = ?1",
                [job_id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        data.map(|d| serde_json::from_str(&d).map_err(db_error))
            .transpose()
    }

    fn list(&self, filter: &JobFilter) -> Result<Vec<Job>> {
        let mut sql = String::from("SELECT data FROM jobs WHERE 1 = 1");
        let mut args: Vec<Value> = Vec::new();
        if let Some(client) = &filter.client_id {
            sql.push_str(" AND client_id = ?");
            args.push(client.clone().into());
        }
        if let Some(printer) = &filter.printer_id {
            sql.push_str(" AND printer_id = ?");
            args.push(printer.0.clone().into());
        }
        if !filter.statuses.is_empty() {
            let marks = vec!["?"; filter.statuses.len()].join(", ");
            sql.push_str(&format!(" AND status IN ({marks})"));
            args.extend(
                filter
                    .statuses
                    .iter()
                    .map(|s| Value::from(s.as_str().to_owned())),
            );
        }
        if let Some(since) = filter.since {
            sql.push_str(" AND created_at >= ?");
            args.push(ts(since).into());
        }
        if let Some(until) = filter.until {
            sql.push_str(" AND created_at < ?");
            args.push(ts(until).into());
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
        args.push((filter.effective_limit() as i64).into());
        args.push((filter.offset as i64).into());
        self.query_jobs(&sql, args)
    }

    fn find_by_idempotency_key(&self, client_id: &str, key: &str) -> Result<Option<Job>> {
        Ok(self
            .query_jobs(
                "SELECT data FROM jobs WHERE client_id = ? AND idempotency_key = ?",
                vec![client_id.to_owned().into(), key.to_owned().into()],
            )?
            .into_iter()
            .next())
    }

    fn non_terminal(&self) -> Result<Vec<Job>> {
        let terminal = [
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ];
        self.query_jobs(
            "SELECT data FROM jobs WHERE status NOT IN (?, ?, ?) ORDER BY created_at",
            terminal
                .iter()
                .map(|s| Value::from(s.as_str().to_owned()))
                .collect(),
        )
    }

    fn purge_terminal_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.conn()
            .execute(
                "DELETE FROM jobs WHERE created_at < ?1 AND status IN ('COMPLETED', 'FAILED', 'CANCELLED')",
                [ts(cutoff)],
            )
            .map(|n| n as u64)
            .map_err(db_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::model::{DocumentType, PrinterId};

    fn job(client: &str, printer: &str, status: JobStatus) -> Job {
        let mut j = Job::new(client, DocumentType::Raw, 1, 10);
        j.printer_id = Some(PrinterId::from(printer));
        j.status = status;
        j
    }

    #[test]
    fn round_trips_and_filters() {
        let db = Database::in_memory().expect("db");
        let a = job("c1", "p1", JobStatus::Completed);
        let mut b = job("c2", "p2", JobStatus::Queued);
        b.created_at = a.created_at + chrono::Duration::seconds(1);
        db.insert(&a).expect("insert");
        db.insert(&b).expect("insert");

        assert_eq!(db.get(a.job_id).expect("get"), Some(a.clone()));
        let all = db.list(&JobFilter::default()).expect("list");
        assert_eq!(
            all.iter().map(|j| j.job_id).collect::<Vec<_>>(),
            vec![b.job_id, a.job_id],
            "newest first"
        );

        let by_client = db
            .list(&JobFilter {
                client_id: Some("c1".into()),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(by_client.len(), 1);
        let by_status = db
            .list(&JobFilter {
                statuses: vec![JobStatus::Queued],
                ..Default::default()
            })
            .expect("list");
        assert_eq!(by_status[0].job_id, b.job_id);
        let by_time = db
            .list(&JobFilter {
                since: Some(b.created_at),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(by_time.len(), 1);

        assert_eq!(db.non_terminal().expect("nt").len(), 1);
    }

    #[test]
    fn updates_replace_state() {
        let db = Database::in_memory().expect("db");
        let mut j = job("c", "p", JobStatus::Queued);
        db.insert(&j).expect("insert");
        j.status = JobStatus::Failed;
        db.update(&j).expect("update");
        assert_eq!(
            db.get(j.job_id).expect("get").map(|j| j.status),
            Some(JobStatus::Failed)
        );
        assert!(db.non_terminal().expect("nt").is_empty());
    }

    #[test]
    fn idempotency_lookup_and_uniqueness() {
        let db = Database::in_memory().expect("db");
        let mut a = job("c", "p", JobStatus::Queued);
        a.idempotency_key = Some("k1".into());
        db.insert(&a).expect("insert");
        assert_eq!(
            db.find_by_idempotency_key("c", "k1")
                .expect("find")
                .map(|j| j.job_id),
            Some(a.job_id)
        );
        assert!(
            db.find_by_idempotency_key("other", "k1")
                .expect("find")
                .is_none()
        );
        let mut dup = job("c", "p", JobStatus::Queued);
        dup.idempotency_key = Some("k1".into());
        assert!(
            db.insert(&dup).is_err(),
            "unique index guards against duplicate keys"
        );
    }

    #[test]
    fn purge_keeps_active_jobs() {
        let db = Database::in_memory().expect("db");
        let old_done = job("c", "p", JobStatus::Completed);
        let old_active = job("c", "p", JobStatus::Queued);
        db.insert(&old_done).expect("insert");
        db.insert(&old_active).expect("insert");
        let removed = db
            .purge_terminal_before(Utc::now() + chrono::Duration::seconds(1))
            .expect("purge");
        assert_eq!(removed, 1);
        assert!(db.get(old_active.job_id).expect("get").is_some());
    }

    #[test]
    fn audit_log_round_trip() {
        let db = Database::in_memory().expect("db");
        db.audit(
            "auth.failure",
            None,
            Some("https://x"),
            &serde_json::json!({"reason": "bad token"}),
        );
        let entries = db.audit_entries(10).expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event, "auth.failure");
    }

    #[test]
    fn reopening_a_database_keeps_data() {
        let dir = std::env::temp_dir().join(format!("kiln-db-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("kiln.db");
        let j = job("c", "p", JobStatus::Completed);
        {
            let db = Database::open(&path).expect("open");
            db.insert(&j).expect("insert");
        }
        let db = Database::open(&path).expect("reopen");
        assert!(db.get(j.job_id).expect("get").is_some());
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
