use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    Queued,
    Delayed,
    Leased,
    AwaitingOutput,
    Written,
    Skipped,
    TerminalError,
}

pub type PageStatus = PageState;

impl PageState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delayed => "delayed",
            Self::Leased => "leased",
            Self::AwaitingOutput => "awaiting_output",
            Self::Written => "written",
            Self::Skipped => "skipped",
            Self::TerminalError => "terminal_error",
        }
    }

    fn parse(value: &str) -> Result<Self, StateError> {
        match value {
            "queued" => Ok(Self::Queued),
            "delayed" => Ok(Self::Delayed),
            "leased" => Ok(Self::Leased),
            "awaiting_output" => Ok(Self::AwaitingOutput),
            "written" => Ok(Self::Written),
            "skipped" => Ok(Self::Skipped),
            "terminal_error" => Ok(Self::TerminalError),
            other => Err(StateError::InvalidState(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRecord {
    pub page_id: i64,
    pub canonical_url: String,
    pub depth: u32,
    pub parent_url: Option<String>,
    pub discovery_source: Option<String>,
    pub state: PageState,
    pub attempts: u32,
    pub next_eligible_at: i64,
    pub lease_token: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub final_url: Option<String>,
    pub status_code: Option<u16>,
    pub output_path: Option<String>,
    pub digest: Option<String>,
    pub bytes: Option<u64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    pub page_id: i64,
    pub inserted: bool,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub page_id: i64,
    pub canonical_url: String,
    pub depth: u32,
    pub attempts: u32,
    pub lease_token: String,
    pub lease_expires_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StateCounts {
    pub total: u64,
    pub queued: u64,
    pub delayed: u64,
    pub leased: u64,
    pub awaiting_output: u64,
    pub written: u64,
    pub skipped: u64,
    pub terminal_error: u64,
}

#[derive(Debug)]
pub enum StateError {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    InvalidInput(String),
    InvalidState(String),
    NotFound(i64),
    LeaseConflict(i64),
    ConfigMismatch { expected: String, actual: String },
}

impl fmt::Display for StateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "SQLite error: {error}"),
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::InvalidInput(error) => formatter.write_str(error),
            Self::InvalidState(state) => write!(formatter, "invalid page state `{state}`"),
            Self::NotFound(page_id) => write!(formatter, "page {page_id} was not found"),
            Self::LeaseConflict(page_id) => write!(formatter, "lease conflict for page {page_id}"),
            Self::ConfigMismatch { expected, actual } => write!(
                formatter,
                "output directory was created with a different configuration \
                 (stored `{actual}`, requested `{expected}`); re-run with --fresh to start over, \
                 or use a different --output directory"
            ),
        }
    }
}

impl std::error::Error for StateError {}
impl From<rusqlite::Error> for StateError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}
impl From<std::io::Error> for StateError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone)]
pub struct StateStore {
    connection: Arc<Mutex<Connection>>,
}

pub type Frontier = StateStore;

impl fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("StateStore").finish_non_exhaustive()
    }
}

