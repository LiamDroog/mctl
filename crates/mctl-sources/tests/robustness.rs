//! Everything here parses text that arrives from a server, so none of it may panic whatever it is handed: a
//! malformed response is an error that the source reports as unhealthy, not a crashed program.

use mctl_core::config::{Config, DEFAULT_CONFIG};
use mctl_core::time::Utc;
use mctl_sources::isslive::{config as isslive_config, tlcp};
use mctl_sources::observer::parse_ip_json;
use mctl_sources::schedules::{parse_hst_report, parse_jwst_week};
use mctl_sources::sky::{parse_2mass, parse_hst_targets, parse_sesame};
use proptest::prelude::*;

const JWST_WEEK: &str = include_str!("../fixtures/jwst-week.txt");
const HST_REPORT: &str = include_str!("../fixtures/hst-report.html");

type Edit = (usize, u8, char);

fn mutate(seed: &str, edits: &[Edit]) -> String {
    let mut c: Vec<char> = seed.chars().collect();
    for &(pos, op, ch) in edits {
        if c.is_empty() {
            break;
        }
        let i = pos % c.len();
        match op % 4 {
            0 => c[i] = ch,
            1 => {
                c.remove(i);
            }
            2 => c.insert(i, ch),
            _ => c.truncate(i),
        }
    }
    c.into_iter().collect()
}

fn edits() -> impl Strategy<Value = Vec<Edit>> {
    prop::collection::vec((any::<usize>(), any::<u8>(), any::<char>()), 0..10)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn lightstreamer_lines_never_panic(s in any::<String>()) {
        let _ = tlcp::parse_notification(&s);
        for head in ["U,1,1,", "CONOK,", "PROBE", "LOOP,", "END,", "SYNC,", "REQOK,", "ERROR,", "MSGDONE,"] {
            let _ = tlcp::parse_notification(&format!("{head}{s}"));
        }
    }

    #[test]
    fn the_isslive_item_list_never_panics(e in edits(), junk in any::<String>()) {
        let _ = isslive_config::Config::parse("x", &mutate(isslive_config::DEFAULT_CONFIG, &e));
        let _ = isslive_config::Config::parse("x", &junk);
    }

    #[test]
    fn schedule_pages_never_panic(e in edits(), junk in any::<String>()) {
        let t = Utc(1.0e9);
        let _ = parse_jwst_week(&mutate(JWST_WEEK, &e));
        let _ = parse_jwst_week(&junk);
        let _ = parse_hst_report(&mutate(HST_REPORT, &e), "https://example.test/r", t);
        let _ = parse_hst_report(&junk, "https://example.test/r", t);
    }

    #[test]
    fn name_and_position_lookups_never_panic(s in any::<String>(), name in any::<String>()) {
        let _ = parse_2mass(&s);
        let _ = parse_sesame(&s, &name);
        let _ = parse_hst_targets(&s);
    }

    #[test]
    fn the_ip_location_response_never_panics(s in any::<String>()) {
        let cfg = Config::from_layers(DEFAULT_CONFIG, None).expect("the embedded config is valid");
        let _ = parse_ip_json(&s, &cfg.observer.ip);
        let _ = parse_ip_json(&format!("{{\"latitude\": {s}, \"longitude\": {s}}}"), &cfg.observer.ip);
    }
}
