//! One Lightstreamer session over a rustls WebSocket: connect, subscribe, stream updates.
//!
//! Failure is loud by design: any protocol error, server `END`, closed socket or silence longer
//! than the negotiated keep-alive allows ends the session with an error. Reconnection policy is
//! deliberately not in the spike; it belongs to the M3 adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
};

use crate::config::Config;
use crate::tlcp::{self, FieldState, Notification, Subscribe};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Smallest stall timeout, so a tiny negotiated keep-alive cannot cause spurious failures.
const MIN_STALL_TIMEOUT: Duration = Duration::from_secs(2);

/// A subscription as sent: its ID, and the item/field names in the order the server will index them.
struct SubDef {
    id: u32,
    items: Vec<String>,
    fields: Vec<String>,
}

/// One decoded update for one item.
pub struct Update<'a> {
    /// Receive time at the client. The server timestamps live in the field values.
    pub rx: DateTime<Utc>,
    pub item_id: &'a str,
    pub fields: &'a [String],
    /// Current values of every field of the item after applying the update.
    pub values: &'a [Option<String>],
    /// Indexes into `fields` that this update touched.
    pub changed: &'a [usize],
}

#[derive(Debug, Default)]
pub struct SessionSummary {
    pub session_id: String,
    pub keepalive_ms: u64,
    pub notifications: HashMap<String, u64>,
    pub connect_ms: u128,
    pub subscribe_ms: u128,
    pub ended_by: &'static str,
}

fn tls_connector() -> Result<Connector> {
    // The ring provider and bundled web PKI roots keep the build free of OpenSSL and of any
    // system certificate store, per NFR-1.
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("selecting TLS protocol versions")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(cfg)))
}

async fn connect(cfg: &Config) -> Result<Ws> {
    let mut req = cfg
        .server
        .url
        .as_str()
        .into_client_request()
        .with_context(|| format!("bad server url {}", cfg.server.url))?;
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(tlcp::SUBPROTOCOL),
    );
    let (ws, response) = timeout(
        Duration::from_secs_f64(cfg.run.connect_timeout_s),
        connect_async_tls_with_config(req, None, false, Some(tls_connector()?)),
    )
    .await
    .context("timed out connecting to the server")?
    .context("WebSocket connection failed")?;
    let agreed = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok());
    if agreed != Some(tlcp::SUBPROTOCOL) {
        bail!(
            "server did not agree to subprotocol {}; it answered {agreed:?}",
            tlcp::SUBPROTOCOL
        );
    }
    Ok(ws)
}

/// Read the next WebSocket text message and parse every notification line in it.
async fn next_notifications(ws: &mut Ws) -> Result<Vec<Notification>> {
    loop {
        let msg = ws
            .next()
            .await
            .ok_or_else(|| anyhow!("server closed the connection"))?
            .context("WebSocket read failed")?;
        match msg {
            Message::Text(text) => {
                return text
                    .as_str()
                    .split("\r\n")
                    .filter(|l| !l.is_empty())
                    .map(|l| tlcp::parse_notification(l).map_err(anyhow::Error::from))
                    .collect();
            }
            Message::Close(frame) => bail!("server sent a close frame: {frame:?}"),
            // Ping/pong are answered by the library; binary frames are not part of TLCP.
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            Message::Binary(b) => bail!("unexpected binary frame of {} bytes", b.len()),
        }
    }
}

/// Fail on notifications that mean the session cannot continue.
fn check_fatal(n: &Notification) -> Result<()> {
    match n {
        Notification::ConErr { code, message } => bail!("session refused: CONERR {code} {message}"),
        Notification::Error { code, message } => bail!("server ERROR {code}: {message}"),
        Notification::ReqErr {
            req_id,
            code,
            message,
        } => bail!("request {req_id} failed: REQERR {code} {message}"),
        Notification::End { code, message } => {
            bail!("server ended the session: END {code} {message}")
        }
        _ => Ok(()),
    }
}

