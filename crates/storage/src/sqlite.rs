use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Row, Sqlite};
use ticket_core::ports::{KnowledgeBase, TicketRepository};
use ticket_core::{
    ClientId, Embedding, Error, ExternalRef, KnowledgeEntry, Resolution, Result, SimilarTicket,
    Ticket, TicketId, TicketStatus, TriageReport, top_k,
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

const TICKET_COLUMNS: &str = "id, client_id, external_system, external_key, title, description, \
     reporter, labels, reported_severity, status, created_at, updated_at";

/// A SQLite-backed implementation of both storage ports.
///
/// Similarity search is a brute-force cosine scan in Rust. That is fine up to
/// tens of thousands of entries. Beyond that, put [`KnowledgeBase`] on a
/// vector index (`sqlite-vec`, `pgvector`, Qdrant) behind the same trait.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Opens (creating if needed) the database at `url` and runs migrations.
    ///
    /// `url` is a SQLite URL such as `sqlite://data/tickets.db`.
    pub async fn connect(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|e| Error::Config(format!("invalid database url `{url}`: {e}")))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .map_err(Error::storage)?;
        Self::from_pool(pool).await
    }

    /// Opens a private in-memory database. Intended for tests.
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(Error::storage)?
            .foreign_keys(true);
        // Each in-memory connection is a separate database, so keep exactly one alive.
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
            .map_err(Error::storage)?;
        Self::from_pool(pool).await
    }

    /// Wraps an existing pool and applies pending migrations.
    pub async fn from_pool(pool: SqlitePool) -> Result<Self> {
        MIGRATOR.run(&pool).await.map_err(Error::storage)?;
        Ok(Self { pool })
    }

    /// The underlying connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

fn col<'r, T>(row: &'r SqliteRow, name: &str) -> Result<T>
where
    T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite>,
{
    row.try_get(name).map_err(Error::storage)
}

fn map_write_error(err: sqlx::Error, what: impl FnOnce() -> String) -> Error {
    match &err {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            Error::Conflict(format!("{} already exists", what()))
        }
        sqlx::Error::Database(db) if db.is_foreign_key_violation() => Error::NotFound(what()),
        _ => Error::storage(err),
    }
}

fn ticket_from_row(row: &SqliteRow) -> Result<Ticket> {
    let id: String = col(row, "id")?;
    let client: String = col(row, "client_id")?;
    let system: Option<String> = col(row, "external_system")?;
    let key: Option<String> = col(row, "external_key")?;
    let labels: String = col(row, "labels")?;
    let reported_severity: Option<String> = col(row, "reported_severity")?;
    let status: String = col(row, "status")?;

    Ok(Ticket {
        id: id.parse()?,
        client: ClientId::new(client)?,
        external_ref: system
            .zip(key)
            .map(|(system, key)| ExternalRef { system, key }),
        title: col(row, "title")?,
        description: col(row, "description")?,
        reporter: col(row, "reporter")?,
        labels: serde_json::from_str(&labels).map_err(Error::storage)?,
        reported_severity: reported_severity.as_deref().map(str::parse).transpose()?,
        status: status.parse()?,
        created_at: col::<DateTime<Utc>>(row, "created_at")?,
        updated_at: col::<DateTime<Utc>>(row, "updated_at")?,
    })
}

fn knowledge_from_row(row: &SqliteRow) -> Result<(KnowledgeEntry, Embedding)> {
    let ticket_id: String = col(row, "ticket_id")?;
    let category: Option<String> = col(row, "category")?;
    let severity: String = col(row, "severity")?;
    let embedding: Vec<u8> = col(row, "embedding")?;

    let entry = KnowledgeEntry {
        ticket_id: ticket_id.parse()?,
        title: col(row, "title")?,
        description: col(row, "description")?,
        category: category.map(ticket_core::Category::new).transpose()?,
        severity: severity.parse()?,
        resolution: Resolution {
            summary: col(row, "resolution_summary")?,
            automatable: col(row, "automatable")?,
            resolved_by: col(row, "resolved_by")?,
        },
        updated_at: col::<DateTime<Utc>>(row, "updated_at")?,
    };
    Ok((entry, Embedding::from_le_bytes(&embedding)?))
}

#[async_trait]
impl TicketRepository for SqliteStore {
    async fn insert(&self, ticket: &Ticket) -> Result<()> {
        let labels = serde_json::to_string(&ticket.labels).map_err(Error::storage)?;
        let external = ticket.external_ref.as_ref();
        sqlx::query(
            "INSERT INTO tickets (id, client_id, external_system, external_key, title, \
             description, reporter, labels, reported_severity, status, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ticket.id.to_string())
        .bind(ticket.client.as_str())
        .bind(external.map(|r| r.system.as_str()))
        .bind(external.map(|r| r.key.as_str()))
        .bind(&ticket.title)
        .bind(&ticket.description)
        .bind(ticket.reporter.as_deref())
        .bind(labels)
        .bind(ticket.reported_severity.map(|s| s.as_str()))
        .bind(ticket.status.as_str())
        .bind(ticket.created_at)
        .bind(ticket.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_write_error(e, || format!("ticket {}", ticket.id)))?;
        Ok(())
    }

    async fn get(&self, id: TicketId) -> Result<Option<Ticket>> {
        let sql = format!("SELECT {TICKET_COLUMNS} FROM tickets WHERE id = ?");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::storage)?
            .as_ref()
            .map(ticket_from_row)
            .transpose()
    }

