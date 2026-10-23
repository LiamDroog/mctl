//! SQLite recording (DESIGN.md §10, FR-60..62): states, telemetry, element sets, events and C&W log,
//! in WAL mode, with retention by age and size and CSV export.
//!
//! Writing happens on a dedicated thread fed by a channel, so the engine never waits on the disk.
//! Replay (FR-63) reads this schema and arrives with the sim milestone.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mctl_core::time::Utc;
use rusqlite::{Connection, params};

/// Schema version stored in `PRAGMA user_version`; a different value is refused, not migrated silently.
pub const SCHEMA_VERSION: i64 = 1;
/// Records are committed at least this often, seconds.
const COMMIT_EVERY_S: f64 = 1.0;
/// Records per transaction at most.
const COMMIT_MAX_RECORDS: usize = 2_000;
/// How often retention runs while recording, seconds.
const RETENTION_EVERY_S: f64 = 3_600.0;
/// Fraction of the oldest rows deleted per pass when the file is over its size limit.
const SIZE_PRUNE_FRACTION: f64 = 0.1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database {path}: {source}")]
    Db {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("database {path} has schema version {found}, this build expects {SCHEMA_VERSION}")]
    Version { path: PathBuf, found: i64 },
    #[error("creating {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("unknown table {0:?}; known: {known}", known = TABLES.join(", "))]
    UnknownTable(String),
    #[error("writing CSV: {0}")]
    Csv(#[from] std::io::Error),
}

pub const TABLES: &[&str] = &["state", "telemetry", "elements", "events", "cw_log"];

/// One thing to record.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    State {
        vehicle: String,
        source: String,
        t: Utc,
        frame: &'static str,
        r_km: [f64; 3],
        v_km_s: [f64; 3],
        quality: String,
    },
    Telemetry {
        item_id: String,
        t_rx: Utc,
        value: Option<f64>,
        text: Option<String>,
    },
    Elements {
        vehicle: String,
        source: String,
        epoch: Option<Utc>,
        fetched_at: Utc,
        blob: String,
    },
    Event {
        t: Utc,
        vehicle: Option<String>,
        kind: String,
        payload: String,
    },
    CwLog {
        t: Utc,
        rule: String,
        from_state: String,
        to_state: String,
        value: Option<f64>,
        acked_at: Option<Utc>,
    },
}

