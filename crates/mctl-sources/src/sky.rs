//! "What is it looking at": resolve a target name to a sky position and fetch a survey cutout of the
//! field. The picture is a DSS2 sky-survey image around the target, never an image taken by Hubble or
//! JWST, and the display says so.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

use mctl_core::config::SkySources;
use mctl_core::schedule::{FieldRequest, FieldState};

use crate::tasks::Event;

/// A service that does not even accept the connection in this long is down, however patient the request is.
const CONNECT_TIMEOUT_S: f64 = 15.0;
/// A host that could not be reached is not asked again for this long, so a service that is down costs one
/// connect timeout, not one per picture.
const HOST_BACKOFF_S: f64 = 600.0;
/// Largest response read for a picture or a program file, bytes.
const MAX_BODY: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub name: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub origin: String,
}

/// Percent-encode a query value.
fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `2MASS-J18022716+6043356` encodes its own position: hhmmss.ss and +ddmmss.s.
pub fn parse_2mass(name: &str) -> Option<Position> {
    let body = name
        .strip_prefix("2MASS-J")
        .or_else(|| name.strip_prefix("2MASS J"))?;
    let split = body.find(['+', '-'])?;
    let (ra, dec) = body.split_at(split);
    let sign = if dec.starts_with('-') { -1.0 } else { 1.0 };
    let dec = &dec[1..];
    if ra.len() != 8
        || dec.len() != 7
        || !ra.bytes().all(|b| b.is_ascii_digit())
        || !dec.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let n = |s: &str| s.parse::<f64>().ok();
    let ra_h = n(&ra[0..2])? + n(&ra[2..4])? / 60.0 + n(&ra[4..8])? / 100.0 / 3600.0;
    let dec_d = n(&dec[0..2])? + n(&dec[2..4])? / 60.0 + n(&dec[4..7])? / 10.0 / 3600.0;
    Some(Position {
        name: name.to_owned(),
        ra_deg: ra_h * 15.0,
        dec_deg: sign * dec_d,
        origin: "2MASS designation".into(),
    })
}

/// The `%J` line of a Sesame reply: `%J 10.68470833 +41.26875000 = 00 42 44.330 +41 16 07.50`.
pub fn parse_sesame(text: &str, name: &str) -> Option<Position> {
    let line = text.lines().find(|l| l.starts_with("%J "))?;
    let mut it = line[3..].split_whitespace();
    let ra: f64 = it.next()?.parse().ok()?;
    let dec: f64 = it.next()?.parse().ok()?;
    ((0.0..360.0).contains(&ra) && (-90.0..=90.0).contains(&dec)).then(|| Position {
        name: name.to_owned(),
        ra_deg: ra,
        dec_deg: dec,
        origin: "Sesame (SIMBAD/NED/VizieR)".into(),
    })
}

/// Fixed targets of a Hubble Phase II program file: lines such as
/// `1   HD-35165   STAR, B3-B5 V-IV, BE   RA=05H21M16.8603S,   J2000 ...` followed by `DEC=-34D20'42.17"`.
pub fn parse_hst_targets(text: &str) -> Vec<Position> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(ra_at) = line.find("RA=") else {
            continue;
        };
        let Some(dec_line) = lines.get(i + 1).filter(|l| l.contains("DEC=")) else {
            continue;
        };
        let mut head = line[..ra_at].split_whitespace();
        let (Some(num), Some(name)) = (head.next(), head.next()) else {
            continue;
        };
        if num.parse::<u32>().is_err() {
            continue;
        }
        let ra = parse_hms(&line[ra_at + 3..]);
        let dec = dec_line
            .find("DEC=")
            .and_then(|d| parse_dms(&dec_line[d + 4..]));
        if let (Some(ra), Some(dec)) = (ra, dec) {
            out.push(Position {
                name: name.trim_start_matches('-').to_owned(),
                ra_deg: ra,
                dec_deg: dec,
                origin: "program Phase II file".into(),
            });
        }
    }
    out
}

/// `05H21M16.8603S` to degrees.
fn parse_hms(s: &str) -> Option<f64> {
    let (h, rest) = s.split_once('H')?;
    let (m, rest) = rest.split_once('M')?;
    let sec = rest.split('S').next()?;
    Some(
        (h.trim().parse::<f64>().ok()?
            + m.parse::<f64>().ok()? / 60.0
            + sec.parse::<f64>().ok()? / 3600.0)
            * 15.0,
    )
}

