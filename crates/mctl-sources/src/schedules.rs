//! Observing schedules published by STScI: JWST weekly visit lists and Hubble daily status reports.
//!
//! Both are scraped from public pages, so the parsers check what they depend on (column layout,
//! report headings) and fail loudly when the page no longer looks as expected.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Duration;

use mctl_core::config::{Config, Fetch, ScheduleSources};
use mctl_core::schedule::{HstProgram, HstReport, JwstSchedule, Ops, Visit};
use mctl_core::time::Utc;
use mctl_core::world::Health;
use thiserror::Error;

use crate::cache::{CacheError, HttpCache, Refresh};
use crate::tasks::{Event, SourceKey};

/// Weekly files read: the newest few cover the current week and the ones planned after it.
const JWST_FILES: usize = 3;

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error("{0}")]
    Format(String),
    #[error("no schedule files are linked from the index page")]
    NoFiles,
    #[error(transparent)]
    Cache(#[from] CacheError),
}

fn fmt_err<T>(msg: impl Into<String>) -> Result<T, ScheduleError> {
    Err(ScheduleError::Format(msg.into()))
}

// ---- JWST -------------------------------------------------------------------------------------------

/// File names (path relative to the site) of the weekly reports linked from the index page, newest
/// week first. The week is the `YYYYMMDD` that starts the file name.
pub fn jwst_files(index_html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = index_html;
    while let Some(i) = rest.find("href=\"") {
        rest = &rest[i + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = &rest[..end];
        let name = href.rsplit('/').next().unwrap_or_default();
        let is_report = name.len() > 20
            && name.ends_with(".txt")
            && name[..8].bytes().all(|b| b.is_ascii_digit())
            && name[8..].starts_with("_report_")
            && href.contains("observing-schedules");
        if is_report && !out.iter().any(|h| h == href) {
            out.push(href.to_owned());
        }
    }
    let week = |h: &String| h.rsplit('/').next().map(|n| n[..8].to_owned());
    out.sort_by_key(|h| std::cmp::Reverse((week(h), h.clone())));
    out
}

/// Column byte ranges taken from the dashed rule under the header row.
fn columns(rule: &str) -> Vec<(usize, usize)> {
    let mut cols = Vec::new();
    let mut start = None;
    for (i, b) in rule.bytes().enumerate() {
        match (b == b'-', start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                cols.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        cols.push((s, rule.len()));
    }
    cols
}

fn cell(line: &str, (a, b): (usize, usize)) -> &str {
    let end = b.min(line.len());
    if a >= end {
        return "";
    }
    line.get(a..end).unwrap_or_default().trim()
}

/// `DD/HH:MM:SS` in seconds.
fn parse_duration(s: &str) -> Option<f64> {
    let (d, hms) = s.split_once('/')?;
    let mut p = hms.split(':');
    let (h, m, sec) = (p.next()?, p.next()?, p.next()?);
    if p.next().is_some() {
        return None;
    }
    Some(
        d.parse::<f64>().ok()? * 86_400.0
            + h.parse::<f64>().ok()? * 3600.0
            + m.parse::<f64>().ok()? * 60.0
            + sec.parse::<f64>().ok()?,
    )
}

/// `2026-10-19T00:57:55Z`.
fn parse_start(s: &str) -> Option<Utc> {
    Utc::parse(s.strip_suffix('Z')?).ok()
}

const ATTACHED: &str = "^ATTACHED TO PRIME^";

/// Parse one weekly report file into visits. Parallel observations (`^ATTACHED TO PRIME^`, or
/// continuation lines without a visit id) are attached to the prime visit above them.
pub fn parse_jwst_week(text: &str) -> Result<Vec<Visit>, ScheduleError> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(h) = lines.iter().position(|l| l.starts_with("VISIT ID")) else {
        return fmt_err("no VISIT ID header row");
    };
    let Some(rule) = lines.get(h + 1).filter(|l| l.starts_with("---")) else {
        return fmt_err("no dashed rule under the header row");
    };
    let cols = columns(rule);
    let header = lines[h];
    let names = [
        "VISIT ID",
        "PCS MODE",
        "VISIT TYPE",
        "SCHEDULED START TIME",
        "DURATION",
        "SCIENCE INSTRUMENT AND MODE",
        "TARGET NAME",
        "CATEGORY",
    ];
    if cols.len() < names.len() {
        return fmt_err(format!(
            "{} columns, expected at least {}",
            cols.len(),
            names.len()
        ));
    }
    for (name, c) in names.iter().zip(&cols) {
        if cell(header, *c) != *name {
            return fmt_err(format!(
                "column heading {:?} is not {name:?}",
                cell(header, *c)
            ));
        }
    }
    let mut visits: Vec<Visit> = Vec::new();
    for (n, line) in lines[h + 2..].iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = |k: usize| cell(line, cols[k]);
        let (id, start, instrument) = (at(0), at(3), at(5));
        let parallel = start == ATTACHED || id.is_empty();
        if parallel {
            let Some(prime) = visits.last_mut() else {
                return fmt_err(format!(
                    "row {}: parallel observation before any prime visit",
                    n + 1
                ));
            };
            if !instrument.is_empty() {
                prime.parallels.push(instrument.to_owned());
            }
            continue;
        }
        let Some(t) = parse_start(start) else {
            return fmt_err(format!(
                "row {}: start time {start:?} is not YYYY-MM-DDThh:mm:ssZ",
                n + 1
            ));
        };
        let Some(d) = parse_duration(at(4)) else {
            return fmt_err(format!(
                "row {}: duration {:?} is not DD/HH:MM:SS",
                n + 1,
                at(4)
            ));
        };
        visits.push(Visit {
            id: id.to_owned(),
            start: t,
            duration_s: d,
            kind: at(2).to_owned(),
            pcs_mode: at(1).to_owned(),
            instrument: instrument.to_owned(),
            target: at(6).to_owned(),
            category: at(7).to_owned(),
            parallels: Vec::new(),
        });
    }
    Ok(visits)
}