impl Record {
    /// An event row whose payload is `{"text": ...}`.
    pub fn log_event(t: Utc, vehicle: Option<&str>, kind: &str, text: &str) -> Self {
        Self::Event {
            t,
            vehicle: vehicle.map(str::to_owned),
            kind: kind.to_owned(),
            payload: serde_json::json!({ "text": text }).to_string(),
        }
    }
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
                path: dir.to_owned(),
                source,
            })?;
        }
        let db = |source| StoreError::Db {
            path: path.to_owned(),
            source,
        };
        let conn = Connection::open(path).map_err(db)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db)?;
        let found: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(db)?;
        let fresh = found == 0 && !Self::has_tables(&conn).map_err(db)?;
        if !fresh && found != SCHEMA_VERSION {
            return Err(StoreError::Version {
                path: path.to_owned(),
                found,
            });
        }
        if fresh {
            conn.execute_batch(SCHEMA).map_err(db)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(db)?;
        }
        Ok(Self {
            conn,
            path: path.to_owned(),
        })
    }

    fn has_tables(conn: &Connection) -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert records in one transaction.
    pub fn write(&mut self, records: &[Record]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for r in records {
            match r {
                Record::State { vehicle, source, t, frame, r_km, v_km_s, quality } => tx.prepare_cached("INSERT INTO state (vehicle, source, t_utc, frame, rx, ry, rz, vx, vy, vz, quality) VALUES (?,?,?,?,?,?,?,?,?,?,?)")?.execute(params![vehicle, source, t.0, frame, r_km[0], r_km[1], r_km[2], v_km_s[0], v_km_s[1], v_km_s[2], quality])?,
                Record::Telemetry { item_id, t_rx, value, text } => tx.prepare_cached("INSERT INTO telemetry (item_id, t_rx, value_real, value_text) VALUES (?,?,?,?)")?.execute(params![item_id, t_rx.0, value, text])?,
                Record::Elements { vehicle, source, epoch, fetched_at, blob } => tx.prepare_cached("INSERT INTO elements (vehicle, source, epoch, fetched_at, blob) VALUES (?,?,?,?,?)")?.execute(params![vehicle, source, epoch.map(|e| e.0), fetched_at.0, blob])?,
                Record::Event { t, vehicle, kind, payload } => tx.prepare_cached("INSERT INTO events (t_utc, vehicle, kind, payload) VALUES (?,?,?,?)")?.execute(params![t.0, vehicle, kind, payload])?,
                Record::CwLog { t, rule, from_state, to_state, value, acked_at } => tx.prepare_cached("INSERT INTO cw_log (t_utc, rule, from_state, to_state, value, acked_at) VALUES (?,?,?,?,?,?)")?.execute(params![t.0, rule, from_state, to_state, value, acked_at.map(|a| a.0)])?,
            };
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self, table: &str) -> Result<i64, StoreError> {
        let t = check_table(table)?;
        Ok(self
            .conn
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))?)
    }

    /// Database size in bytes as SQLite sees it (pages in use plus free pages).
    pub fn size_bytes(&self) -> Result<u64, StoreError> {
        let pages: i64 = self
            .conn
            .pragma_query_value(None, "page_count", |r| r.get(0))?;
        let size: i64 = self
            .conn
            .pragma_query_value(None, "page_size", |r| r.get(0))?;
        Ok((pages * size) as u64)
    }

    /// Delete rows older than `max_age_s` before `now`. Element sets are kept twice as long, since
    /// they are small and the only record of what the sources said. Returns rows deleted.
    pub fn prune_by_age(&mut self, now: Utc, max_age_s: f64) -> Result<usize, StoreError> {
        let cut = now.0 - max_age_s;
        let cut_elements = now.0 - 2.0 * max_age_s;
        let mut n = 0;
        for (table, col, cut) in [
            ("state", "t_utc", cut),
            ("telemetry", "t_rx", cut),
            ("events", "t_utc", cut),
            ("cw_log", "t_utc", cut),
            ("elements", "fetched_at", cut_elements),
        ] {
            n += self
                .conn
                .execute(&format!("DELETE FROM {table} WHERE {col} < ?"), [cut])?;
        }
        Ok(n)
    }

    /// Delete the oldest rows of the bulky tables until the database is at most `max_bytes`, or until
    /// there is nothing bulky left to delete. Returns rows deleted.
    pub fn prune_by_size(&mut self, max_bytes: u64) -> Result<usize, StoreError> {
        let mut deleted = 0;
        // Freed pages are reused by later writes; the file does not shrink without a VACUUM, so the
        // size that matters is the used part: pages minus the free list.
        loop {
            let free: i64 = self
                .conn
                .pragma_query_value(None, "freelist_count", |r| r.get(0))?;
            let page: i64 = self
                .conn
                .pragma_query_value(None, "page_size", |r| r.get(0))?;
            let used = self.size_bytes()?.saturating_sub((free * page) as u64);
            if used <= max_bytes {
                return Ok(deleted);
            }
            let mut pass = 0;
            for (table, col) in [("telemetry", "t_rx"), ("state", "t_utc")] {
                let n: i64 =
                    self.conn
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
                let k = ((n as f64 * SIZE_PRUNE_FRACTION).ceil() as i64)
                    .max(1)
                    .min(n);
                if k > 0 {
                    pass += self.conn.execute(&format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} ORDER BY {col} LIMIT ?)"), [k])?;
                }
            }
            if pass == 0 {
                return Ok(deleted);
            }
            deleted += pass;
        }
    }

    /// Write `table` rows with `from <= time < to` as CSV (header first) and return the row count.
    pub fn export_csv(
        &self,
        table: &str,
        from: Utc,
        to: Utc,
        out: &mut impl std::io::Write,
    ) -> Result<usize, StoreError> {
        let t = check_table(table)?;
        let time_col = match t {
            "telemetry" => "t_rx",
            "elements" => "fetched_at",
            _ => "t_utc",
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT * FROM {t} WHERE {time_col} >= ?1 AND {time_col} < ?2 ORDER BY {time_col}"
        ))?;
        let names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        writeln!(out, "{}", names.join(","))?;
        let mut rows = stmt.query(params![from.0, to.0])?;
        let mut n = 0;
        while let Some(row) = rows.next()? {
            let cells: Vec<String> = (0..names.len())
                .map(|i| {
                    use rusqlite::types::ValueRef::{Blob, Integer, Null, Real, Text};
                    match row.get_ref(i) {
                        Ok(Null) | Err(_) => String::new(),
                        Ok(Integer(v)) => v.to_string(),
                        Ok(Real(v)) => format!("{v}"),
                        Ok(Text(v)) => csv_field(&String::from_utf8_lossy(v)),
                        Ok(Blob(v)) => format!("<{} bytes>", v.len()),
                    }
                })
                .collect();
            writeln!(out, "{}", cells.join(","))?;
            n += 1;
        }
        Ok(n)
    }
}

