//! The parsers face downloaded data, so none of them may panic whatever they are given: they return an error.
//! Each property feeds a parser a real fixture with a few random edits (a character replaced, deleted or
//! inserted, the text cut short, a line dropped or doubled) and arbitrary text.

use mctl_astro::horizons;
use mctl_astro::oem::Oem;
use mctl_astro::sgp4model::GpModel;
use mctl_astro::supgp::SupGp;
use mctl_core::time::Utc;
use proptest::prelude::*;

const OMM: &str = include_str!("../fixtures/iss-omm.json");
const OEM: &str = include_str!("../fixtures/iss-oem-sample.txt");
const HORIZONS: &str = include_str!("../fixtures/horizons-jwst-sample.txt");

type Edit = (usize, u8, char);

/// `seed` with the given edits applied; positions wrap, so any edit list is valid.
fn mutate(seed: &str, edits: &[Edit]) -> String {
    let mut c: Vec<char> = seed.chars().collect();
    for &(pos, op, ch) in edits {
        if c.is_empty() {
            break;
        }
        let i = pos % c.len();
        match op % 6 {
            0 => c[i] = ch,
            1 => {
                c.remove(i);
            }
            2 => c.insert(i, ch),
            3 => c.truncate(i),
            _ => {
                let start = c[..i].iter().rposition(|&x| x == '\n').map_or(0, |p| p + 1);
                let end = c[i..]
                    .iter()
                    .position(|&x| x == '\n')
                    .map_or(c.len(), |p| i + p + 1);
                if op % 6 == 4 {
                    c.drain(start..end);
                } else {
                    let line: Vec<char> = c[start..end].to_vec();
                    c.splice(end..end, line);
                }
            }
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
    fn omm_json_never_panics(e in edits(), junk in any::<String>()) {
        let _ = GpModel::from_omm_json(&mutate(OMM, &e));
        let _ = GpModel::from_omm_json(&junk);
        let _ = GpModel::all_from_omm_json(&mutate(OMM, &e));
    }

    #[test]
    fn supplemental_gp_never_panics(e in edits(), junk in any::<String>()) {
        let _ = SupGp::parse(&mutate(OMM, &e));
        let _ = SupGp::parse(&junk);
    }

    #[test]
    fn oem_never_panics(e in edits(), junk in any::<String>()) {
        let _ = Oem::parse(&mutate(OEM, &e));
        let _ = Oem::parse(&junk);
    }

    #[test]
    fn horizons_never_panics(e in edits(), junk in any::<String>()) {
        let t = Utc(1.0e9);
        let _ = horizons::parse(&mutate(HORIZONS, &e), t);
        let _ = horizons::parse(&junk, t);
    }
}
