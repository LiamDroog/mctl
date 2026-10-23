//! The ISSLIVE feed as an engine source: one thread running the TLCP session, reconnecting with
//! backoff, and forwarding every item update as an [`Event`]. Failures are reported as health, never
//! swallowed (DESIGN.md R1: pages degrade to NO DATA, trajectory views do not depend on this).

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;

use mctl_core::config::Fetch;
use mctl_core::time::Utc;
use mctl_core::world::Health;

use super::config::Config;
use super::session;
use crate::tasks::Event;

/// A session is asked to run this long; it is re-established when it ends for any reason.
const SESSION_SECONDS: f64 = 86_400.0;
/// How often a running session checks whether the owner went away, milliseconds.
const STOP_POLL_MS: u64 = 500;

fn stopped(stop: &Receiver<()>) -> bool {
    matches!(
        stop.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected) | Ok(())
    )
}

pub fn run(
    mut cfg: Config,
    tx: &SyncSender<Event>,
    stop: &Receiver<()>,
    fetch: &Fetch,
    offline: bool,
) {
    let health = |h| tx.send(Event::TelemetryHealth(h)).is_ok();
    if offline {
        health(Health::Offline);
        let _ = stop.recv();
        return;
    }
    tracing::info!(url = %cfg.server.url, "telemetry feed starting");
    cfg.run.duration_s = SESSION_SECONDS;
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "cannot start the telemetry runtime");
            health(Health::Failing(format!("runtime: {e}")));
            return;
        }
    };
    let mut backoff = fetch.backoff_initial_s;
    loop {
        if !health(Health::Starting) {
            return;
        }
        let started = std::time::Instant::now();
        let result = rt.block_on(async {
            let mut announced = false;
            let sess = session::run(&cfg, |u| {
                if !announced {
                    announced = true;
                    let _ = tx.send(Event::TelemetryHealth(Health::Ok));
                }
                let text = u
                    .fields
                    .iter()
                    .position(|f| f == "Value")
                    .and_then(|i| u.values[i].clone());
                let _ = tx.send(Event::Telemetry {
                    id: u.item_id.to_owned(),
                    text,
                    rx: Utc::now().unwrap_or(Utc(0.0)),
                });
            });
            tokio::pin!(sess);
            loop {
                tokio::select! {
                    r = &mut sess => break Some(r),
                    () = tokio::time::sleep(Duration::from_millis(STOP_POLL_MS)) => {
                        if stopped(stop) { break None; }
                    }
                }
            }
        });
        let Some(result) = result else { return };
        let why = match result {
            Ok(s) => format!("session ended ({})", s.ended_by),
            Err(e) => format!("{e:#}"),
        };
        tracing::warn!(reason = %why, "telemetry session ended; reconnecting");
        // A session that stayed up for a while resets the backoff.
        if started.elapsed().as_secs_f64() > 60.0 {
            backoff = fetch.backoff_initial_s;
        }
        if !health(Health::Failing(why)) {
            return;
        }
        match stop.recv_timeout(Duration::from_secs_f64(backoff)) {
            Err(RecvTimeoutError::Timeout) => backoff = (backoff * 2.0).min(fetch.backoff_max_s),
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mctl_core::config::Config as AppConfig;
    use std::sync::mpsc::sync_channel;

    /// Network test: run with `cargo test -p mctl-sources feed_delivers -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the network"]
    fn feed_delivers_updates_from_the_live_server() {
        let app = AppConfig::from_layers(mctl_core::config::DEFAULT_CONFIG, None).unwrap();
        let cfg = Config::parse("test", super::super::config::DEFAULT_CONFIG).unwrap();
        let (tx, rx) = sync_channel(256);
        let (stop_tx, stop_rx) = sync_channel::<()>(1);
        let fetch = app.fetch.clone();
        let h = std::thread::spawn(move || run(cfg, &tx, &stop_rx, &fetch, false));
        let t0 = std::time::Instant::now();
        let mut updates = 0;
        while t0.elapsed().as_secs() < 20 {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Event::Telemetry { .. }) => updates += 1,
                Ok(Event::TelemetryHealth(h)) => println!("health {h:?} at {:?}", t0.elapsed()),
                _ => {}
            }
        }
        drop(stop_tx);
        h.join().unwrap();
        assert!(updates > 10, "only {updates} updates");
    }
}