/// `-34D20'42.17"` to degrees.
fn parse_dms(s: &str) -> Option<f64> {
    let (d, rest) = s.split_once('D')?;
    let (m, rest) = rest.split_once('\'')?;
    let sec = rest.split('"').next()?;
    let d = d.trim();
    let sign = if d.starts_with('-') { -1.0 } else { 1.0 };
    let deg = d.trim_start_matches(['+', '-']).parse::<f64>().ok()?;
    Some(sign * (deg + m.parse::<f64>().ok()? / 60.0 + sec.parse::<f64>().ok()? / 3600.0))
}

/// hips2fits request for a TAN cutout centred on the position.
pub fn cutout_url(cfg: &SkySources, ra_deg: f64, dec_deg: f64) -> String {
    format!(
        "{}?hips={}&width={px}&height={px}&fov={}&projection=TAN&coordsys=icrs&ra={ra_deg:.6}&dec={dec_deg:.6}&format=jpg",
        cfg.hips2fits_url,
        encode(&cfg.survey),
        cfg.fov_deg,
        px = cfg.size_px,
    )
}

/// STScI Digitized Sky Survey request for a GIF cutout `fov_deg` across, centred on the position.
pub fn dss_url(cfg: &SkySources, ra_deg: f64, dec_deg: f64) -> String {
    let arcmin = cfg.fov_deg * 60.0;
    format!(
        "{}?v={}&r={ra_deg:.6}&d={dec_deg:.6}&e=J2000&h={arcmin:.1}&w={arcmin:.1}&f=gif&c=none&fov=NONE&v3=",
        cfg.dss_url, cfg.dss_survey,
    )
}

/// Names a visit list uses for things that are not sky positions.
fn is_internal_name(name: &str) -> bool {
    let n = name.trim();
    n.is_empty()
        || n.to_ascii_lowercase().contains("apt")
        || n.starts_with("input_")
        || n.contains("-Background")
        || n.contains("-bkg")
}

struct Http {
    agent: ureq::Agent,
    user_agent: String,
    cache: PathBuf,
    max_age_s: f64,
    /// Nothing is fetched: only what is cached, however old, is served.
    offline: bool,
    /// When each host that could not be reached last failed.
    down: RefCell<HashMap<String, Instant>>,
}

/// `host` of an `http(s)` URL.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

impl Http {
    fn cached(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.cache.join(key);
        let age = std::fs::metadata(&path)
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .ok()?;
        (self.offline || age.as_secs_f64() < self.max_age_s)
            .then(|| std::fs::read(&path).ok())
            .flatten()
    }

    fn get(&self, key: &str, url: &str) -> Result<Vec<u8>, String> {
        if let Some(b) = self.cached(key) {
            return Ok(b);
        }
        if self.offline {
            return Err(format!("offline and {key} is not cached"));
        }
        let host = host_of(url).to_owned();
        if let Some(at) = self.down.borrow().get(&host)
            && at.elapsed().as_secs_f64() < HOST_BACKOFF_S
        {
            return Err(format!("{host} could not be reached a moment ago"));
        }
        let call = || {
            self.agent
                .get(url)
                .header("User-Agent", &self.user_agent)
                .call()
        };
        // A service that is merely slow is given a second go; one that refuses is not asked twice.
        let reply = match call() {
            Err(ureq::Error::Timeout(why)) => {
                tracing::warn!(url, %why, "request timed out, trying once more");
                call()
            }
            other => other,
        };
        let mut resp = reply.map_err(|e| {
            self.down.borrow_mut().insert(host, Instant::now());
            format!("{url}: {e}")
        })?;
        if resp.status().as_u16() != 200 {
            return Err(format!("{url}: HTTP {}", resp.status().as_u16()));
        }
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))?;
        if body.is_empty() {
            return Err(format!("{url}: empty reply"));
        }
        std::fs::create_dir_all(&self.cache)
            .map_err(|e| format!("{}: {e}", self.cache.display()))?;
        std::fs::write(self.cache.join(key), &body)
            .map_err(|e| format!("{}: {e}", self.cache.display()))?;
        Ok(body)
    }

    fn text(&self, key: &str, url: &str) -> Result<String, String> {
        String::from_utf8(self.get(key, url)?).map_err(|e| format!("{url}: not text: {e}"))
    }
}

