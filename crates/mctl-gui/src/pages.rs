//! The page map (DESIGN.md §9.4) and per-vehicle availability.
//!
//! A page a vehicle has no data for is greyed with the reason (`NO SOURCE`), never hidden, so the
//! layout stays stable across vehicles (DESIGN.md §2).

use mctl_core::config::Regime;
use mctl_core::world::VehicleView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Master,
    Traj,
    Gnc,
    Eps,
    Tcs,
    Comm,
    Pass,
    Prox,
    View,
    Sys,
    Ops,
}

pub const ALL_PAGES: [Page; 11] = [
    Page::Master,
    Page::Traj,
    Page::Gnc,
    Page::Eps,
    Page::Tcs,
    Page::Comm,
    Page::Pass,
    Page::Prox,
    Page::View,
    Page::Sys,
    Page::Ops,
];

impl Page {
    /// Command-line name.
    pub fn key(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Traj => "traj",
            Self::Gnc => "gnc",
            Self::Eps => "eps",
            Self::Tcs => "tcs",
            Self::Comm => "comm",
            Self::Pass => "pass",
            Self::Prox => "prox",
            Self::View => "view",
            Self::Sys => "sys",
            Self::Ops => "ops",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Master => "MASTER",
            Self::Traj => "TRAJ",
            Self::Gnc => "GNC",
            Self::Eps => "EPS",
            Self::Tcs => "TCS/ECLSS",
            Self::Comm => "COMM",
            Self::Pass => "PASS",
            Self::Prox => "PROX",
            Self::View => "VIEW",
            Self::Sys => "SYS",
            Self::Ops => "OPS",
        }
    }

    /// Function key number (F1..F10).
    pub fn fkey(self) -> u8 {
        ALL_PAGES.iter().position(|p| *p == self).expect("listed") as u8 + 1
    }

    pub fn from_fkey(n: u8) -> Option<Self> {
        n.checked_sub(1)
            .and_then(|i| ALL_PAGES.get(usize::from(i)).copied())
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let n = name.to_ascii_lowercase();
        ALL_PAGES.into_iter().find(|p| p.key() == n)
    }

    /// Sub-page titles; Tab cycles through them.
    pub fn subpages(self) -> &'static [&'static str] {
        match self {
            Self::Master => &["MASTER"],
            Self::Traj => &[
                "ORBIT MAP 3D",
                "GROUND TRACK",
                "ELEMENTS AND TRENDS",
                "MANEUVER PLANNER",
            ],
            Self::Gnc => &["ADI", "RATES AND ERRORS", "CMG MOMENTUM"],
            Self::Eps => &["ARRAYS/SARJ", "BATTERIES AND BUSES"],
            Self::Tcs => &["LOOPS AND RADIATORS", "ATMOSPHERE"],
            Self::Comm => &["LINKS AND AOS/LOS", "TDRS COVERAGE"],
            Self::Pass => &["SKY PLOT", "PASS LIST", "SITES"],
            Self::Prox => &["R-BAR/V-BAR"],
            Self::View => &["WINDOW VIEW"],
            Self::Sys => &["EVENT LOG", "SOURCE HEALTH", "SETTINGS", "C&W RULES"],
            Self::Ops => &["OBSERVING SCHEDULE"],
        }
    }

    /// Resolve a sub-page given as a 1-based number or a (prefix of a) word of its title.
    pub fn resolve_sub(self, arg: &str) -> Option<usize> {
        if let Ok(n) = arg.parse::<usize>() {
            return (1..=self.subpages().len()).contains(&n).then(|| n - 1);
        }
        let a = arg.to_ascii_lowercase();
        let hits: Vec<usize> = self
            .subpages()
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.to_ascii_lowercase()
                    .split([' ', '/'])
                    .any(|w| w.starts_with(&a))
            })
            .map(|(i, _)| i)
            .collect();
        (hits.len() == 1).then(|| hits[0])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Greyed, with the reason shown to the operator.
    NoSource(String),
}

