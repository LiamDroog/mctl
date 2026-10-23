//! On-disk HTTP cache with ETag / Last-Modified revalidation (DESIGN.md §3.3).
//!
//! Each key is a body file plus a small JSON metadata file. A refresh inside the minimum interval
//! does no network I/O at all; otherwise it sends a conditional request, so an unchanged resource
//! costs a `304`. Files are written to a temporary name and renamed, so a crash cannot leave a
//! half-written cache entry.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Largest response accepted, bytes. The biggest real source is the 3.7 MB EOP file.
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cache metadata for {key} is corrupt: {reason}")]
    Corrupt { key: String, reason: String },
    #[error("GET {url}: {reason}")]
    Network { url: String, reason: String },
    #[error("GET {url}: HTTP {status}")]
    Http { url: String, status: u16 },
    #[error("GET {url}: empty body")]
    Empty { url: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Unix seconds when the body was last downloaded (changed).
    pub fetched_at: f64,
    /// Unix seconds of the last successful contact with the server (200 or 304).
    pub checked_at: f64,
}

#[derive(Debug, Clone)]
pub struct CachedFile {
    pub body: String,
    pub meta: Meta,
}

#[derive(Debug)]
pub enum Refresh {
    /// Inside the minimum interval: no request was made.
    NotDue,
    /// Server answered 304: cache is current.
    NotModified,
    /// New body downloaded and stored.
    Updated(CachedFile),
}

pub struct HttpCache {
    dir: PathBuf,
    agent: ureq::Agent,
    user_agent: String,
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> CacheError + '_ {
    move |source| CacheError::Io {
        path: path.to_owned(),
        source,
    }
}