impl StateStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut connection = Connection::open(path)?;
        Self::configure(&connection)?;
        Self::initialize(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn in_memory() -> Result<Self, StateError> {
        let mut connection = Connection::open_in_memory()?;
        Self::configure(&connection)?;
        Self::initialize(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn open_in_memory() -> Result<Self, StateError> {
        Self::in_memory()
    }

    fn configure(connection: &Connection) -> Result<(), StateError> {
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;",
        )?;
        Ok(())
    }

    fn initialize(connection: &mut Connection) -> Result<(), StateError> {
        let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if current > SCHEMA_VERSION {
            return Err(StateError::InvalidInput(format!(
                "state schema version {current} is newer than supported version {SCHEMA_VERSION}"
            )));
        }
        if current == SCHEMA_VERSION {
            return Ok(());
        }
        let transaction = connection.transaction()?;
        transaction.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS pages (
                page_id INTEGER PRIMARY KEY,
                canonical_url TEXT NOT NULL UNIQUE,
                depth INTEGER NOT NULL CHECK (depth >= 0),
                parent_url TEXT,
                discovery_source TEXT,
                state TEXT NOT NULL CHECK (state IN ('queued','delayed','leased','awaiting_output','written','skipped','terminal_error')),
                attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
                next_eligible_at INTEGER NOT NULL DEFAULT 0,
                lease_token TEXT,
                lease_expires_at INTEGER,
                final_url TEXT,
                status_code INTEGER,
                output_path TEXT,
                digest TEXT,
                bytes INTEGER,
                error TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS pages_ready_idx
                ON pages (state, next_eligible_at, depth, canonical_url);
            CREATE INDEX IF NOT EXISTS pages_lease_idx
                ON pages (state, lease_expires_at);
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            PRAGMA user_version = {SCHEMA_VERSION};"
        ))?;
        transaction.commit()?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StateError> {
        self.connection
            .lock()
            .map_err(|_| StateError::InvalidInput("state mutex was poisoned".to_owned()))
    }

    pub fn schema_version(&self) -> Result<i64, StateError> {
        let connection = self.lock()?;
        Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn admit_url(
        &self,
        canonical_url: &str,
        depth: u32,
        parent_url: Option<&str>,
        discovery_source: Option<&str>,
    ) -> Result<Admission, StateError> {
        if canonical_url.is_empty() {
            return Err(StateError::InvalidInput(
                "canonical URL must not be empty".to_owned(),
            ));
        }
        let mut connection = self.lock()?;
        let now = unix_time_ms()?;
        let transaction = connection.transaction()?;
        let was_present: Option<i64> = transaction
            .query_row(
                "SELECT page_id FROM pages WHERE canonical_url = ?1",
                [canonical_url],
                |row| row.get(0),
            )
            .optional()?;
        transaction.execute(
            "INSERT INTO pages (
                canonical_url, depth, parent_url, discovery_source, state,
                next_eligible_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?5, ?5)
             ON CONFLICT(canonical_url) DO UPDATE SET
                depth = MIN(pages.depth, excluded.depth),
                parent_url = CASE WHEN excluded.depth < pages.depth THEN excluded.parent_url ELSE pages.parent_url END,
                discovery_source = CASE WHEN excluded.depth < pages.depth THEN excluded.discovery_source ELSE pages.discovery_source END,
                updated_at = CASE WHEN excluded.depth < pages.depth THEN excluded.updated_at ELSE pages.updated_at END",
            params![canonical_url, i64::from(depth), parent_url, discovery_source, now],
        )?;
        let (page_id, stored_depth): (i64, i64) = transaction.query_row(
            "SELECT page_id, depth FROM pages WHERE canonical_url = ?1",
            [canonical_url],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        transaction.commit()?;
        Ok(Admission {
            page_id,
            inserted: was_present.is_none(),
            depth: u32::try_from(stored_depth).map_err(|_| {
                StateError::InvalidInput("stored page depth is outside u32 range".to_owned())
            })?,
        })
    }

    pub fn admit(
        &self,
        canonical_url: &str,
        depth: u32,
        parent_url: Option<&str>,
        discovery_source: Option<&str>,
    ) -> Result<Admission, StateError> {
        self.admit_url(canonical_url, depth, parent_url, discovery_source)
    }

    pub fn page(&self, page_id: i64) -> Result<Option<PageRecord>, StateError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT page_id, canonical_url, depth, parent_url, discovery_source,
                        state, attempts, next_eligible_at, lease_token, lease_expires_at,
                        final_url, status_code, output_path, digest, bytes, error,
                        created_at, updated_at
                 FROM pages WHERE page_id = ?1",
                [page_id],
                row_to_page,
            )
            .optional()
            .map_err(StateError::from)
    }

    pub fn page_by_url(&self, canonical_url: &str) -> Result<Option<PageRecord>, StateError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT page_id, canonical_url, depth, parent_url, discovery_source,
                        state, attempts, next_eligible_at, lease_token, lease_expires_at,
                        final_url, status_code, output_path, digest, bytes, error,
                        created_at, updated_at
                 FROM pages WHERE canonical_url = ?1",
                [canonical_url],
                row_to_page,
            )
            .optional()
            .map_err(StateError::from)
    }

    pub fn page_count(&self) -> Result<u64, StateError> {
        let connection = self.lock()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM pages", [], |row| row.get(0))?;
        u64::try_from(count).map_err(|_| StateError::InvalidInput("negative page count".to_owned()))
    }

    pub fn lease_batch(
        &self,
        now_ms: i64,
        limit: usize,
        lease_duration_ms: i64,
    ) -> Result<Vec<Lease>, StateError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if lease_duration_ms <= 0 {
            return Err(StateError::InvalidInput(
                "lease duration must be greater than zero".to_owned(),
            ));
        }
        let limit = i64::try_from(limit)
            .map_err(|_| StateError::InvalidInput("lease limit is too large".to_owned()))?;
        let expires_at = now_ms
            .checked_add(lease_duration_ms)
            .ok_or_else(|| StateError::InvalidInput("lease expiry overflows i64".to_owned()))?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let mut selected = Vec::new();
        {
            let mut statement = transaction.prepare(
                "SELECT page_id, canonical_url, depth, attempts
                 FROM pages
                 WHERE state IN ('queued', 'delayed') AND next_eligible_at <= ?1
                 ORDER BY depth ASC, canonical_url ASC
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(params![now_ms, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                selected.push(row?);
            }
        }
        let mut leases = Vec::with_capacity(selected.len());
        for (page_id, canonical_url, depth, attempts) in selected {
            let token = lease_token(page_id, now_ms, attempts);
            transaction.execute(
                "UPDATE pages
                 SET state = 'leased', lease_token = ?1, lease_expires_at = ?2,
                     updated_at = ?3
                 WHERE page_id = ?4",
                params![token, expires_at, now_ms, page_id],
            )?;
            leases.push(Lease {
                page_id,
                canonical_url,
                depth: u32::try_from(depth).map_err(|_| {
                    StateError::InvalidInput("stored page depth is outside u32 range".to_owned())
                })?,
                attempts: u32::try_from(attempts).map_err(|_| {
                    StateError::InvalidInput("stored attempt count is outside u32 range".to_owned())
                })?,
                lease_token: token,
                lease_expires_at: expires_at,
            });
        }
        transaction.commit()?;
        Ok(leases)
    }

    pub fn lease_ready(
        &self,
        now_ms: i64,
        limit: usize,
        lease_duration_ms: i64,
    ) -> Result<Vec<Lease>, StateError> {
        self.lease_batch(now_ms, limit, lease_duration_ms)
    }

    pub fn record_attempt(
        &self,
        page_id: i64,
        lease_token: &str,
        now_ms: i64,
    ) -> Result<(), StateError> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE pages SET attempts = attempts + 1, error = NULL, updated_at = ?1
             WHERE page_id = ?2 AND state = 'leased' AND lease_token = ?3
               AND lease_expires_at > ?1",
            params![now_ms, page_id, lease_token],
        )?;
        if changed == 0 {
            return Err(StateError::LeaseConflict(page_id));
        }
        Ok(())
    }

    pub fn schedule_retry(
        &self,
        page_id: i64,
        lease_token: &str,
        now_ms: i64,
        next_eligible_at: i64,
        error: &str,
    ) -> Result<(), StateError> {
        self.update_leased(
            page_id,
            lease_token,
            "delayed",
            now_ms,
            Some(next_eligible_at),
            Some(error),
        )
    }

    pub fn retry(
        &self,
        page_id: i64,
        lease_token: &str,
        now_ms: i64,
        next_eligible_at: i64,
        error: &str,
    ) -> Result<(), StateError> {
        self.schedule_retry(page_id, lease_token, now_ms, next_eligible_at, error)
    }

    fn update_leased(
        &self,
        page_id: i64,
        lease_token: &str,
        state: &str,
        now_ms: i64,
        next_eligible_at: Option<i64>,
        error: Option<&str>,
    ) -> Result<(), StateError> {
        if !matches!(state, "delayed" | "awaiting_output" | "terminal_error") {
            return Err(StateError::InvalidInput(
                "invalid lease transition".to_owned(),
            ));
        }
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE pages
             SET state = ?1, next_eligible_at = COALESCE(?2, next_eligible_at),
                 lease_token = NULL, lease_expires_at = NULL, error = ?3, updated_at = ?4
             WHERE page_id = ?5 AND state = 'leased' AND lease_token = ?6
               AND lease_expires_at > ?4",
            params![state, next_eligible_at, error, now_ms, page_id, lease_token],
        )?;
        if changed == 0 {
            return Err(StateError::LeaseConflict(page_id));
        }
        Ok(())
    }

    pub fn mark_awaiting_output(
        &self,
        page_id: i64,
        lease_token: &str,
        now_ms: i64,
    ) -> Result<(), StateError> {
        self.update_leased(page_id, lease_token, "awaiting_output", now_ms, None, None)
    }

    pub fn mark_terminal_error(
        &self,
        page_id: i64,
        lease_token: &str,
        now_ms: i64,
        error: &str,
    ) -> Result<(), StateError> {
        self.update_leased(
            page_id,
            lease_token,
            "terminal_error",
            now_ms,
            None,
            Some(error),
        )
    }

    pub fn mark_skipped(&self, page_id: i64, reason: &str) -> Result<(), StateError> {
        let now = unix_time_ms()?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE pages SET state = 'skipped', error = ?1, lease_token = NULL,
                    lease_expires_at = NULL, updated_at = ?3
             WHERE page_id = ?2 AND state NOT IN ('written', 'terminal_error')",
            params![reason, page_id, now],
        )?;
        if changed == 0 {
            return Err(StateError::NotFound(page_id));
        }
        Ok(())
    }

    pub fn mark_written(
        &self,
        page_id: i64,
        output_path: &str,
        digest: &str,
        bytes: u64,
        now_ms: i64,
    ) -> Result<(), StateError> {
        let bytes = i64::try_from(bytes)
            .map_err(|_| StateError::InvalidInput("output size is too large".to_owned()))?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE pages SET state = 'written', output_path = ?1, digest = ?2, bytes = ?3,
                    lease_token = NULL, lease_expires_at = NULL, updated_at = ?4
             WHERE page_id = ?5 AND state IN ('awaiting_output', 'leased')
               AND (state = 'awaiting_output' OR lease_expires_at > ?6)",
            params![output_path, digest, bytes, now_ms, page_id, now_ms],
        )?;
        if changed == 0 {
            return Err(StateError::NotFound(page_id));
        }
        Ok(())
    }

    pub fn record_response(
        &self,
        page_id: i64,
        lease_token: &str,
        final_url: Option<&str>,
        status_code: Option<u16>,
        now_ms: i64,
    ) -> Result<(), StateError> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE pages SET final_url = ?1, status_code = ?2, updated_at = ?3
             WHERE page_id = ?4 AND state = 'leased' AND lease_token = ?5
               AND lease_expires_at > ?3",
            params![
                final_url,
                status_code.map(i64::from),
                now_ms,
                page_id,
                lease_token
            ],
        )?;
        if changed == 0 {
            return Err(StateError::LeaseConflict(page_id));
        }
        Ok(())
    }

    pub fn recover_expired_leases(&self, now_ms: i64) -> Result<u64, StateError> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE pages SET state = 'queued', lease_token = NULL, lease_expires_at = NULL,
                    next_eligible_at = MIN(next_eligible_at, ?1), updated_at = ?1
             WHERE state = 'leased' AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?1",
            [now_ms],
        )?;
        Ok(changed as u64)
    }

    /// Minimum `next_eligible_at` across delayed pages, or 0 when none.
    pub fn earliest_next_eligible_at(&self) -> Result<i64, StateError> {
        let connection = self.lock()?;
        let earliest: Option<i64> = connection.query_row(
            "SELECT MIN(next_eligible_at) FROM pages WHERE state = 'delayed'",
            [],
            |row| row.get(0),
        )?;
        Ok(earliest.unwrap_or(0))
    }

    pub fn counts(&self) -> Result<StateCounts, StateError> {
        let connection = self.lock()?;
        let mut counts = StateCounts::default();
        let mut statement =
            connection.prepare("SELECT state, COUNT(*) FROM pages GROUP BY state")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (state, count) = row?;
            let count = u64::try_from(count)
                .map_err(|_| StateError::InvalidInput("negative state count".to_owned()))?;
            counts.total += count;
            match state.as_str() {
                "queued" => counts.queued = count,
                "delayed" => counts.delayed = count,
                "leased" => counts.leased = count,
                "awaiting_output" => counts.awaiting_output = count,
                "written" => counts.written = count,
                "skipped" => counts.skipped = count,
                "terminal_error" => counts.terminal_error = count,
                other => return Err(StateError::InvalidState(other.to_owned())),
            }
        }
        Ok(counts)
    }

    pub fn config_fingerprint(&self) -> Result<Option<String>, StateError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'config_fingerprint'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StateError::from)
    }

    pub fn ensure_config_fingerprint(
        &self,
        fingerprint: &str,
    ) -> Result<Option<String>, StateError> {
        if fingerprint.is_empty() {
            return Err(StateError::InvalidInput(
                "config fingerprint must not be empty".to_owned(),
            ));
        }
        let connection = self.lock()?;
        let stored: Option<String> = connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'config_fingerprint'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match stored {
            None => {
                connection.execute(
                    "INSERT INTO metadata (key, value) VALUES ('config_fingerprint', ?1)",
                    [fingerprint],
                )?;
                Ok(None)
            }
            Some(value) if value == fingerprint => Ok(Some(value)),
            Some(value) => Err(StateError::ConfigMismatch {
                expected: fingerprint.to_owned(),
                actual: value,
            }),
        }
    }

    pub fn set_config_fingerprint(&self, fingerprint: &str) -> Result<Option<String>, StateError> {
        self.ensure_config_fingerprint(fingerprint)
    }

    pub fn fingerprint_for<T: Serialize>(config: &T) -> Result<String, StateError> {
        let bytes = serde_json::to_vec(config).map_err(|error| {
            StateError::InvalidInput(format!("cannot serialize config: {error}"))
        })?;
        Ok(hex_digest(&bytes))
    }
}