/// Whether `page`/`sub` has a data source for `vehicle`.
pub fn availability(page: Page, sub: usize, vehicle: Option<&VehicleView>) -> Availability {
    use Availability::{Available, NoSource};
    let telemetry = |vehicle: Option<&VehicleView>| match vehicle {
        None => NoSource("no vehicle data yet".into()),
        Some(v) => match v.telemetry.as_deref() {
            None => NoSource(format!("{} has no telemetry source", v.name)),
            Some("isslive") => NoSource("no source for this page".into()),
            Some(other) => NoSource(format!("telemetry source {other} is not implemented")),
        },
    };
    // ISSLIVE vehicles have a feed; a page is available when it has item IDs configured.
    let isslive = |vehicle: Option<&VehicleView>, available: bool, why: &str| match vehicle
        .and_then(|v| v.telemetry.as_deref())
    {
        Some("isslive") if available => Available,
        Some("isslive") => NoSource(why.into()),
        _ => telemetry(vehicle),
    };
    let l2 = vehicle.is_some_and(|v| v.regime == Regime::L2);
    match page {
        Page::Master | Page::Sys => Available,
        Page::Traj if sub == 3 && l2 => NoSource(
            "the maneuver planner models Earth-orbit vehicles; an L2 halo orbit is not planned here".into(),
        ),
        Page::Traj if l2 && sub == 1 => NoSource(
            "a ground track means nothing 1.5 million km from Earth; see the halo plots".into(),
        ),
        Page::Traj => Available,
        Page::Pass if l2 => NoSource(
            "an L2 vehicle is above the horizon for about half of every day; passes are not predicted"
                .into(),
        ),
        Page::Ops => match vehicle {
            None => NoSource("no vehicle data yet".into()),
            Some(v) if v.schedule.is_some() => Available,
            Some(v) => NoSource(format!(
                "{} has no public observing schedule (none found)",
                v.name
            )),
        },
        Page::Gnc | Page::Tcs => isslive(vehicle, true, ""),
        Page::Eps => isslive(
            vehicle,
            sub == 0,
            "the public ISSLIVE item list has no battery or bus items",
        ),
        Page::Comm => isslive(vehicle, sub == 0, "TDRS coverage is a v2 feature"),
        Page::Pass => match vehicle {
            Some(_) => Available,
            None => NoSource("no vehicle data yet".into()),
        },
        Page::Prox => NoSource("no visiting vehicle is defined; prox-ops arrive in M6".into()),
        Page::View => NoSource("window view arrives in M7".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vehicle(telemetry: Option<&str>) -> VehicleView {
        VehicleView {
            id: "x".into(),
            name: "X".into(),
            norad: 1,
            telemetry: telemetry.map(str::to_owned),
            regime: Default::default(),
            schedule: None,
            deep: None,
            reference_burns: vec![],
            fields: vec![],
            ops: None,
            ops_health: None,
            met_epoch: None,
            sources: vec![],
            solution: None,
            track: None,
            passes: vec![],
            beta_series: Default::default(),
        }
    }

    #[test]
    fn function_keys_map_one_to_one_onto_pages() {
        for (i, p) in ALL_PAGES.iter().enumerate() {
            assert_eq!(p.fkey() as usize, i + 1);
            assert_eq!(Page::from_fkey(p.fkey()), Some(*p));
            assert_eq!(Page::from_name(p.key()), Some(*p));
            assert!(!p.subpages().is_empty());
        }
        assert_eq!(Page::from_fkey(0), None);
        assert_eq!(Page::from_fkey(12), None);
        assert_eq!(
            Page::from_name("TRAJ"),
            Some(Page::Traj),
            "names are case-insensitive"
        );
        assert_eq!(Page::from_name("nope"), None);
    }

    #[test]
    fn subpages_resolve_by_number_or_unambiguous_word() {
        assert_eq!(Page::Traj.resolve_sub("2"), Some(1));
        assert_eq!(Page::Traj.resolve_sub("ground"), Some(1));
        assert_eq!(Page::Traj.resolve_sub("gro"), Some(1));
        assert_eq!(Page::Traj.resolve_sub("planner"), Some(3));
        assert_eq!(Page::Traj.resolve_sub("0"), None);
        assert_eq!(Page::Traj.resolve_sub("5"), None);
        assert_eq!(Page::Traj.resolve_sub("zzz"), None);
        assert_eq!(Page::Sys.resolve_sub("source"), Some(1));
        assert_eq!(
            Page::Traj.resolve_sub("m"),
            None,
            "m matches MAP and MANEUVER"
        );
    }

    #[test]
    fn availability_follows_the_vehicle_capability_matrix() {
        let iss = vehicle(Some("isslive"));
        let hst = vehicle(None);
        assert_eq!(
            availability(Page::Master, 0, Some(&hst)),
            Availability::Available
        );
        assert_eq!(
            availability(Page::Traj, 0, Some(&hst)),
            Availability::Available
        );
        assert!(availability(Page::Traj, 3, Some(&iss)) == Availability::Available);
        assert!(
            matches!(availability(Page::Eps, 1, Some(&iss)), Availability::NoSource(r) if r.contains("no battery"))
        );
        assert!(
            matches!(availability(Page::Gnc, 0, Some(&hst)), Availability::NoSource(r) if r.contains("no telemetry source"))
        );
        assert!(availability(Page::Pass, 0, Some(&iss)) == Availability::Available);
        assert!(matches!(
            availability(Page::Gnc, 0, None),
            Availability::NoSource(_)
        ));
        assert_eq!(availability(Page::Sys, 2, None), Availability::Available);
    }

    #[test]
    fn l2_vehicles_lose_the_ground_track_and_passes_and_ops_needs_a_schedule() {
        let mut jwst = vehicle(None);
        jwst.regime = Regime::L2;
        jwst.schedule = Some("jwst".into());
        assert!(
            matches!(availability(Page::Traj, 1, Some(&jwst)), Availability::NoSource(r) if r.contains("ground track"))
        );
        assert_eq!(
            availability(Page::Traj, 0, Some(&jwst)),
            Availability::Available
        );
        assert!(matches!(
            availability(Page::Pass, 0, Some(&jwst)),
            Availability::NoSource(_)
        ));
        assert_eq!(
            availability(Page::Ops, 0, Some(&jwst)),
            Availability::Available
        );
        let iss = vehicle(Some("isslive"));
        assert!(
            matches!(availability(Page::Ops, 0, Some(&iss)), Availability::NoSource(r) if r.contains("no public observing schedule"))
        );
    }
}
