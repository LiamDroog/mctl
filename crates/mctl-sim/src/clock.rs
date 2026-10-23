//! The simulation clock (DESIGN.md FR-20..22): LIVE follows the wall clock at 1x; SIM runs from any
//! epoch at any rate from -10000x to +10000x, can pause, step and scrub.
//!
//! Every method takes the wall time explicitly so behaviour is deterministic under test.

use std::time::Instant;

use mctl_core::command::{Command, check_rate};
use mctl_core::time::Utc;
use mctl_core::world::{ClockMode, ClockView};

/// Earliest and latest epochs the clock will go to. The lower bound is the start of UTC leap seconds;
/// the frame and time-scale models are not defined before it. The upper bound is far beyond any
/// element set's usefulness but keeps date arithmetic safe.
pub const MIN_EPOCH: (i32, u32, u32) = (1972, 1, 1);
pub const MAX_EPOCH: (i32, u32, u32) = (2099, 12, 31);

fn bound((y, m, d): (i32, u32, u32)) -> Utc {
    Utc::from_ymd_hms(y, m, d, 0, 0, 0.0).expect("bounds are valid dates")
}

#[derive(Debug, Clone)]
pub struct Clock {
    mode: ClockMode,
    /// SIM time at `anchor` (and the frozen time while paused).
    sim_epoch: Utc,
    anchor: Instant,
    rate: f64,
    paused: bool,
}

impl Clock {
    pub fn live(wall: Instant) -> Self {
        Self {
            mode: ClockMode::Live,
            sim_epoch: Utc(0.0),
            anchor: wall,
            rate: 1.0,
            paused: false,
        }
    }

    pub fn view(&self) -> ClockView {
        ClockView {
            mode: self.mode,
            rate: if self.mode == ClockMode::Live {
                1.0
            } else {
                self.rate
            },
            paused: self.mode == ClockMode::Sim && self.paused,
        }
    }

    /// Clock time at `wall` (`wall_utc` is the system clock reading at the same instant).
    pub fn now(&self, wall: Instant, wall_utc: Utc) -> Utc {
        match self.mode {
            ClockMode::Live => wall_utc,
            ClockMode::Sim if self.paused => self.sim_epoch,
            ClockMode::Sim => self
                .sim_epoch
                .plus(wall.saturating_duration_since(self.anchor).as_secs_f64() * self.rate),
        }
    }

    /// Enforce the epoch bounds each tick. Running into a bound pauses the clock exactly on it, so the
    /// frame models are never asked for an epoch they cannot handle. Returns a message when that happens.
    pub fn enforce_bounds(&mut self, wall: Instant, wall_utc: Utc) -> Option<String> {
        if self.mode != ClockMode::Sim || self.paused {
            return None;
        }
        let t = self.now(wall, wall_utc);
        let (lo, hi) = (bound(MIN_EPOCH), bound(MAX_EPOCH));
        let hit = if t.0 < lo.0 {
            Some(lo)
        } else if t.0 > hi.0 {
            Some(hi)
        } else {
            None
        }?;
        self.sim_epoch = hit;
        self.paused = true;
        Some(format!("clock reached its limit {} and paused", hit.iso()))
    }

    fn enter_sim_at(&mut self, t: Utc, wall: Instant) {
        self.mode = ClockMode::Sim;
        self.sim_epoch = t;
        self.anchor = wall;
    }