fn row_to_page(row: &Row<'_>) -> rusqlite::Result<PageRecord> {
    let depth: i64 = row.get(2)?;
    let attempts: i64 = row.get(6)?;
    let status_code: Option<i64> = row.get(11)?;
    let bytes: Option<i64> = row.get(14)?;
    Ok(PageRecord {
        page_id: row.get(0)?,
        canonical_url: row.get(1)?,
        depth: u32::try_from(depth)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, depth))?,
        parent_url: row.get(3)?,
        discovery_source: row.get(4)?,
        state: PageState::parse(&row.get::<_, String>(5)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        attempts: u32::try_from(attempts)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, attempts))?,
        next_eligible_at: row.get(7)?,
        lease_token: row.get(8)?,
        lease_expires_at: row.get(9)?,
        final_url: row.get(10)?,
        status_code: status_code
            .map(|value| {
                u16::try_from(value)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, value))
            })
            .transpose()?,
        output_path: row.get(12)?,
        digest: row.get(13)?,
        bytes: bytes
            .map(|value| {
                u64::try_from(value)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(14, value))
            })
            .transpose()?,
        error: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn lease_token(page_id: i64, now_ms: i64, attempts: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(page_id.to_le_bytes());
    hasher.update(now_ms.to_le_bytes());
    hasher.update(attempts.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn unix_time_ms() -> Result<i64, StateError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            StateError::InvalidInput(format!("system clock is before Unix epoch: {error}"))
        })?;
    i64::try_from(duration.as_millis())
        .map_err(|_| StateError::InvalidInput("system clock exceeds i64 milliseconds".to_owned()))
}