fn check_table(t: &str) -> Result<&'static str, StoreError> {
    TABLES
        .iter()
        .find(|k| **k == t)
        .copied()
        .ok_or_else(|| StoreError::UnknownTable(t.to_owned()))
}

/// RFC 4180 quoting: a field with a comma, quote or newline is quoted and its quotes doubled.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

const SCHEMA: &str = "
CREATE TABLE state (vehicle TEXT NOT NULL, source TEXT NOT NULL, t_utc REAL NOT NULL, frame TEXT NOT NULL,
  rx REAL, ry REAL, rz REAL, vx REAL, vy REAL, vz REAL, quality TEXT NOT NULL);
CREATE INDEX state_t ON state (t_utc);
CREATE TABLE telemetry (item_id TEXT NOT NULL, t_rx REAL NOT NULL, value_real REAL, value_text TEXT);
CREATE INDEX telemetry_t ON telemetry (t_rx);
CREATE TABLE elements (vehicle TEXT NOT NULL, source TEXT NOT NULL, epoch REAL, fetched_at REAL NOT NULL, blob TEXT NOT NULL);
CREATE TABLE events (t_utc REAL NOT NULL, vehicle TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL);
CREATE INDEX events_t ON events (t_utc);
CREATE TABLE cw_log (t_utc REAL NOT NULL, rule TEXT NOT NULL, from_state TEXT NOT NULL, to_state TEXT NOT NULL, value REAL, acked_at REAL);
CREATE INDEX cw_log_t ON cw_log (t_utc);
";