    async fn find_by_external_ref(&self, external: &ExternalRef) -> Result<Option<Ticket>> {
        let sql = format!(
            "SELECT {TICKET_COLUMNS} FROM tickets WHERE external_system = ? AND external_key = ?"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&external.system)
            .bind(&external.key)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::storage)?
            .as_ref()
            .map(ticket_from_row)
            .transpose()
    }

    async fn set_status(&self, id: TicketId, status: TicketStatus) -> Result<()> {
        let result = sqlx::query("UPDATE tickets SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(Utc::now())
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(Error::storage)?;
        if result.rows_affected() == 0 {
            return Err(Error::NotFound(format!("ticket {id}")));
        }
        Ok(())
    }

    async fn transition(
        &self,
        id: TicketId,
        from: &[TicketStatus],
        to: TicketStatus,
    ) -> Result<bool> {
        if from.is_empty() {
            return Ok(false);
        }
        // Only `?` placeholders are interpolated; every value is bound.
        let placeholders = vec!["?"; from.len()].join(", ");
        let sql = format!(
            "UPDATE tickets SET status = ?, updated_at = ? WHERE id = ? AND status IN ({placeholders})"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(to.as_str())
            .bind(Utc::now())
            .bind(id.to_string());
        for status in from {
            query = query.bind(status.as_str());
        }
        let result = query.execute(&self.pool).await.map_err(Error::storage)?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_by_status(&self, status: TicketStatus, limit: u32) -> Result<Vec<Ticket>> {
        let sql = format!(
            "SELECT {TICKET_COLUMNS} FROM tickets WHERE status = ? ORDER BY created_at, id LIMIT ?"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(status.as_str())
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(Error::storage)?
            .iter()
            .map(ticket_from_row)
            .collect()
    }

    async fn save_triage(&self, report: &TriageReport) -> Result<()> {
        let json = serde_json::to_string(report).map_err(Error::storage)?;
        sqlx::query("INSERT INTO triage_reports (ticket_id, report, decided_at) VALUES (?, ?, ?)")
            .bind(report.ticket_id.to_string())
            .bind(json)
            .bind(report.decided_at)
            .execute(&self.pool)
            .await
            .map_err(|e| map_write_error(e, || format!("ticket {}", report.ticket_id)))?;
        Ok(())
    }

    async fn latest_triage(&self, id: TicketId) -> Result<Option<TriageReport>> {
        let row = sqlx::query(
            "SELECT report FROM triage_reports WHERE ticket_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::storage)?;
        row.map(|row| {
            let json: String = col(&row, "report")?;
            serde_json::from_str(&json).map_err(Error::storage)
        })
        .transpose()
    }
}

#[async_trait]
impl KnowledgeBase for SqliteStore {
    async fn upsert(&self, entry: &KnowledgeEntry, embedding: &Embedding) -> Result<()> {
        sqlx::query(
            "INSERT INTO knowledge (ticket_id, title, description, category, severity, \
             resolution_summary, automatable, resolved_by, embedding_dim, embedding, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (ticket_id) DO UPDATE SET \
               title = excluded.title, description = excluded.description, \
               category = excluded.category, severity = excluded.severity, \
               resolution_summary = excluded.resolution_summary, \
               automatable = excluded.automatable, resolved_by = excluded.resolved_by, \
               embedding_dim = excluded.embedding_dim, embedding = excluded.embedding, \
               updated_at = excluded.updated_at",
        )
        .bind(entry.ticket_id.to_string())
        .bind(&entry.title)
        .bind(&entry.description)
        .bind(entry.category.as_ref().map(|c| c.as_str()))
        .bind(entry.severity.as_str())
        .bind(&entry.resolution.summary)
        .bind(entry.resolution.automatable)
        .bind(entry.resolution.resolved_by.as_deref())
        .bind(i64::try_from(embedding.dim()).map_err(Error::storage)?)
        .bind(embedding.to_le_bytes())
        .bind(entry.updated_at)
        .execute(&self.pool)
        .await
        .map_err(Error::storage)?;
        Ok(())
    }

    async fn search(&self, query: &Embedding, limit: usize) -> Result<Vec<SimilarTicket>> {
        let rows = sqlx::query("SELECT * FROM knowledge WHERE embedding_dim = ?")
            .bind(i64::try_from(query.dim()).map_err(Error::storage)?)
            .fetch_all(&self.pool)
            .await
            .map_err(Error::storage)?;
        let mut scored = Vec::with_capacity(rows.len());
        for row in &rows {
            let (entry, embedding) = knowledge_from_row(row)?;
            let score = embedding.cosine(query);
            scored.push((entry, score));
        }
        Ok(top_k(scored, limit)
            .into_iter()
            .map(|(entry, score)| SimilarTicket { entry, score })
            .collect())
    }

    async fn count(&self) -> Result<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&self.pool)
            .await
            .map_err(Error::storage)?;
        u64::try_from(count).map_err(Error::storage)
    }
}