impl HttpCache {
    pub fn new(dir: PathBuf, user_agent: &str, timeout_s: f64) -> Result<Self, CacheError> {
        std::fs::create_dir_all(&dir).map_err(io(&dir))?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs_f64(timeout_s)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            dir,
            agent,
            user_agent: user_agent.to_owned(),
        })
    }

    fn body_path(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.meta.json"))
    }

    /// The cached copy, if any. A body without metadata (or the reverse) is an error, not a guess.
    pub fn load(&self, key: &str) -> Result<Option<CachedFile>, CacheError> {
        let (bp, mp) = (self.body_path(key), self.meta_path(key));
        match (bp.exists(), mp.exists()) {
            (false, false) => Ok(None),
            (true, true) => {
                let body = std::fs::read_to_string(&bp).map_err(io(&bp))?;
                let meta = serde_json::from_str(&std::fs::read_to_string(&mp).map_err(io(&mp))?)
                    .map_err(|e| CacheError::Corrupt {
                        key: key.into(),
                        reason: e.to_string(),
                    })?;
                Ok(Some(CachedFile { body, meta }))
            }
            _ => Err(CacheError::Corrupt {
                key: key.into(),
                reason: "body and metadata files are not both present".into(),
            }),
        }
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CacheError> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(io(&tmp))?;
        std::fs::rename(&tmp, path).map_err(io(path))
    }

    fn store(&self, key: &str, body: &str, meta: &Meta) -> Result<(), CacheError> {
        Self::write_atomic(&self.body_path(key), body.as_bytes())?;
        let json = serde_json::to_vec_pretty(meta).expect("Meta serialises");
        Self::write_atomic(&self.meta_path(key), &json)
    }

    /// Revalidate `key` from `url` unless it was checked within `min_interval_s` of `now` (unix seconds).
    pub fn refresh(
        &self,
        key: &str,
        url: &str,
        min_interval_s: f64,
        now: f64,
    ) -> Result<Refresh, CacheError> {
        let cached = self.load(key)?;
        if let Some(c) = &cached
            && now - c.meta.checked_at < min_interval_s
        {
            return Ok(Refresh::NotDue);
        }
        let mut req = self.agent.get(url).header("User-Agent", &self.user_agent);
        if let Some(c) = &cached {
            if let Some(e) = &c.meta.etag {
                req = req.header("If-None-Match", e);
            }
            if let Some(m) = &c.meta.last_modified {
                req = req.header("If-Modified-Since", m);
            }
        }
        let net = |e: &dyn std::fmt::Display| CacheError::Network {
            url: url.to_owned(),
            reason: e.to_string(),
        };
        let mut resp = req.call().map_err(|e| net(&e))?;
        let status = resp.status().as_u16();
        match status {
            304 => {
                let mut c = cached.ok_or_else(|| CacheError::Http {
                    url: url.to_owned(),
                    status,
                })?;
                c.meta.checked_at = now;
                self.store(key, &c.body, &c.meta)?;
                Ok(Refresh::NotModified)
            }
            200 => {
                let header = |n: &str| {
                    resp.headers()
                        .get(n)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned)
                };
                let (etag, last_modified) = (header("ETag"), header("Last-Modified"));
                let mut body = String::new();
                resp.body_mut()
                    .as_reader()
                    .take(MAX_BODY_BYTES)
                    .read_to_string(&mut body)
                    .map_err(|e| net(&e))?;
                if body.trim().is_empty() {
                    return Err(CacheError::Empty {
                        url: url.to_owned(),
                    });
                }
                let meta = Meta {
                    etag,
                    last_modified,
                    fetched_at: now,
                    checked_at: now,
                };
                self.store(key, &body, &meta)?;
                Ok(Refresh::Updated(CachedFile { body, meta }))
            }
            s => Err(CacheError::Http {
                url: url.to_owned(),
                status: s,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A tiny HTTP server: answers 200 with an ETag, or 304 if `If-None-Match` matches, or a forced status.
    struct Mock {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        status_override: Arc<Mutex<Option<u16>>>,
    }

    fn mock() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/data", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let status_override = Arc::new(Mutex::new(None));
        let (r2, s2) = (requests.clone(), status_override.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut inm = None;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("if-none-match:") {
                        inm = Some(v.trim().to_owned());
                    }
                }
                r2.lock().unwrap().push(inm.clone().unwrap_or_default());
                let forced = *s2.lock().unwrap();
                let resp = match (forced, inm.as_deref()) {
                    (Some(c), _) => format!("HTTP/1.1 {c} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                    (None, Some("\"v1\"")) => "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n".to_owned(),
                    _ => "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nLast-Modified: Wed, 21 Oct 2026 16:13:43 GMT\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_owned(),
                };
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });
        Mock {
            url,
            requests,
            status_override,
        }
    }

    fn cache() -> (HttpCache, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "mctl-cache-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (HttpCache::new(dir.clone(), "mctl-test", 5.0).unwrap(), dir)
    }

    #[test]
    fn download_then_conditional_revalidation_then_not_due() {
        let m = mock();
        let (c, dir) = cache();
        assert!(c.load("k").unwrap().is_none());
        let Refresh::Updated(f) = c.refresh("k", &m.url, 100.0, 1000.0).unwrap() else {
            panic!("first fetch must download")
        };
        assert_eq!(
            (f.body.as_str(), f.meta.etag.as_deref()),
            ("hello", Some("\"v1\""))
        );
        // Inside the interval: no request at all.
        assert!(matches!(
            c.refresh("k", &m.url, 100.0, 1050.0).unwrap(),
            Refresh::NotDue
        ));
        assert_eq!(m.requests.lock().unwrap().len(), 1);
        // Past the interval: a conditional request that comes back 304 and advances checked_at only.
        assert!(matches!(
            c.refresh("k", &m.url, 100.0, 1200.0).unwrap(),
            Refresh::NotModified
        ));
        assert_eq!(
            m.requests.lock().unwrap()[1],
            "\"v1\"",
            "ETag must be sent back"
        );
        let loaded = c.load("k").unwrap().unwrap();
        assert_eq!(
            (loaded.meta.fetched_at, loaded.meta.checked_at),
            (1000.0, 1200.0)
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn http_errors_and_unreachable_hosts_are_errors_and_keep_the_old_copy() {
        let m = mock();
        let (c, dir) = cache();
        c.refresh("k", &m.url, 0.0, 1.0).unwrap();
        *m.status_override.lock().unwrap() = Some(503);
        assert!(matches!(
            c.refresh("k", &m.url, 0.0, 2.0),
            Err(CacheError::Http { status: 503, .. })
        ));
        assert_eq!(
            c.load("k").unwrap().unwrap().body,
            "hello",
            "a failed refresh must not damage the cache"
        );
        assert!(matches!(
            c.refresh("other", "http://127.0.0.1:1/x", 0.0, 3.0),
            Err(CacheError::Network { .. })
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn half_written_cache_is_corrupt_not_silently_ignored() {
        let (c, dir) = cache();
        std::fs::write(dir.join("k"), "body only").unwrap();
        assert!(matches!(c.load("k"), Err(CacheError::Corrupt { .. })));
        std::fs::remove_dir_all(dir).ok();
    }
}
