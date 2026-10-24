//! The immutable snapshot the engine publishes and the console reads (DESIGN.md §6.2).
//!
//! Nothing here is ever a placeholder: a quantity with no source is `None`, and the UI shows `NO DATA`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::state::{Elements, Geodetic, Quality, SourceKind, State};
use crate::time::Utc;

/// Fetch health of one source, as reported by its supervisor task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Health {
    /// Nothing loaded yet and no failure recorded.
    Starting,
    /// Data loaded; the last fetch attempt succeeded or was not yet due.
    Ok,
    /// Data may be loaded (from cache) but the last fetch attempt failed.
    Failing(String),
    /// Network disabled by `--offline`; running from cache only.
    Offline,
}

/// What the arbiter decided about a source at this instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SourceStatus {
    Active,
    Standby,
    /// Data older than the configured limit.
    Stale,
    /// Requested time lies outside the source's validity span.
    OutOfSpan,
    NoData,
    /// Data present but propagation failed (decayed object, bad elements, ...).
    Failed(String),
}

/// Difference between a source and the active one, in the active solution's RTN frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Residual {
    pub vs: SourceKind,
    pub rtn_km: [f64; 3],
    pub norm_km: f64,
    /// |dv| in mm/s.
    pub dv_mm_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceView {
    pub kind: SourceKind,
    pub health: Health,
    pub status: SourceStatus,
    /// GP element-set epoch, or OEM creation date.
    pub data_epoch: Option<Utc>,
    /// When this data was last obtained (cache file time or network fetch).
    pub fetched_at: Option<Utc>,
    pub from_cache: bool,
    pub age_s: Option<f64>,
    pub validity: Option<(Utc, Utc)>,
    pub residual: Option<Residual>,
    /// This source's own GCRF position (km) and velocity (km/s) now, whether or not it is active.
    pub state_gcrf: Option<[f64; 6]>,
}

/// One revolution-scale track sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackPoint {
    pub t: Utc,
    pub r_gcrf_km: [f64; 3],
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_km: f64,
    /// 1 = full sunlight, 0 = umbra.
    pub illumination: f32,
    /// Position in the Sun-Earth rotating frame (x away from the Sun, z ecliptic north), km.
    pub rot_km: [f64; 3],
}

/// L2 vehicle geometry at the snapshot time (see `mctl_astro::l2`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeepView {
    pub dist_km: f64,
    pub speed_km_s: f64,
    pub range_rate_km_s: f64,
    pub light_time_s: f64,
    pub off_axis_deg: f64,
    pub rot_km: [f64; 3],
}

/// Orbit events (DESIGN.md FR-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    AscendingNode,
    DescendingNode,
    Periapsis,
    Apoapsis,
    EclipseEntry,
    EclipseExit,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::AscendingNode => "ASCENDING NODE",
            Self::DescendingNode => "DESCENDING NODE",
            Self::Periapsis => "PERIAPSIS",
            Self::Apoapsis => "APOAPSIS",
            Self::EclipseEntry => "ECLIPSE ENTRY",
            Self::EclipseExit => "ECLIPSE EXIT",
        }
    }
}

/// Past and predicted track around the snapshot time. Rebuilt about once a second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Increments each rebuild so consumers can cheaply detect changes.
    pub revision: u64,
    pub source: SourceKind,
    pub points: Vec<TrackPoint>,
    /// Index of the first point at or after the snapshot time (past points precede it).
    pub now_index: usize,
    /// Upcoming events from root finding, sorted by time.
    pub events: Vec<(Utc, EventKind)>,
}

