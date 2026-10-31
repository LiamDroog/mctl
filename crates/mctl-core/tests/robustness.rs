//! Configuration, plans, earth-orientation data and user-typed times come from files and keyboards, so none
//! of their parsers may panic whatever they are given: they return an error.

use mctl_core::command::{parse_offset, parse_seek};
use mctl_core::config::{Config, DEFAULT_CONFIG};
use mctl_core::plan::{PlanDef, Trigger};
use mctl_core::time::{EopTable, Utc};
use proptest::prelude::*;

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
    fn times_and_offsets_the_user_types_never_panic(s in any::<String>()) {
        let _ = Utc::parse(&s);
        let _ = parse_offset(&s);
        let _ = parse_seek(&s);
        let _ = Trigger::parse(&s);
    }

    #[test]
    fn a_damaged_user_config_is_an_error_not_a_panic(e in edits(), junk in any::<String>()) {
        let _ = Config::from_layers(DEFAULT_CONFIG, Some(("user.toml", &mutate(DEFAULT_CONFIG, &e))));
        let _ = Config::from_layers(DEFAULT_CONFIG, Some(("user.toml", &junk)));
    }

    #[test]
    fn a_damaged_plan_file_is_an_error_not_a_panic(junk in any::<String>()) {
        let _ = PlanDef::from_toml(&junk);
        let _ = PlanDef::from_toml(&format!("[[burns]]\ntrigger = \"{junk}\"\nframe = \"{junk}\"\ndv_m_s = [1.0, 0.0, 0.0]\n"));
    }

    #[test]
    fn earth_orientation_data_never_panics(junk in any::<String>(), e in edits()) {
        let row = "26 1 1 61041.00 I  0.100000 0.000050  0.200000 0.000050  I-0.0123456 0.0000200  0.3000 0.0200  P";
        let _ = EopTable::parse(&junk);
        let _ = EopTable::parse(&mutate(row, &e));
    }
}