/// Merge weekly files (newest first): a visit seen in a newer file replaces the same visit (same id
/// and start) in an older one, and the result is in start order.
pub fn merge_weeks(weeks_newest_first: Vec<Vec<Visit>>) -> Vec<Visit> {
    let mut out: Vec<Visit> = Vec::new();
    for week in weeks_newest_first {
        for v in week {
            if !out.iter().any(|o| o.id == v.id && o.start == v.start) {
                out.push(v);
            }
        }
    }
    out.sort_by(|a, b| a.start.0.total_cmp(&b.start.0));
    out
}

// ---- Hubble -----------------------------------------------------------------------------------------

/// Link (relative to the index) of the newest daily report on a year's list page.
pub fn hst_latest_report(index_html: &str) -> Option<String> {
    let mut best: Option<(String, String)> = None;
    let mut rest = index_html;
    while let Some(i) = rest.find("href=\"") {
        rest = &rest[i + 6..];
        let end = rest.find('"')?;
        let href = &rest[..end];
        let name = href.rsplit('/').next().unwrap_or_default();
        // hst_status_<year><doy>_<number>.html
        if let Some(core) = name
            .strip_prefix("hst_status_")
            .and_then(|n| n.strip_suffix(".html"))
            && let Some((date, num)) = core.split_once('_')
            && date.len() == 7
            && let Ok(num) = num.parse::<u32>()
        {
            let key = format!("{date}{num:08}");
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((key, href.to_owned()));
            }
        }
    }
    best.map(|(_, h)| h)
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse one Hubble daily status report page.
pub fn parse_hst_report(
    html: &str,
    url: &str,
    fetched_at: Utc,
) -> Result<HstReport, ScheduleError> {
    let number = html
        .find("Daily Report #")
        .and_then(|i| {
            let d: String = html[i + 14..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            d.parse::<u32>().ok()
        })
        .ok_or_else(|| ScheduleError::Format("no `Daily Report #n` heading".into()))?;
    let period = html
        .find("Period Covered:")
        .map(|i| {
            let end = html[i..].find("</p>").map_or(html.len(), |e| i + e);
            strip_tags(&html[i + 15..end].replace("<br>", " / "))
                .trim_start_matches(['/', ' '])
                .to_owned()
        })
        .ok_or_else(|| ScheduleError::Format("no `Period Covered:` line".into()))?;
    let (mut science, mut calibration, mut other) = (Vec::new(), Vec::new(), Vec::new());
    let mut section: Option<u8> = None;
    for row in html.split("<tr>").skip(1) {
        let row = row.split("</tr>").next().unwrap_or_default();
        let cells: Vec<String> = row
            .split("<td")
            .skip(1)
            .map(|c| strip_tags(c.split_once('>').map_or(c, |(_, rest)| rest)))
            .collect();
        match cells.as_slice() {
            [one] if one.starts_with("Science Observations") => section = Some(0),
            [one] if one.starts_with("Calibration Observations") => section = Some(1),
            [one] if one.starts_with("Other Observations") => section = Some(2),
            [id, pi, title] if id.bytes().all(|b| b.is_ascii_digit()) && !id.is_empty() => {
                let p = HstProgram {
                    id: id.clone(),
                    investigator: pi.clone(),
                    title: title.clone(),
                };
                match section {
                    Some(0) => science.push(p),
                    Some(1) => calibration.push(p),
                    Some(2) => other.push(p),
                    _ => {
                        return fmt_err(format!("program {id} appears before any section heading"));
                    }
                }
            }
            _ => {}
        }
    }
    if science.is_empty() && calibration.is_empty() && other.is_empty() {
        return fmt_err("report lists no programs");
    }
    Ok(HstReport {
        number,
        period,
        science,
        calibration,
        other,
        url: url.to_owned(),
        fetched_at,
    })
}

// ---- fetching ---------------------------------------------------------------------------------------

/// Body for `key`: from the network when `online` and due, else from the cache.
fn get(
    cache: &HttpCache,
    key: &str,
    url: &str,
    online: bool,
    interval_s: f64,
) -> Result<(String, Utc), ScheduleError> {
    let now = Utc::now().map_or(0.0, |u| u.0);
    if online {
        match cache.refresh(key, url, interval_s, now)? {
            Refresh::Updated(f) => return Ok((f.body, Utc(f.meta.fetched_at))),
            Refresh::NotDue | Refresh::NotModified => {}
        }
    }
    match cache.load(key)? {
        Some(f) => Ok((f.body, Utc(f.meta.fetched_at))),
        None => Err(ScheduleError::Format(format!(
            "{url}: not cached and offline"
        ))),
    }
}

fn fetch_jwst(
    cache: &HttpCache,
    s: &ScheduleSources,
    online: bool,
) -> Result<JwstSchedule, ScheduleError> {
    let (index, mut fetched) = get(
        cache,
        "jwst-index.html",
        &s.jwst_index_url,
        online,
        s.min_refresh_s,
    )?;
    let files = jwst_files(&index);
    if files.is_empty() {
        return Err(ScheduleError::NoFiles);
    }
    let (mut weeks, mut names) = (Vec::new(), Vec::new());
    for href in files.iter().take(JWST_FILES) {
        let name = href.rsplit('/').next().unwrap_or(href).to_owned();
        let url = if href.starts_with("http") {
            href.clone()
        } else {
            format!("{}{}", s.jwst_origin, href)
        };
        let (body, at) = get(
            cache,
            &format!("jwst-{name}"),
            &url,
            online,
            s.min_refresh_s,
        )?;
        fetched = Utc(fetched.0.max(at.0));
        weeks.push(
            parse_jwst_week(&body).map_err(|e| ScheduleError::Format(format!("{name}: {e}")))?,
        );
        names.push(name);
    }
    Ok(JwstSchedule {
        visits: merge_weeks(weeks),
        files: names,
        fetched_at: fetched,
    })
}

fn fetch_hst(
    cache: &HttpCache,
    s: &ScheduleSources,
    online: bool,
) -> Result<HstReport, ScheduleError> {
    let now = Utc::now().unwrap_or(Utc(0.0));
    let year = now.to_ymd_hms().0;
    // Early in January the new year's list can be empty; the previous year's is then the newest.
    let mut last_err = ScheduleError::NoFiles;
    for y in [year, year - 1] {
        let url = s.hst_index_template.replace("{year}", &y.to_string());
        let (index, _) = get(
            cache,
            &format!("hst-index-{y}.html"),
            &url,
            online,
            s.min_refresh_s,
        )?;
        let Some(href) = hst_latest_report(&index) else {
            continue;
        };
        let report_url = if href.starts_with("http") {
            href.clone()
        } else {
            // The list page and the reports share a directory.
            format!(
                "{}/public/hst_status/{}",
                s.hst_origin,
                href.trim_start_matches('/')
            )
        };
        let (body, at) = get(
            cache,
            "hst-latest.html",
            &report_url,
            online,
            s.min_refresh_s,
        )?;
        match parse_hst_report(&body, &report_url, at) {
            Ok(r) => return Ok(r),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn wait(stop: &Receiver<()>, seconds: f64) -> bool {
    matches!(
        stop.recv_timeout(Duration::from_secs_f64(seconds.max(1.0))),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

/// Run the schedule source for one vehicle until `stop` fires. Cached copies are shown first; with
/// `offline` set they are all there is.
pub fn run(
    cfg: &Config,
    (vehicle, kind): (&str, &str),
    cache: &HttpCache,
    (tx, stop): (&SyncSender<Event>, &Receiver<()>),
    fetch: &Fetch,
    offline: bool,
) {
    let key = SourceKey::Schedule(vehicle.to_owned());
    let health = |h| {
        tx.send(Event::Health {
            key: key.clone(),
            health: h,
        })
        .is_ok()
    };
    let s = &cfg.sources.schedules;
    let load = |online: bool| -> Result<Ops, ScheduleError> {
        match kind {
            "jwst" => fetch_jwst(cache, s, online).map(|j| Ops::Jwst(Arc::new(j))),
            "hst" => fetch_hst(cache, s, online).map(|r| Ops::Hst(Arc::new(r))),
            other => fmt_err(format!("unknown schedule kind {other:?}")),
        }
    };
    // Cached copy first, so the page has something before the network answers.
    if let Ok(ops) = load(false) {
        tracing::info!(vehicle, kind, "schedule loaded from cache");
        if tx
            .send(Event::Ops {
                vehicle: vehicle.to_owned(),
                ops,
                from_cache: true,
            })
            .is_err()
        {
            return;
        }
    }
    if offline {
        health(Health::Offline);
        while wait(stop, 3600.0) {}
        return;
    }
    let mut backoff = fetch.backoff_initial_s;
    loop {
        let next = match load(true) {
            Ok(ops) => {
                tracing::info!(vehicle, kind, "schedule refreshed");
                if tx
                    .send(Event::Ops {
                        vehicle: vehicle.to_owned(),
                        ops,
                        from_cache: false,
                    })
                    .is_err()
                    || !health(Health::Ok)
                {
                    return;
                }
                backoff = fetch.backoff_initial_s;
                s.min_refresh_s
            }
            Err(e) => {
                tracing::warn!(vehicle, kind, error = %e, backoff_s = backoff, "schedule refresh failed");
                if !health(Health::Failing(e.to_string())) {
                    return;
                }
                let w = backoff;
                backoff = (backoff * 2.0).min(fetch.backoff_max_s);
                w
            }
        };
        if !wait(stop, next) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEK: &str = include_str!("../fixtures/jwst-week.txt");
    const HST: &str = include_str!("../fixtures/hst-report.html");

    #[test]
    fn jwst_week_parses_prime_visits_and_attaches_parallels() {
        let v = parse_jwst_week(WEEK).unwrap();
        assert_eq!(v[0].id, "7782:16:1");
        assert_eq!(v[0].instrument, "NIRSpec MultiObject Spectroscopy");
        assert_eq!(v[0].target, "o16");
        assert_eq!(v[0].duration_s, 4.0 * 3600.0 + 11.0 * 60.0 + 14.0);
        assert_eq!(v[0].start.iso(), "2026-10-19T00:57:55.000");
        // The continuation line (COORDINATED PARALLEL / NIRCam Imaging) belongs to the visit above.
        assert_eq!(v[0].parallels, vec!["NIRCam Imaging".to_owned()]);
        // `^ATTACHED TO PRIME^` rows are not visits of their own.
        let mir = v.iter().find(|x| x.id == "11318:3:1").unwrap();
        assert_eq!(mir.parallels, vec!["NIRSpec Dark".to_owned()]);
        assert_eq!(mir.category, "Star");
        assert!(v.iter().all(|x| x.start.0 > 0.0));
        assert!(v.windows(2).all(|w| w[0].start.0 <= w[1].start.0));
        assert_eq!(mir.instrument_family(), "MIRI");
    }

    #[test]
    fn jwst_maintenance_rows_have_no_instrument() {
        let text = "VISIT ID       PCS MODE    VISIT TYPE                     SCHEDULED START TIME  DURATION     SCIENCE INSTRUMENT AND MODE                         TARGET NAME                      CATEGORY\n\
-------------  ----------  -----------------------------  --------------------  -----------  --------------------------------------------------  -------------------------------  ----------------------\n\
1000:1:43      NONE        FULL BIASED MOMENTUM UNLOAD    2026-10-21T07:22:32Z  00/01:12:10\n";
        let v = parse_jwst_week(text).unwrap();
        assert_eq!(v.len(), 1);
        assert!(v[0].is_maintenance());
        assert_eq!(v[0].kind, "FULL BIASED MOMENTUM UNLOAD");
    }

    #[test]
    fn jwst_layout_changes_are_errors() {
        let renamed = WEEK.replace("TARGET NAME", "TGT NAME   ");
        assert!(
            matches!(parse_jwst_week(&renamed), Err(ScheduleError::Format(m)) if m.contains("TARGET NAME"))
        );
        assert!(parse_jwst_week("nothing here").is_err());
        let bad_time = WEEK.replace("2026-10-19T00:57:55Z", "soon                ");
        assert!(parse_jwst_week(&bad_time).is_err());
    }

    #[test]
    fn jwst_index_links_are_found_newest_week_first() {
        let html = r#"<a href="/files/live/sites/www/files/home/jwst/science-execution/observing-schedules/_documents/20261012_report_20261008.txt">a</a>
<a href="/files/live/sites/www/files/home/jwst/science-execution/observing-schedules/_documents/20261026_report_20261022.txt">b</a>
<a href="/other/20270101_report_x.txt">no</a>
<a href="/files/live/sites/www/files/home/jwst/science-execution/observing-schedules/_documents/20261019_report_20261022.txt">c</a>"#;
        let f = jwst_files(html);
        assert_eq!(f.len(), 3);
        assert!(
            f[0].contains("20261026") && f[1].contains("20261019") && f[2].contains("20261012")
        );
    }

    #[test]
    fn newer_files_win_when_weeks_overlap() {
        let mk = |target: &str| Visit {
            id: "1:1:1".into(),
            start: Utc(100.0),
            duration_s: 10.0,
            kind: "PRIME".into(),
            pcs_mode: String::new(),
            instrument: "NIRCam Imaging".into(),
            target: target.into(),
            category: String::new(),
            parallels: vec![],
        };
        let m = merge_weeks(vec![vec![mk("new")], vec![mk("old")]]);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].target, "new");
    }

    #[test]
    fn hst_report_parses_sections_and_programs() {
        let r = parse_hst_report(HST, "u", Utc(5.0)).unwrap();
        assert_eq!(r.number, 11057);
        assert!(r.period.contains("October 21, 2026"), "{}", r.period);
        assert_eq!(r.science.len(), 2);
        assert_eq!(r.science[0].id, "17470");
        assert!(r.science[0].investigator.starts_with("Kurt Retherford"));
        assert!(r.science[0].title.starts_with("HST-Juno Io"));
        assert_eq!(r.calibration.len(), 2);
        assert_eq!(r.other.len(), 1);
    }

    #[test]
    fn hst_report_without_programs_or_number_is_an_error() {
        assert!(parse_hst_report("<html>nothing</html>", "u", Utc(0.0)).is_err());
        let no_rows = HST.split("<table border=\"1\"").next().unwrap();
        assert!(
            matches!(parse_hst_report(no_rows, "u", Utc(0.0)), Err(ScheduleError::Format(m)) if m.contains("no programs"))
        );
    }

    #[test]
    fn hst_latest_report_is_the_highest_day_and_number() {
        let html = r#"<a href="hst_status_2026293_11055.html">x</a><a href="hst_status_2026295_11057.html">y</a><a href="hst_status_2026294_11056.html">z</a><a href="x.html">n</a>"#;
        assert_eq!(
            hst_latest_report(html).as_deref(),
            Some("hst_status_2026295_11057.html")
        );
        assert_eq!(hst_latest_report("<a href=\"x\">"), None);
    }
}