    /// Apply a command. `Ok` carries the text for the event log.
    pub fn apply(&mut self, cmd: &Command, wall: Instant, wall_utc: Utc) -> Result<String, String> {
        let now = self.now(wall, wall_utc);
        let in_range = |t: Utc| (bound(MIN_EPOCH).0..=bound(MAX_EPOCH).0).contains(&t.0);
        match cmd {
            Command::SetRate(r) => {
                let r = check_rate(*r).map_err(|e| e.to_string())?;
                if self.mode == ClockMode::Live {
                    self.paused = false;
                }
                self.enter_sim_at(now, wall);
                self.rate = r;
                Ok(format!("clock: SIM {r}x"))
            }
            Command::Pause => {
                self.enter_sim_at(now, wall);
                self.paused = true;
                Ok("clock: paused".into())
            }
            Command::Resume => {
                if self.mode == ClockMode::Live {
                    return Err("the clock is LIVE; there is nothing to resume".into());
                }
                self.enter_sim_at(now, wall);
                self.paused = false;
                Ok(format!("clock: resumed at {}x", self.rate))
            }
            Command::Seek(t) => {
                if !in_range(*t) {
                    return Err(format!(
                        "{} is outside {} .. {}",
                        t.iso(),
                        bound(MIN_EPOCH).iso(),
                        bound(MAX_EPOCH).iso()
                    ));
                }
                let was_live = self.mode == ClockMode::Live;
                self.enter_sim_at(*t, wall);
                if was_live {
                    self.rate = 1.0;
                    self.paused = false;
                }
                Ok(format!("clock: seek to {}", t.iso()))
            }
            Command::SeekRelative(dt) => self.apply(&Command::Seek(now.plus(*dt)), wall, wall_utc),
            Command::Step(dt) => {
                let t = now.plus(*dt);
                if !in_range(t) {
                    return Err(format!("{} is outside the clock range", t.iso()));
                }
                self.enter_sim_at(t, wall);
                self.paused = true;
                Ok(format!("clock: step {dt:+} s to {}", t.iso()))
            }
            Command::Live => {
                *self = Self::live(wall);
                Ok("clock: LIVE".into())
            }
            Command::Ack
            | Command::SelectSite(_)
            | Command::AddSite { .. }
            | Command::SaveSite(_) => Err("not a clock command".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(base: Instant, s: f64) -> Instant {
        base + Duration::from_secs_f64(s)
    }

    fn setup() -> (Instant, Utc, Clock) {
        let wall = Instant::now();
        (
            wall,
            Utc::parse("2026-10-23T12:00:00").unwrap(),
            Clock::live(wall),
        )
    }

    #[test]
    fn live_follows_the_wall_clock_exactly() {
        let (w, u, c) = setup();
        assert_eq!(c.now(at(w, 5.0), u.plus(5.0)), u.plus(5.0));
        assert!(c.view().is_live() && c.view().rate == 1.0 && !c.view().paused);
    }

    #[test]
    fn sim_rate_scales_elapsed_wall_time_in_both_directions() {
        let (w, u, mut c) = setup();
        c.apply(&Command::SetRate(60.0), w, u).unwrap();
        assert_eq!(c.now(at(w, 10.0), u.plus(10.0)).minus(u), 600.0);
        c.apply(&Command::SetRate(-10.0), at(w, 10.0), u.plus(10.0))
            .unwrap();
        // Re-anchored at +600 s; 5 more wall seconds at -10x go back 50 s.
        assert!((c.now(at(w, 15.0), u.plus(15.0)).minus(u) - 550.0).abs() < 1e-9);
        assert_eq!(c.view().rate, -10.0);
    }

    #[test]
    fn pause_freezes_and_resume_continues_from_the_frozen_time() {
        let (w, u, mut c) = setup();
        c.apply(&Command::SetRate(100.0), w, u).unwrap();
        c.apply(&Command::Pause, at(w, 2.0), u.plus(2.0)).unwrap();
        assert!(c.view().paused);
        assert_eq!(
            c.now(at(w, 50.0), u.plus(50.0)).minus(u),
            200.0,
            "frozen at 2 s x 100"
        );
        c.apply(&Command::Resume, at(w, 50.0), u.plus(50.0))
            .unwrap();
        assert_eq!(c.now(at(w, 51.0), u.plus(51.0)).minus(u), 300.0);
        assert!(c.apply(&Command::Resume, w, u).is_ok());
        let (w2, u2, mut live) = setup();
        assert!(
            live.apply(&Command::Resume, w2, u2).is_err(),
            "resuming a LIVE clock is an error"
        );
    }

    #[test]
    fn seek_from_live_starts_running_at_one_x_and_keeps_pause_when_already_sim() {
        let (w, u, mut c) = setup();
        let target = Utc::parse("2026-10-25T00:00:00").unwrap();
        c.apply(&Command::Seek(target), w, u).unwrap();
        assert_eq!(c.now(at(w, 30.0), u.plus(30.0)), target.plus(30.0));
        c.apply(&Command::Pause, at(w, 30.0), u.plus(30.0)).unwrap();
        c.apply(&Command::SeekRelative(3600.0), at(w, 40.0), u.plus(40.0))
            .unwrap();
        assert!(c.view().paused, "scrubbing while paused stays paused");
        assert_eq!(c.now(at(w, 99.0), u.plus(99.0)), target.plus(30.0 + 3600.0));
    }

    #[test]
    fn step_advances_and_pauses_even_from_live() {
        let (w, u, mut c) = setup();
        c.apply(&Command::Step(300.0), w, u).unwrap();
        assert!(c.view().paused && !c.view().is_live());
        assert_eq!(c.now(at(w, 10.0), u.plus(10.0)), u.plus(300.0));
        c.apply(&Command::Step(-60.0), at(w, 10.0), u.plus(10.0))
            .unwrap();
        assert_eq!(c.now(at(w, 20.0), u.plus(20.0)), u.plus(240.0));
    }

    #[test]
    fn live_command_returns_to_wall_time_from_any_state() {
        let (w, u, mut c) = setup();
        c.apply(&Command::SetRate(5000.0), w, u).unwrap();
        c.apply(&Command::Live, at(w, 1.0), u.plus(1.0)).unwrap();
        assert!(c.view().is_live());
        assert_eq!(c.now(at(w, 9.0), u.plus(9.0)), u.plus(9.0));
    }

    #[test]
    fn invalid_rates_and_epochs_are_rejected_without_changing_the_clock() {
        let (w, u, mut c) = setup();
        for r in [0.0, 20_000.0, f64::NAN] {
            assert!(c.apply(&Command::SetRate(r), w, u).is_err(), "{r}");
        }
        assert!(c.view().is_live(), "rejected commands must not leave LIVE");
        assert!(
            c.apply(
                &Command::Seek(Utc::parse("1960-01-01T00:00:00").unwrap()),
                w,
                u
            )
            .is_err()
        );
        assert!(
            c.apply(
                &Command::Seek(Utc::parse("2150-01-01T00:00:00").unwrap()),
                w,
                u
            )
            .is_err()
        );
        assert!(c.apply(&Command::SeekRelative(-1.0e10), w, u).is_err());
        assert!(c.view().is_live());
    }

    #[test]
    fn running_into_the_lower_bound_pauses_exactly_on_it() {
        let (w, u, mut c) = setup();
        c.apply(
            &Command::Seek(Utc::parse("1972-01-01T00:00:10").unwrap()),
            w,
            u,
        )
        .unwrap();
        c.apply(&Command::SetRate(-100.0), w, u).unwrap();
        assert!(
            c.enforce_bounds(at(w, 0.05), u).is_none(),
            "5 s of simulated time left"
        );
        let msg = c.enforce_bounds(at(w, 1.0), u).expect("must hit the bound");
        assert!(msg.contains("1972-01-01T00:00:00"));
        assert!(c.view().paused);
        assert_eq!(
            c.now(at(w, 5.0), u),
            Utc::parse("1972-01-01T00:00:00").unwrap()
        );
        assert!(c.enforce_bounds(at(w, 6.0), u).is_none(), "already paused");
    }
}