fn safe_key(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Resolve a JWST target name to a position: its own designation if it has one, else Sesame, trying the
/// name as written and with hyphens read as spaces.
fn resolve_name(http: &Http, cfg: &SkySources, name: &str) -> Result<Position, String> {
    if let Some(p) = parse_2mass(name) {
        return Ok(p);
    }
    if is_internal_name(name) {
        return Err(format!(
            "{name} is a proposal-internal name, not a catalogued object"
        ));
    }
    let sesame = resolve_sesame(http, cfg, name);
    let Err(first) = sesame else {
        return sesame;
    };
    // Sesame is down or does not know it: ask MAST, which also searches SIMBAD and NED.
    resolve_mast(http, cfg, name).map_err(|e| format!("{first}; then {e}"))
}

fn resolve_sesame(http: &Http, cfg: &SkySources, name: &str) -> Result<Position, String> {
    let mut tried = Vec::new();
    for variant in [name.to_owned(), name.replace('-', " ")] {
        if tried.contains(&variant) {
            continue;
        }
        let url = cfg.sesame_url.replace("{name}", &encode(&variant));
        let text = http.text(&format!("sesame-{}.txt", safe_key(&variant)), &url)?;
        if let Some(p) = parse_sesame(&text, name) {
            return Ok(p);
        }
        tried.push(variant);
    }
    Err(format!("Sesame does not know {name:?}"))
}

/// The reply of MAST's `Mast.Name.Lookup`: `{"resolvedCoordinate":[{"ra":182.6,"decl":39.4,...}],...}`.
pub fn parse_mast(text: &str, name: &str) -> Option<Position> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let r = v.get("resolvedCoordinate")?.get(0)?;
    let (ra, dec) = (r.get("ra")?.as_f64()?, r.get("decl")?.as_f64()?);
    ((0.0..360.0).contains(&ra) && (-90.0..=90.0).contains(&dec)).then(|| Position {
        name: name.to_owned(),
        ra_deg: ra,
        dec_deg: dec,
        origin: "MAST name resolver (SIMBAD/NED)".into(),
    })
}

fn resolve_mast(http: &Http, cfg: &SkySources, name: &str) -> Result<Position, String> {
    let mut tried = Vec::new();
    for variant in [name.to_owned(), name.replace('-', " ")] {
        if tried.contains(&variant) {
            continue;
        }
        let request = serde_json::json!({
            "service": "Mast.Name.Lookup",
            "params": { "input": variant, "format": "json" },
        });
        let url = format!(
            "{}?request={}",
            cfg.mast_name_url,
            encode(&request.to_string())
        );
        let text = http.text(&format!("mast-{}.json", safe_key(&variant)), &url)?;
        if let Some(p) = parse_mast(&text, name) {
            return Ok(p);
        }
        tried.push(variant);
    }
    Err(format!("MAST does not know {name:?}"))
}

fn resolve(
    http: &Http,
    cfg: &SkySources,
    req: &FieldRequest,
) -> Result<(Position, Vec<u8>, String), String> {
    let pos = match req {
        FieldRequest::Target(name) => resolve_name(http, cfg, name)?,
        FieldRequest::HstProgram(id) => {
            let url = cfg.hst_program_url.replace("{id}", id);
            let text = http.text(&format!("hstpro-{}.txt", safe_key(id)), &url)?;
            parse_hst_targets(&text).into_iter().next().ok_or_else(|| {
                "the program's Phase II file lists no fixed targets (solar-system or moving targets have no sky position)"
                    .to_owned()
            })?
        }
    };
    let (image, survey) = match cutout(http, cfg, &pos) {
        Ok(c) => c,
        Err(first) => dss_cutout(http, cfg, &pos).map_err(|e| format!("{first}; then {e}"))?,
    };
    Ok((pos, image, survey))
}

/// The hips2fits cutout, a JPEG.
fn cutout(http: &Http, cfg: &SkySources, pos: &Position) -> Result<(Vec<u8>, String), String> {
    let url = cutout_url(cfg, pos.ra_deg, pos.dec_deg);
    let key = format!(
        "cutout-{:.4}-{:+.4}-{}.jpg",
        pos.ra_deg,
        pos.dec_deg,
        safe_key(&cfg.survey)
    );
    let jpeg = http.get(&key, &url)?;
    // A reply that is not a JPEG is an error page, not a picture.
    if jpeg.len() < 4 || jpeg[..2] != [0xFF, 0xD8] {
        return Err(format!("{url}: the reply is not a JPEG"));
    }
    Ok((jpeg, cfg.survey.clone()))
}