/// Handle to the writer thread. Dropping it flushes and stops the thread.
pub struct Recorder {
    tx: Option<Sender<Record>>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Start the writer thread. `retention_s` and `max_bytes` are applied at start and hourly.
    pub fn start(mut store: Store, retention_s: f64, max_bytes: u64) -> Self {
        let (tx, rx) = channel::<Record>();
        let thread = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || run_writer(&mut store, &rx, retention_s, max_bytes))
            .expect("spawn recorder thread");
        Self {
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    /// A sender for the engine. Sending fails only after the recorder has stopped.
    pub fn sender(&self) -> Sender<Record> {
        self.tx.clone().expect("recorder is running until dropped")
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run_writer(store: &mut Store, rx: &Receiver<Record>, retention_s: f64, max_bytes: u64) {
    let mut buf: Vec<Record> = Vec::new();
    let mut last_commit = Instant::now();
    let mut last_retention: Option<Instant> = None;
    loop {
        let wait = Duration::from_secs_f64(
            (COMMIT_EVERY_S - last_commit.elapsed().as_secs_f64()).max(0.0),
        );
        let closed = match rx.recv_timeout(wait) {
            Ok(r) => {
                buf.push(r);
                false
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => true,
        };
        if closed
            || buf.len() >= COMMIT_MAX_RECORDS
            || last_commit.elapsed().as_secs_f64() >= COMMIT_EVERY_S
        {
            if !buf.is_empty() {
                if let Err(e) = store.write(&buf) {
                    tracing::error!(error = %e, dropped = buf.len(), "recording failed; these records are lost");
                }
                buf.clear();
            }
            last_commit = Instant::now();
        }
        if last_retention.is_none_or(|t| t.elapsed().as_secs_f64() >= RETENTION_EVERY_S)
            && let Ok(now) = Utc::now()
        {
            last_retention = Some(Instant::now());
            match (
                store.prune_by_age(now, retention_s),
                store.prune_by_size(max_bytes),
            ) {
                (Ok(a), Ok(b)) if a + b > 0 => {
                    tracing::info!(by_age = a, by_size = b, "retention pruned rows")
                }
                (Err(e), _) | (_, Err(e)) => tracing::error!(error = %e, "retention failed"),
                _ => {}
            }
        }
        if closed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("sub/dir/mctl.db")).unwrap();
        (dir, s)
    }

    fn state(t: f64) -> Record {
        Record::State {
            vehicle: "iss".into(),
            source: "OEM".into(),
            t: Utc(t),
            frame: "GCRF",
            r_km: [7000.0, 1.0, 2.0],
            v_km_s: [0.0, 7.5, 0.0],
            quality: "PRED".into(),
        }
    }

    fn tel(t: f64, v: Option<f64>, text: Option<&str>) -> Record {
        Record::Telemetry {
            item_id: "USLAB000058".into(),
            t_rx: Utc(t),
            value: v,
            text: text.map(str::to_owned),
        }
    }

    #[test]
    fn creates_the_database_in_wal_mode_with_every_table() {
        let (_d, s) = store();
        let mode: String = s
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        for t in TABLES {
            assert_eq!(s.count(t).unwrap(), 0, "{t}");
        }
    }

    #[test]
    fn reopening_keeps_data_and_a_foreign_schema_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.db");
        let mut s = Store::open(&p).unwrap();
        s.write(&[state(1.0)]).unwrap();
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.count("state").unwrap(), 1);
        s.conn.pragma_update(None, "user_version", 99).unwrap();
        drop(s);
        match Store::open(&p) {
            Err(StoreError::Version { found: 99, .. }) => {}
            other => panic!("expected a version error, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn a_non_database_file_is_an_error_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.db");
        std::fs::write(
            &p,
            b"this is not sqlite, it is some other file of the user's",
        )
        .unwrap();
        assert!(Store::open(&p).is_err());
        assert!(
            std::fs::read(&p)
                .unwrap()
                .starts_with(b"this is not sqlite"),
            "the file is left alone"
        );
    }

    #[test]
    fn every_record_kind_round_trips_into_its_table() {
        let (_d, mut s) = store();
        s.write(&[
            state(10.0),
            tel(10.0, Some(749.0), None),
            tel(11.0, None, Some("ON")),
            Record::Elements {
                vehicle: "iss".into(),
                source: "GP".into(),
                epoch: Some(Utc(9.0)),
                fetched_at: Utc(10.0),
                blob: "{\"a\":1}".into(),
            },
            Record::Event {
                t: Utc(10.0),
                vehicle: Some("iss".into()),
                kind: "source".into(),
                payload: "{}".into(),
            },
            Record::CwLog {
                t: Utc(10.0),
                rule: "r".into(),
                from_state: "NOM".into(),
                to_state: "WARN".into(),
                value: Some(1.5),
                acked_at: None,
            },
        ])
        .unwrap();
        assert_eq!(
            [
                s.count("state"),
                s.count("telemetry"),
                s.count("elements"),
                s.count("events"),
                s.count("cw_log")
            ]
            .map(Result::unwrap),
            [1, 2, 1, 1, 1]
        );
        let (v, txt): (Option<f64>, Option<String>) = s
            .conn
            .query_row(
                "SELECT value_real, value_text FROM telemetry WHERE t_rx = 11",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (v, txt.as_deref()),
            (None, Some("ON")),
            "text and numeric values stay distinct"
        );
    }

    #[test]
    fn a_failed_batch_leaves_nothing_behind() {
        let (_d, mut s) = store();
        s.conn.execute_batch("DROP TABLE telemetry;").unwrap();
        assert!(s.write(&[state(1.0), tel(1.0, Some(1.0), None)]).is_err());
        assert_eq!(s.count("state").unwrap(), 0, "the transaction rolled back");
    }

    #[test]
    fn age_retention_deletes_old_rows_and_keeps_elements_twice_as_long() {
        let (_d, mut s) = store();
        let day = 86_400.0;
        let now = Utc(100.0 * day);
        let el = |t| Record::Elements {
            vehicle: "iss".into(),
            source: "GP".into(),
            epoch: None,
            fetched_at: Utc(t),
            blob: "x".into(),
        };
        s.write(&[
            state(now.0 - 40.0 * day),
            state(now.0 - 10.0 * day),
            tel(now.0 - 31.0 * day, Some(1.0), None),
            tel(now.0 - 1.0, Some(1.0), None),
            el(now.0 - 45.0 * day),
            el(now.0 - 70.0 * day),
        ])
        .unwrap();
        let n = s.prune_by_age(now, 30.0 * day).unwrap();
        assert_eq!(
            n, 3,
            "old state, old telemetry, and the 70-day-old element set"
        );
        assert_eq!(
            [
                s.count("state").unwrap(),
                s.count("telemetry").unwrap(),
                s.count("elements").unwrap()
            ],
            [1, 1, 1]
        );
    }

    #[test]
    fn size_retention_removes_the_oldest_bulky_rows_first() {
        let (_d, mut s) = store();
        let rows: Vec<Record> = (0..20_000)
            .map(|i| tel(f64::from(i), Some(f64::from(i)), None))
            .collect();
        s.write(&rows).unwrap();
        let before = s.size_bytes().unwrap();
        let limit = before / 2;
        let n = s.prune_by_size(limit).unwrap();
        assert!(n > 0);
        let oldest: f64 = s
            .conn
            .query_row("SELECT min(t_rx) FROM telemetry", [], |r| r.get(0))
            .unwrap();
        assert!(
            oldest > 0.0,
            "the oldest rows went first: min is now {oldest}"
        );
        let newest: f64 = s
            .conn
            .query_row("SELECT max(t_rx) FROM telemetry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(newest, 19_999.0);
        assert_eq!(
            s.prune_by_size(u64::MAX).unwrap(),
            0,
            "under the limit deletes nothing"
        );
    }

    #[test]
    fn csv_export_is_ranged_quoted_and_rejects_unknown_tables() {
        let (_d, mut s) = store();
        s.write(&[
            tel(1.0, Some(1.5), None),
            tel(2.0, None, Some("a,\"b\"")),
            tel(3.0, Some(3.0), None),
        ])
        .unwrap();
        let mut out = Vec::new();
        let n = s
            .export_csv("telemetry", Utc(1.0), Utc(3.0), &mut out)
            .unwrap();
        assert_eq!(n, 2, "to is exclusive");
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "item_id,t_rx,value_real,value_text");
        assert_eq!(lines[1], "USLAB000058,1,1.5,");
        assert_eq!(lines[2], "USLAB000058,2,,\"a,\"\"b\"\"\"");
        assert!(
            matches!(
                s.export_csv("sqlite_master", Utc(0.0), Utc(1.0), &mut Vec::new()),
                Err(StoreError::UnknownTable(_))
            ),
            "no arbitrary table names reach the SQL"
        );
        assert!(matches!(
            s.count("x; DROP TABLE state"),
            Err(StoreError::UnknownTable(_))
        ));
    }

    #[test]
    fn the_recorder_thread_commits_and_flushes_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.db");
        let rec = Recorder::start(Store::open(&p).unwrap(), 30.0 * 86_400.0, u64::MAX);
        let tx = rec.sender();
        let now = Utc::now().unwrap();
        for i in 0..50 {
            tx.send(tel(now.0 + f64::from(i), Some(f64::from(i)), None))
                .unwrap();
        }
        drop(tx);
        drop(rec);
        let s = Store::open(&p).unwrap();
        assert_eq!(
            s.count("telemetry").unwrap(),
            50,
            "nothing queued is lost on shutdown"
        );
    }
}