fn tag_of(n: &Notification) -> String {
    match n {
        Notification::WsOk => "WSOK".into(),
        Notification::ConOk { .. } => "CONOK".into(),
        Notification::ConErr { .. } => "CONERR".into(),
        Notification::ReqOk { .. } => "REQOK".into(),
        Notification::ReqErr { .. } => "REQERR".into(),
        Notification::Error { .. } => "ERROR".into(),
        Notification::SubOk { .. } => "SUBOK".into(),
        Notification::Update { .. } => "U".into(),
        Notification::Probe => "PROBE".into(),
        Notification::Loop { .. } => "LOOP".into(),
        Notification::End { .. } => "END".into(),
        Notification::Other { tag, .. } => tag.clone(),
    }
}

/// Connect, subscribe and stream updates to `sink` until `duration_s` elapses or Ctrl-C.
pub async fn run(cfg: &Config, mut sink: impl FnMut(&Update<'_>)) -> Result<SessionSummary> {
    let mut summary = SessionSummary::default();
    let t0 = std::time::Instant::now();
    let mut ws = connect(cfg).await?;
    tracing::info!(url = %cfg.server.url, "WebSocket connected, subprotocol agreed");

    ws.send(Message::text(tlcp::create_session_request(
        &cfg.server.adapter_set,
    )))
    .await
    .context("sending create_session")?;
    let connect_deadline = Instant::now() + Duration::from_secs_f64(cfg.run.connect_timeout_s);
    let (session_id, keepalive_ms) = 'conok: loop {
        let batch = timeout_at(connect_deadline, next_notifications(&mut ws))
            .await
            .context("timed out waiting for CONOK")??;
        for n in batch {
            check_fatal(&n)?;
            *summary.notifications.entry(tag_of(&n)).or_default() += 1;
            if let Notification::ConOk {
                session_id,
                keepalive_ms,
                request_limit,
                control_link,
            } = n
            {
                tracing::info!(%session_id, keepalive_ms, request_limit, %control_link, "session created");
                break 'conok (session_id, keepalive_ms);
            }
        }
    };
    summary.session_id = session_id;
    summary.keepalive_ms = keepalive_ms;
    summary.connect_ms = t0.elapsed().as_millis();

    // Mimic subscribes the telemetry items with (TimeStamp, Value) and the TIME item with four fields.
    let data_items: Vec<String> = cfg.items.iter().map(|i| i.id.clone()).collect();
    let subs = [
        SubDef {
            id: 1,
            items: data_items,
            fields: cfg.fields.data.clone(),
        },
        SubDef {
            id: 2,
            items: vec![cfg.time_item.id.clone()],
            fields: cfg.fields.time.clone(),
        },
    ];
    let mut req_id = 0u32;
    for s in &subs {
        req_id += 1;
        let req = tlcp::subscribe_request(&Subscribe {
            req_id,
            sub_id: s.id,
            items: &s.items,
            fields: &s.fields,
            snapshot: true,
            data_adapter: cfg.server.data_adapter.as_deref(),
        });
        ws.send(Message::text(req))
            .await
            .context("sending subscription")?;
    }

    let mut states: HashMap<(u32, usize), FieldState> = HashMap::new();
    let mut pending: Vec<u32> = subs.iter().map(|s| s.id).collect();
    // The server accepts item names it does not know and simply never sends data for them (observed
    // against ISSLIVE: SUBOK reported the full item count). Requiring a snapshot per item turns a
    // typo in the item list from silent missing data into a startup error.
    let mut awaiting_snapshot: std::collections::BTreeSet<String> =
        subs.iter().flat_map(|s| s.items.iter().cloned()).collect();
    let setup_deadline = Instant::now() + Duration::from_secs_f64(cfg.run.setup_timeout_s);
    let t_sub = std::time::Instant::now();
    let stall = Duration::from_millis((keepalive_ms as f64 * cfg.run.stall_factor) as u64)
        .max(MIN_STALL_TIMEOUT);
    let end = Instant::now() + Duration::from_secs_f64(cfg.run.duration_s);
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
    summary.ended_by = "duration";

    loop {
        let setup_done = pending.is_empty() && awaiting_snapshot.is_empty();
        let wait_until = if setup_done {
            end
        } else {
            setup_deadline.min(end)
        };
        let batch = tokio::select! {
            _ = &mut ctrl_c => { summary.ended_by = "ctrl-c"; break; }
            r = timeout_at(wait_until, timeout(stall, next_notifications(&mut ws))) => match r {
                Err(_) if setup_done => break,
                Err(_) => bail!(
                    "setup incomplete after {} s: subscriptions not confirmed {pending:?}; items with no snapshot {awaiting_snapshot:?} (unknown item names are accepted by the server but never delivered)",
                    cfg.run.setup_timeout_s
                ),
                Ok(Err(_)) => bail!("feed stalled: nothing received for {} ms (keep-alive {keepalive_ms} ms x {})", stall.as_millis(), cfg.run.stall_factor),
                Ok(Ok(batch)) => batch?,
            }
        };
        let rx = Utc::now();
        for n in batch {
            check_fatal(&n)?;
            *summary.notifications.entry(tag_of(&n)).or_default() += 1;
            match n {
                Notification::SubOk {
                    sub_id,
                    items,
                    fields,
                } => {
                    let def = subs
                        .iter()
                        .find(|s| s.id == sub_id)
                        .ok_or_else(|| anyhow!("SUBOK for unknown subscription {sub_id}"))?;
                    if items != def.items.len() || fields != def.fields.len() {
                        bail!(
                            "subscription {sub_id}: server resolved {items} items x {fields} fields, we asked for {} x {}",
                            def.items.len(),
                            def.fields.len()
                        );
                    }
                    pending.retain(|&p| p != sub_id);
                    tracing::info!(sub_id, items, fields, "subscribed");
                }
                Notification::Update {
                    sub_id,
                    item,
                    values,
                } => {
                    let def = subs
                        .iter()
                        .find(|s| s.id == sub_id)
                        .ok_or_else(|| anyhow!("update for unknown subscription {sub_id}"))?;
                    let item_id = item
                        .checked_sub(1)
                        .and_then(|i| def.items.get(i))
                        .ok_or_else(|| {
                            anyhow!(
                                "update for item {item} outside subscription {sub_id} ({} items)",
                                def.items.len()
                            )
                        })?;
                    let state = states
                        .entry((sub_id, item))
                        .or_insert_with(|| FieldState::new(def.fields.len()));
                    let changed = state
                        .apply(&values)
                        .with_context(|| format!("decoding update for {item_id}"))?;
                    awaiting_snapshot.remove(item_id);
                    if pending.is_empty()
                        && awaiting_snapshot.is_empty()
                        && summary.subscribe_ms == 0
                    {
                        summary.subscribe_ms = t_sub.elapsed().as_millis().max(1);
                    }
                    sink(&Update {
                        rx,
                        item_id,
                        fields: &def.fields,
                        values: state.values(),
                        changed: &changed,
                    });
                }
                Notification::Loop { expected_delay_ms } => bail!(
                    "server asked for a rebind (LOOP {expected_delay_ms} ms), which only happens on HTTP streaming; unexpected on WebSocket"
                ),
                Notification::Other { tag, args }
                    if matches!(
                        tag.as_str(),
                        "SERVNAME"
                            | "CLIENTIP"
                            | "CONS"
                            | "CONF"
                            | "SYNC"
                            | "PROG"
                            | "NOOP"
                            | "EOS"
                            | "CS"
                    ) =>
                {
                    tracing::debug!(%tag, %args, "informational notification");
                }
                Notification::Other { tag, args } => {
                    tracing::warn!(%tag, %args, "unhandled notification")
                }
                _ => {}
            }
        }
    }

    // Best-effort clean shutdown; failure here cannot change what was already measured.
    req_id += 1;
    if let Err(e) = ws.send(Message::text(tlcp::destroy_request(req_id))).await {
        tracing::warn!(error = %e, "could not send destroy request");
    }
    let _ = ws.close(None).await;
    Ok(summary)
}