/// The STScI DSS cutout, a GIF.
fn dss_cutout(http: &Http, cfg: &SkySources, pos: &Position) -> Result<(Vec<u8>, String), String> {
    let url = dss_url(cfg, pos.ra_deg, pos.dec_deg);
    let key = format!(
        "dss-{:.4}-{:+.4}-{}.gif",
        pos.ra_deg,
        pos.dec_deg,
        safe_key(&cfg.dss_survey)
    );
    let gif = http.get(&key, &url)?;
    if !gif.starts_with(b"GIF8") {
        return Err(format!("{url}: the reply is not a GIF"));
    }
    Ok((gif, format!("STScI DSS {}", cfg.dss_survey)))
}

/// Serve picture requests until the channel closes. Each reply is an `Event::Field`.
pub fn run(
    cfg: SkySources,
    user_agent: String,
    cache_dir: &Path,
    requests: &Receiver<(String, FieldRequest)>,
    tx: &SyncSender<Event>,
    offline: bool,
) {
    let http = Http {
        agent: ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs_f64(cfg.timeout_s)))
            .timeout_connect(Some(Duration::from_secs_f64(CONNECT_TIMEOUT_S)))
            .http_status_as_error(false)
            .build()
            .into(),
        user_agent,
        cache: cache_dir.join("sky"),
        max_age_s: cfg.cache_s,
        offline,
        down: RefCell::default(),
    };
    while let Ok((vehicle, req)) = requests.recv() {
        let state = if offline {
            // Cached pictures still work offline; the HTTP layer answers from the cache and never connects.
            match resolve(&http, &cfg, &req) {
                Ok(r) => ready(&cfg, r),
                Err(_) => FieldState::Unavailable("offline and not cached".into()),
            }
        } else {
            match resolve(&http, &cfg, &req) {
                Ok(r) => ready(&cfg, r),
                Err(e) => {
                    tracing::warn!(?req, error = %e, "field picture unavailable");
                    FieldState::Unavailable(e)
                }
            }
        };
        if tx
            .send(Event::Field {
                vehicle,
                key: req.key(),
                state,
            })
            .is_err()
        {
            return;
        }
    }
}