/// The arbiter-selected state and everything derived from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Solution {
    pub source: SourceKind,
    pub quality: Quality,
    pub state_gcrf: State,
    pub state_itrf: State,
    pub geodetic: Geodetic,
    pub elements: Elements,
    pub sun_gcrf_km: [f64; 3],
    pub beta_deg: f64,
    /// 1 = sunlit, 0 = umbra, between = penumbra.
    pub illumination: f64,
    pub gmst_rad: f64,
    /// Rotation taking GCRF vectors to ITRF, row-major.
    pub ecef_from_gcrf: [[f64; 3]; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct VehicleView {
    pub id: String,
    pub name: String,
    pub norad: u32,
    /// Telemetry source this vehicle has, if any (pages that need one are greyed without it).
    pub telemetry: Option<String>,
    pub regime: crate::config::Regime,
    /// Observing-schedule kind this vehicle has (`jwst`, `hst`), if any.
    pub schedule: Option<String>,
    /// L2 vehicles only, once a state exists.
    pub deep: Option<DeepView>,
    /// Latest observing schedule or daily report, when the vehicle has one and it has arrived.
    pub ops: Option<crate::schedule::Ops>,
    /// Fetch health of the schedule source; `None` for a vehicle without one.
    pub ops_health: Option<Health>,
    pub met_epoch: Option<Utc>,
    pub sources: Vec<SourceView>,
    /// `None` until some source can produce a state.
    pub solution: Option<Solution>,
    pub track: Option<Arc<Track>>,
    /// One set per observer site; empty until the first prediction completes.
    pub passes: Vec<PassSet>,
    /// Beta angle at daily samples from 30 days before to 30 days after the prediction time, degrees.
    /// Beyond the OEM span this is GP/SGP4 extrapolation.
    pub beta_series: Arc<Vec<(Utc, f64)>>,
}

/// Where an observer site came from (DESIGN.md §8).
#[derive(Debug, Clone, PartialEq)]
pub struct SiteView {
    pub name: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
    /// `CONFIGURED`, `ADDED` (this session) or `DEFAULT` (the fallback).
    pub origin: String,
    pub sun_az_deg: Option<f64>,
    pub sun_el_deg: Option<f64>,
    pub moon_az_deg: Option<f64>,
    pub moon_el_deg: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObserverView {
    pub sites: Vec<SiteView>,
    pub active: usize,
    pub mask_deg: f64,
}

impl ObserverView {
    pub fn active_site(&self) -> Option<&SiteView> {
        self.sites.get(self.active)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookView {
    pub az_deg: f64,
    pub el_deg: f64,
    pub range_km: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PassView {
    pub aos: Utc,
    pub tca: Utc,
    pub los: Utc,
    pub max_el_deg: f64,
    pub aos_az_deg: f64,
    pub tca_az_deg: f64,
    pub los_az_deg: f64,
    pub started_before_window: bool,
    pub ends_after_window: bool,
    /// Dark sky at the site and the vehicle lit at culmination; `None` when the Sun cannot be computed.
    pub visible: Option<bool>,
    /// Sky track as `[azimuth, elevation]` degrees from AOS to LOS.
    pub path: Vec<[f32; 2]>,
}

impl PassView {
    pub fn duration_s(&self) -> f64 {
        self.los.minus(self.aos)
    }
}

/// Predicted passes of one vehicle over one site.
#[derive(Debug, Clone, PartialEq)]
pub struct PassSet {
    pub site: String,
    /// Window start the prediction was made for.
    pub from: Utc,
    pub to: Utc,
    /// Trajectory source(s) used, e.g. `OEM then GP/SGP4`.
    pub source: String,
    pub passes: Vec<PassView>,
    pub look_now: Option<LookView>,
}

/// Earth-orientation data status (DESIGN.md §6.3: assume zero UT1-UTC, but say so).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EopView {
    Loaded {
        covers_to: Utc,
        fetched_at: Option<Utc>,
        from_cache: bool,
    },
    Assumed {
        reason: String,
    },
}

/// Simulation clock mode (DESIGN.md FR-20). REPLAY joins with the recording store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMode {
    Live,
    Sim,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockView {
    pub mode: ClockMode,
    pub rate: f64,
    pub paused: bool,
}

impl ClockView {
    pub const LIVE: Self = Self {
        mode: ClockMode::Live,
        rate: 1.0,
        paused: false,
    };

    pub fn is_live(&self) -> bool {
        self.mode == ClockMode::Live
    }
}

/// One line of the event log (DESIGN.md FR-42 foreshadows the full C&W log; this records system events).
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// Wall-clock time of the event.
    pub at: Utc,
    pub text: String,
}

/// One telemetry item's latest value (DESIGN.md FR-3, FR-6).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryItem {
    pub id: String,
    pub group: String,
    /// Documentation text from the item list; not verified against the vehicle.
    pub description: String,
    pub label: String,
    pub section: String,
    pub row: String,
    /// Column heading when the section is a matrix; empty for a plain list.
    pub col: String,
    pub units: String,
    /// The numeric value, when the feed's text parses as one.
    pub value: Option<f64>,
    pub text: Option<String>,
    /// Receive time at this client.
    pub rx: Utc,
    /// Seconds between receive time and the receive wall clock now; computed by the engine.
    pub age_s: f64,
    /// Recent numeric values, oldest first, one per `HISTORY_PERIOD_S` (empty for text items).
    pub history: Arc<[f32]>,
}

/// Spacing of the recent-value samples kept for each telemetry item, seconds.
pub const HISTORY_PERIOD_S: f64 = 10.0;
/// Samples kept per item (15 minutes at the period above).
pub const HISTORY_LEN: usize = 90;

#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryView {
    pub health: Health,
    /// Items in configuration order; an item that has not been received yet is absent.
    pub items: Vec<TelemetryItem>,
    /// Total items the feed is subscribed to.
    pub subscribed: usize,
    /// Subscribed item count per group.
    pub group_totals: Vec<(String, usize)>,
}

impl TelemetryView {
    /// Items subscribed in `group`, received or not.
    pub fn total_in(&self, group: &str) -> usize {
        self.group_totals
            .iter()
            .find(|(g, _)| g == group)
            .map_or(0, |(_, n)| *n)
    }

    pub fn get(&self, id: &str) -> Option<&TelemetryItem> {
        self.items.iter().find(|i| i.id == id)
    }
}

/// One line on a trend plot.
#[derive(Debug, Clone, PartialEq)]
pub struct TrendSeries {
    pub label: String,
    /// (POSIX seconds, value)
    pub points: Vec<(f64, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrendPlot {
    pub title: String,
    pub units: String,
    pub series: Vec<TrendSeries>,
}

/// Time series computed from the recording (DESIGN.md §9.6 F2.3).
#[derive(Debug, Clone, PartialEq)]
pub struct TrendSet {
    pub vehicle: String,
    pub from: Utc,
    pub to: Utc,
    pub plots: Vec<TrendPlot>,
    /// Epochs of recorded GP element sets inside the window; a reboost shows as a step at one of them.
    pub element_marks: Vec<Utc>,
    /// Recorded state rows the series were computed from.
    pub rows: usize,
}

/// One threshold rule's published state.
#[derive(Debug, Clone, PartialEq)]
pub struct CwRuleView {
    pub id: String,
    pub title: String,
    pub state: crate::cw::CwState,
    /// The watched value, when there is one.
    pub value: Option<f64>,
    pub units: String,
    pub caution: Option<f64>,
    pub warning: Option<f64>,
    pub basis: String,
}

/// Caution and warning picture: rules, standing conditions and the master latches (FR-40..42).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwView {
    pub rules: Vec<CwRuleView>,
    pub conditions: Vec<crate::cw::Condition>,
    pub latch: crate::cw::Latch,
}

impl CwView {
    /// Worst severity annunciating now, for the given vehicle (global conditions always count).
    pub fn worst_now(&self, vehicle: Option<&str>) -> Option<crate::cw::Severity> {
        let r = self.rules.iter().filter_map(|r| r.state.severity());
        let c = self
            .conditions
            .iter()
            .filter(|c| c.vehicle.is_none() || c.vehicle.as_deref() == vehicle)
            .map(|c| c.severity);
        r.chain(c).max()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct World {
    pub t: Utc,
    pub clock: ClockView,
    /// Most recent events, oldest first.
    pub log: Vec<LogEntry>,
    /// Increments every published snapshot.
    pub seq: u64,
    pub eop: EopView,
    /// ISSLIVE telemetry, present only when a configured vehicle uses it.
    pub telemetry: Option<TelemetryView>,
    pub cw: CwView,
    pub observer: ObserverView,
    /// Latest trend computation requested by the UI.
    pub trends: Option<Arc<TrendSet>>,
    pub vehicles: Vec<VehicleView>,
}

impl World {
    /// Empty world at `t`: the state before any data has arrived.
    pub fn empty(t: Utc) -> Self {
        Self {
            t,
            clock: ClockView::LIVE,
            log: Vec::new(),
            seq: 0,
            eop: EopView::Assumed {
                reason: "starting".into(),
            },
            telemetry: None,
            cw: CwView::default(),
            trends: None,
            observer: ObserverView {
                sites: Vec::new(),
                active: 0,
                mask_deg: 10.0,
            },
            vehicles: Vec::new(),
        }
    }
}