fn ready(cfg: &SkySources, (pos, jpeg, survey): (Position, Vec<u8>, String)) -> FieldState {
    FieldState::Ready {
        target: pos.name,
        ra_deg: pos.ra_deg,
        dec_deg: pos.dec_deg,
        fov_deg: cfg.fov_deg,
        origin: pos.origin,
        survey,
        jpeg: Arc::new(jpeg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRO: &str = "\
1   HD-35165     STAR, B3-B5 V-IV, BE    RA=05H21M16.8603S,               J2000             V = 6.09
                                         DEC=-34D20'42.17\"                                  erg/s
    Reference Frame: ICRS    Extended: NO
2   -ALF-ARA     STAR, B0-B2 V-IV, BE    RA=17H31M50.4915S,               J2000             V = 2.95
                                         DEC=+49D52'34.12\"
";

    fn cfg() -> SkySources {
        mctl_core::config::Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None)
            .unwrap()
            .sources
            .sky
    }

    #[test]
    fn two_mass_names_carry_their_position() {
        let p = parse_2mass("2MASS-J18022716+6043356").unwrap();
        assert!((p.ra_deg - 270.6132).abs() < 1e-3, "{}", p.ra_deg); // 18h02m27.16s
        assert!((p.dec_deg - 60.7266).abs() < 1e-3, "{}", p.dec_deg); // +60d43m35.6s
        let s = parse_2mass("2MASS-J03313052-2739514").unwrap();
        assert!(
            s.dec_deg < -27.0 && s.dec_deg > -28.0,
            "southern declination is negative"
        );
        assert!(parse_2mass("V339-Del").is_none());
        assert!(
            parse_2mass("2MASS-J1802+60").is_none(),
            "short designations are refused, not guessed"
        );
    }

    #[test]
    fn sesame_replies_are_read_from_the_j_line() {
        let text = "# M31\n%@ @1575544\n%J 10.68470833 +41.26875000 = 00 42 44.330  +41 16 07.50\n";
        let p = parse_sesame(text, "M31").unwrap();
        assert_eq!((p.ra_deg, p.dec_deg), (10.68470833, 41.26875));
        assert!(parse_sesame("# nothing\n#!Sc=Simbad: not found\n", "x").is_none());
        assert!(
            parse_sesame("%J 400 +10", "x").is_none(),
            "out-of-range right ascension"
        );
    }

    #[test]
    fn hst_program_files_yield_fixed_targets_only() {
        let t = parse_hst_targets(PRO);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "HD-35165");
        assert!((t[0].ra_deg - 80.3202).abs() < 1e-3, "{}", t[0].ra_deg); // 5h21m16.86s
        assert!((t[0].dec_deg - -34.34505).abs() < 1e-4, "{}", t[0].dec_deg);
        assert_eq!(
            t[1].name, "ALF-ARA",
            "the leading dash of a Greek-letter name is dropped"
        );
        assert!(t[1].dec_deg > 49.0);
        assert!(parse_hst_targets("TARGET NAME: IO\nSTD=JUPITER\n").is_empty());
    }

    #[test]
    fn cutout_urls_ask_for_a_tan_jpeg_around_the_target() {
        let u = cutout_url(&cfg(), 10.6847, 41.2687);
        assert!(u.starts_with("https://alasky.cds.unistra.fr/hips-image-services/hips2fits?hips=CDS%2FP%2FDSS2%2Fcolor"), "{u}");
        for part in [
            "width=512",
            "height=512",
            "fov=0.25",
            "projection=TAN",
            "coordsys=icrs",
            "ra=10.684700",
            "dec=41.268700",
            "format=jpg",
        ] {
            assert!(u.contains(part), "{part} in {u}");
        }
    }

    #[test]
    fn proposal_internal_names_are_not_sent_to_sesame() {
        for n in [
            "input_lulls_00",
            "GO9016_apt_v2",
            "V339-Del-Background",
            "Pa30-bkg",
            "",
        ] {
            assert!(is_internal_name(n), "{n}");
        }
        assert!(!is_internal_name("V339-Del") && !is_internal_name("HD-209458b"));
    }

    #[test]
    fn mast_replies_carry_the_position() {
        let text = r#"{"resolvedCoordinate":[{"canonicalName":"NGC  4151","ra":182.6357,"decl":39.4059}],"status":""}"#;
        let p = parse_mast(text, "NGC 4151").unwrap();
        assert_eq!((p.ra_deg, p.dec_deg), (182.6357, 39.4059));
        assert!(parse_mast(r#"{"resolvedCoordinate":[],"status":""}"#, "x").is_none());
        assert!(parse_mast("<html>", "x").is_none());
    }

    #[test]
    fn dss_urls_ask_for_a_gif_the_size_of_the_field() {
        let u = dss_url(&cfg(), 182.6357, 39.4059);
        assert!(u.starts_with("https://archive.stsci.edu/cgi-bin/dss_search?v=poss2ukstu_red&r=182.635700&d=39.405900"), "{u}");
        assert!(u.contains("&h=15.0&w=15.0&f=gif"), "{u}");
    }

    #[test]
    fn hosts_are_read_from_urls() {
        assert_eq!(
            host_of("https://cds.unistra.fr/cgi-bin/x?y=1"),
            "cds.unistra.fr"
        );
        assert_eq!(host_of("https://a.b?c"), "a.b");
    }

    fn http(dir: &Path, offline: bool, max_age_s: f64) -> Http {
        Http {
            agent: ureq::Agent::config_builder().build().into(),
            user_agent: "test".into(),
            cache: dir.to_owned(),
            max_age_s,
            offline,
            down: RefCell::default(),
        }
    }

    #[test]
    fn offline_serves_the_cache_at_any_age_and_never_connects() {
        let dir = std::env::temp_dir().join(format!("mctl-sky-offline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.txt"), b"cached").unwrap();
        // Nothing listens on port 1, so a request would fail with a connection error, not "offline".
        let url = "http://127.0.0.1:1/x";
        let online = http(&dir, false, 0.0);
        assert!(online.get("old.txt", url).is_err(), "too old to use online");
        let offline = http(&dir, true, 0.0);
        assert_eq!(offline.get("old.txt", url).unwrap(), b"cached");
        let err = offline.get("missing.txt", url).unwrap_err();
        assert!(err.starts_with("offline and missing.txt"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
