//! The `:` command line (DESIGN.md FR-72): parsing and completion. Pure functions, so every command
//! and every error message is unit-tested without a window.

use mctl_core::command::{Command, check_rate, parse_offset, parse_seek};

use crate::pages::Page;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Scanlines,
    Bloom,
    Persistence,
    Curvature,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Quit,
    ToggleHelp,
    Theme(String),
    Vehicle(String),
    GoPage {
        page: Page,
        sub: Option<usize>,
    },
    Fx {
        which: Option<Effect>,
        on: Option<bool>,
    },
    Altitude(usize),
    TrendWindow(f64),
    CameraEarthFixed(bool),
    FocusVehicle(bool),
    StateEcef(bool),
    /// Edit the maneuver plan of the selected vehicle.
    Plan(mctl_core::plan::PlanCommand),
    /// Recompute the selected vehicle's plan from the current clock time.
    PlanRun,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Engine(Command),
    Ui(UiAction),
}

/// Names the completer and parser may need.
pub struct Names<'a> {
    pub themes: &'a [String],
    pub vehicles: &'a [String],
    pub sites: &'a [String],
}

/// Every command and a one-line usage, in the order shown by `help`.
pub const COMMANDS: &[(&str, &str)] = &[
    (
        "warp",
        "warp <rate>        simulation rate, e.g. warp 60 or warp -300 (|rate| 0.01..10000)",
    ),
    (
        "seek",
        "seek <time>        +2h, -30m, 90s, now, or 2026-10-25T12:00:00 (UTC)",
    ),
    ("step", "step <offset>      advance by +5m / -30s and pause"),
    (
        "ack",
        "ack                acknowledge the master caution/warning latch",
    ),
    (
        "replay",
        "replay [<time>]    play back the recording from its start, an epoch, or an offset such as -2h",
    ),
    ("pause", "pause              freeze the simulation clock"),
    ("resume", "resume             continue from a pause"),
    ("live", "live               return to wall-clock time"),
    (
        "page",
        "page <name> [sub]  master traj gnc eps tcs comm pass prox view sys",
    ),
    ("vehicle", "vehicle <id>       switch vehicle"),
    (
        "site",
        "site <name>        active observer site; `site add <name> <lat> <lon> [alt_m]` adds one; `site save [name]` keeps the active site",
    ),
    ("theme", "theme <name>       switch theme"),
    (
        "fx",
        "fx <effect> [on|off]  scanlines bloom persistence crt, or all",
    ),
    ("alt", "alt <1|2|5|10>     orbit altitude exaggeration"),
    (
        "trend",
        "trend <6h|24h|7d|30d|90d>  window of the trend plots (TRAJ, elements and trends)",
    ),
    (
        "burn",
        "burn add <an|dn|apo|peri|epoch> <vnb|rtn> <dx> <dy> <dz>  (m/s) | burn del <n> | burn clear",
    ),
    (
        "plan",
        "plan run | horizon <revs> | force <zonal|sun|moon|drag|srp> <on|off> | drag <scale> | save|load <name>",
    ),
    ("camera", "camera <inertial|earthfixed>"),
    ("focus", "focus <earth|vehicle>"),
    ("frame", "frame <eci|ecef>   state-vector frame"),
    ("help", "help               key and command reference"),
    ("quit", "quit               leave mctl"),
];

/// Where a replay starts: nothing or `start` is the beginning of the recording, an offset (`-2h`) is
/// relative to now, anything else must be an ISO epoch.
pub fn parse_replay_start(arg: Option<&str>) -> Result<Option<mctl_core::time::Utc>, String> {
    match arg {
        None | Some("start") => Ok(None),
        Some(a) if a.starts_with(['-', '+']) => {
            let secs = mctl_core::command::parse_offset(a).map_err(|e| e.to_string())?;
            let now = mctl_core::time::Utc::now().map_err(|e| e.to_string())?;
            Ok(Some(now.plus(secs)))
        }
        Some(a) => mctl_core::time::Utc::parse(a)
            .map(Some)
            .map_err(|e| format!("replay: {e}")),
    }
}

fn one_arg<'a>(cmd: &str, args: &'a [&'a str]) -> Result<&'a str, String> {
    match args {
        [a] => Ok(a),
        [] => Err(format!("{cmd}: missing argument (see help)")),
        _ => Err(format!("{cmd}: too many arguments")),
    }
}

fn on_off(a: &str) -> Result<bool, String> {
    match a.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Ok(true),
        "off" | "0" | "false" => Ok(false),
        _ => Err(format!("expected on or off, got {a:?}")),
    }
}

/// Case-insensitive exact match, or a unique prefix, among `options`.
fn pick<'a>(what: &str, arg: &str, options: &'a [String]) -> Result<&'a String, String> {
    let a = arg.to_ascii_lowercase();
    if let Some(o) = options.iter().find(|o| o.to_ascii_lowercase() == a) {
        return Ok(o);
    }
    let hits: Vec<&String> = options
        .iter()
        .filter(|o| o.to_ascii_lowercase().starts_with(&a))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "unknown {what} {arg:?}; known: {}",
            options.join(", ")
        )),
        many => Err(format!(
            "{what} {arg:?} is ambiguous: {}",
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Parse one command line (without the leading `:`).
pub fn parse(line: &str, names: &Names<'_>) -> Result<Action, String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let Some((cmd, args)) = parts.split_first() else {
        return Err("empty command".into());
    };
    let cmd = cmd.to_ascii_lowercase();
    let hits: Vec<&str> = COMMANDS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| n.starts_with(&cmd))
        .collect();
    let cmd = if COMMANDS.iter().any(|(n, _)| *n == cmd) {
        cmd.as_str()
    } else {
        match hits.as_slice() {
            [one] => one,
            [] => return Err(format!("unknown command {cmd:?}; try help")),
            many => return Err(format!("{cmd:?} is ambiguous: {}", many.join(", "))),
        }
    };
    let eng = |c| Ok(Action::Engine(c));
    let ui = |a| Ok(Action::Ui(a));
    match cmd {
        "warp" => {
            let a = one_arg(cmd, args)?;
            let r: f64 = a
                .trim_end_matches('x')
                .parse()
                .map_err(|_| format!("warp: {a:?} is not a number"))?;
            eng(Command::SetRate(check_rate(r).map_err(|e| e.to_string())?))
        }
        "seek" => {
            if args.is_empty() {
                return Err("seek: missing time (+2h, -30m, now, or an ISO epoch)".into());
            }
            eng(parse_seek(&args.join(" ")).map_err(|e| format!("seek: {e}"))?)
        }
        "step" => eng(Command::Step(
            parse_offset(one_arg(cmd, args)?).map_err(|e| format!("step: {e}"))?,
        )),
        "ack" => eng(Command::Ack),
        "site" => match args {
            [] => Err("site: missing name (or: site add <name> <lat> <lon> [alt_m])".into()),
            [a] if a.eq_ignore_ascii_case("save") => eng(Command::SaveSite(None)),
            [a, name] if a.eq_ignore_ascii_case("save") => {
                eng(Command::SaveSite(Some((*name).to_owned())))
            }
            [a, name, rest @ ..] if a.eq_ignore_ascii_case("add") => {
                let num = |what: &str, s: &str| {
                    s.parse::<f64>()
                        .map_err(|_| format!("site add: {what} {s:?} is not a number"))
                };
                match rest {
                    [lat, lon] => eng(Command::AddSite {
                        name: (*name).to_owned(),
                        lat_deg: num("latitude", lat)?,
                        lon_deg: num("longitude", lon)?,
                        alt_m: 0.0,
                    }),
                    [lat, lon, alt] => eng(Command::AddSite {
                        name: (*name).to_owned(),
                        lat_deg: num("latitude", lat)?,
                        lon_deg: num("longitude", lon)?,
                        alt_m: num("altitude", alt)?,
                    }),
                    _ => Err("site add: expected <name> <lat> <lon> [alt_m]".into()),
                }
            }
            [name] => eng(Command::SelectSite(
                pick("site", name, names.sites)?.clone(),
            )),
            _ => Err("site: too many arguments".into()),
        },
        "pause" => eng(Command::Pause),
        "resume" => eng(Command::Resume),
        "live" => eng(Command::Live),
        "page" => {
            let (name, sub) = match args {
                [n] => (*n, None),
                [n, s] => (*n, Some(*s)),
                [] => return Err("page: missing page name".into()),
                _ => return Err("page: too many arguments".into()),
            };
            let page = Page::from_name(name).ok_or_else(|| {
                format!(
                    "unknown page {name:?}; known: {}",
                    crate::pages::ALL_PAGES.map(Page::key).join(" ")
                )
            })?;
            let sub = match sub {
                None => None,
                Some(s) => Some(page.resolve_sub(s).ok_or_else(|| {
                    format!(
                        "{} has no sub-page {s:?}; known: {}",
                        page.key(),
                        page.subpages().join(" | ")
                    )
                })?),
            };
            ui(UiAction::GoPage { page, sub })
        }
        "vehicle" => ui(UiAction::Vehicle(
            pick("vehicle", one_arg(cmd, args)?, names.vehicles)?.clone(),
        )),
        "theme" => ui(UiAction::Theme(
            pick("theme", one_arg(cmd, args)?, names.themes)?.clone(),
        )),
        "fx" => {
            let (which, on) = match args {
                [w] => (*w, None),
                [w, o] => (*w, Some(on_off(o)?)),
                [] => return Err("fx: missing effect (scanlines bloom persistence crt all)".into()),
                _ => return Err("fx: too many arguments".into()),
            };
            let which = match which.to_ascii_lowercase().as_str() {
                "all" => None,
                "scanlines" | "scan" => Some(Effect::Scanlines),
                "bloom" => Some(Effect::Bloom),
                "persistence" | "persist" => Some(Effect::Persistence),
                "crt" | "curvature" => Some(Effect::Curvature),
                other => {
                    return Err(format!(
                        "unknown effect {other:?}; known: scanlines bloom persistence crt all"
                    ));
                }
            };
            ui(UiAction::Fx { which, on })
        }
        "alt" => {
            let a = one_arg(cmd, args)?;
            let steps = crate::globe::EXAGGERATION_STEPS;
            let v: f32 = a
                .trim_start_matches('x')
                .parse()
                .map_err(|_| format!("alt: {a:?} is not a number"))?;
            let idx = steps
                .iter()
                .position(|s| (*s - v).abs() < 1e-6)
                .ok_or_else(|| {
                    format!(
                        "alt: choose one of {}",
                        steps.map(|s| format!("{s}")).join(", ")
                    )
                })?;
            ui(UiAction::Altitude(idx))
        }
        "trend" => ui(UiAction::TrendWindow(crate::trendplot::parse_window(
            one_arg(cmd, args)?,
        )?)),
        "camera" => match one_arg(cmd, args)?.to_ascii_lowercase().as_str() {
            "inertial" => ui(UiAction::CameraEarthFixed(false)),
            "earthfixed" | "earth-fixed" | "ecef" => ui(UiAction::CameraEarthFixed(true)),
            o => Err(format!(
                "camera: expected inertial or earthfixed, got {o:?}"
            )),
        },
        "focus" => match one_arg(cmd, args)?.to_ascii_lowercase().as_str() {
            "earth" => ui(UiAction::FocusVehicle(false)),
            "vehicle" => ui(UiAction::FocusVehicle(true)),
            o => Err(format!("focus: expected earth or vehicle, got {o:?}")),
        },
        "frame" => match one_arg(cmd, args)?.to_ascii_lowercase().as_str() {
            "eci" | "gcrf" => ui(UiAction::StateEcef(false)),
            "ecef" | "itrf" => ui(UiAction::StateEcef(true)),
            o => Err(format!("frame: expected eci or ecef, got {o:?}")),
        },
        "replay" => eng(Command::Replay(parse_replay_start(args.first().copied())?)),
        "burn" => {
            use mctl_core::plan::{BurnDef, BurnFrame, PlanCommand};
            let num = |a: &str| {
                a.parse::<f64>()
                    .map_err(|_| format!("burn: {a:?} is not a number"))
            };
            match args {
                ["add", trig, frame, dx, dy, dz] => {
                    let b = BurnDef {
                        trigger: (*trig).to_owned(),
                        frame: BurnFrame::parse(frame).map_err(|e| e.to_string())?,
                        dv_m_s: [num(dx)?, num(dy)?, num(dz)?],
                    };
                    b.validate().map_err(|e| e.to_string())?;
                    ui(UiAction::Plan(PlanCommand::AddBurn(b)))
                }
                ["del", n] => {
                    let n: usize = n.parse().map_err(|_| format!("burn del: {n:?} is not a burn number"))?;
                    ui(UiAction::Plan(PlanCommand::DelBurn(n)))
                }
                ["clear"] => ui(UiAction::Plan(PlanCommand::Clear)),
                _ => Err("burn: use 'burn add <trigger> <vnb|rtn> <dx> <dy> <dz>', 'burn del <n>' or 'burn clear'".into()),
            }
        }
        "plan" => {
            use mctl_core::plan::PlanCommand;
            match args {
                ["run"] => ui(UiAction::PlanRun),
                ["horizon", r] => ui(UiAction::Plan(PlanCommand::Horizon(
                    r.parse().map_err(|_| format!("plan horizon: {r:?} is not a number of revolutions"))?,
                ))),
                ["force", name, onoff] => ui(UiAction::Plan(PlanCommand::Force {
                    name: name.to_ascii_lowercase(),
                    on: on_off(onoff)?,
                })),
                ["drag", x] => ui(UiAction::Plan(PlanCommand::DragScale(
                    x.parse().map_err(|_| format!("plan drag: {x:?} is not a number"))?,
                ))),
                ["save", n] => ui(UiAction::Plan(PlanCommand::Save((*n).to_owned()))),
                ["load", n] => ui(UiAction::Plan(PlanCommand::Load((*n).to_owned()))),
                _ => Err("plan: run | horizon <revs> | force <name> <on|off> | drag <scale> | save <name> | load <name>".into()),
            }
        }
        "help" => ui(UiAction::ToggleHelp),
        "quit" => ui(UiAction::Quit),
        _ => unreachable!("every command in COMMANDS is handled"),
    }
}

/// Result of Tab completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The full line after completing as far as is unambiguous.
    pub line: String,
    /// Remaining options when ambiguous, for display.
    pub candidates: Vec<String>,
}

fn complete_word(prefix: &str, options: &[String]) -> (String, Vec<String>) {
    let p = prefix.to_ascii_lowercase();
    let hits: Vec<String> = options
        .iter()
        .filter(|o| o.to_ascii_lowercase().starts_with(&p))
        .cloned()
        .collect();
    match hits.as_slice() {
        [] => (prefix.to_owned(), vec![]),
        [one] => (format!("{one} "), vec![]),
        many => {
            // Longest common prefix (case-insensitive on the first candidate's spelling).
            let first = many[0].to_ascii_lowercase();
            let mut n = first.len();
            for h in &many[1..] {
                n = n.min(
                    first
                        .chars()
                        .zip(h.to_ascii_lowercase().chars())
                        .take_while(|(a, b)| a == b)
                        .count(),
                );
            }
            (many[0][..n].to_owned(), many.to_vec())
        }
    }
}

/// Complete the word under the cursor (the last word of `line`).
pub fn complete(line: &str, names: &Names<'_>) -> Completion {
    let trailing_space = line.ends_with(char::is_whitespace);
    let mut words: Vec<&str> = line.split_whitespace().collect();
    let prefix = if trailing_space {
        ""
    } else {
        words.pop().unwrap_or("")
    };
    let done = |w: &[&str]| {
        if w.is_empty() {
            String::new()
        } else {
            format!("{} ", w.join(" "))
        }
    };
    let options: Vec<String> = match words.first().map(|c| c.to_ascii_lowercase()) {
        None => COMMANDS.iter().map(|(n, _)| (*n).to_owned()).collect(),
        Some(c) => {
            let cmd = COMMANDS
                .iter()
                .map(|(n, _)| *n)
                .find(|n| *n == c)
                .or_else(|| {
                    let h: Vec<&str> = COMMANDS
                        .iter()
                        .map(|(n, _)| *n)
                        .filter(|n| n.starts_with(&c))
                        .collect();
                    (h.len() == 1).then(|| h[0])
                });
            match (cmd, words.len()) {
                (Some("theme"), 1) => names.themes.to_vec(),
                (Some("vehicle"), 1) => names.vehicles.to_vec(),
                (Some("site"), 1) => names
                    .sites
                    .iter()
                    .cloned()
                    .chain(["add".to_owned(), "save".to_owned()])
                    .collect(),
                (Some("page"), 1) => crate::pages::ALL_PAGES.map(|p| p.key().to_owned()).to_vec(),
                (Some("page"), 2) => Page::from_name(words[1]).map_or(vec![], |p| {
                    p.subpages()
                        .iter()
                        .flat_map(|t| {
                            t.to_ascii_lowercase()
                                .split([' ', '/'])
                                .map(str::to_owned)
                                .collect::<Vec<_>>()
                        })
                        .collect()
                }),
                (Some("fx"), 1) => ["scanlines", "bloom", "persistence", "crt", "all"]
                    .map(str::to_owned)
                    .to_vec(),
                (Some("fx"), 2) => vec!["on".into(), "off".into()],
                (Some("alt"), 1) => crate::globe::EXAGGERATION_STEPS
                    .map(|s| format!("{s}"))
                    .to_vec(),
                (Some("trend"), 1) => crate::trendplot::WINDOWS
                    .map(|(l, _)| l.to_owned())
                    .to_vec(),
                (Some("camera"), 1) => vec!["inertial".into(), "earthfixed".into()],
                (Some("focus"), 1) => vec!["earth".into(), "vehicle".into()],
                (Some("frame"), 1) => vec!["eci".into(), "ecef".into()],
                (Some("seek"), 1) => vec!["now".into()],
                (Some("burn"), 1) => vec!["add".into(), "del".into(), "clear".into()],
                (Some("burn"), 2) => ["an", "dn", "apo", "peri"].map(String::from).to_vec(),
                (Some("burn"), 3) => vec!["vnb".into(), "rtn".into()],
                (Some("plan"), 1) => ["run", "horizon", "force", "drag", "save", "load"]
                    .map(String::from)
                    .to_vec(),
                (Some("plan"), 2) if words.get(1).is_some_and(|w| *w == "force") => {
                    ["zonal", "sun", "moon", "drag", "srp"]
                        .map(String::from)
                        .to_vec()
                }
                _ => vec![],
            }
        }
    };
    let (completed, candidates) = complete_word(prefix, &options);
    Completion {
        line: format!("{}{}", done(&words), completed),
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use mctl_core::time::Utc;

    use super::*;

    fn names() -> (Vec<String>, Vec<String>) {
        (
            vec![
                "Phosphor".into(),
                "Retro".into(),
                "Modern".into(),
                "Retrowave".into(),
            ],
            vec!["iss".into(), "hubble".into()],
        )
    }

    fn p(line: &str) -> Result<Action, String> {
        let (t, v) = names();
        parse(
            line,
            &Names {
                themes: &t,
                vehicles: &v,
                sites: &["NASA HQ".to_owned(), "Home".to_owned()],
            },
        )
    }

    fn c(line: &str) -> Completion {
        let (t, v) = names();
        complete(
            line,
            &Names {
                themes: &t,
                vehicles: &v,
                sites: &["NASA HQ".to_owned(), "Home".to_owned()],
            },
        )
    }

    #[test]
    fn clock_commands_map_onto_engine_commands() {
        assert_eq!(p("warp 60"), Ok(Action::Engine(Command::SetRate(60.0))));
        assert_eq!(p("warp -300"), Ok(Action::Engine(Command::SetRate(-300.0))));
        assert_eq!(p("warp 60x"), Ok(Action::Engine(Command::SetRate(60.0))));
        assert_eq!(
            p("seek +2h"),
            Ok(Action::Engine(Command::SeekRelative(7200.0)))
        );
        assert_eq!(p("seek now"), Ok(Action::Engine(Command::Live)));
        assert_eq!(
            p("seek 2026-10-25T12:00:00"),
            Ok(Action::Engine(Command::Seek(
                Utc::parse("2026-10-25T12:00:00").unwrap()
            )))
        );
        assert_eq!(p("step -30s"), Ok(Action::Engine(Command::Step(-30.0))));
        assert_eq!(p("pause"), Ok(Action::Engine(Command::Pause)));
        assert_eq!(p("resume"), Ok(Action::Engine(Command::Resume)));
        assert_eq!(p("live"), Ok(Action::Engine(Command::Live)));
    }

    #[test]
    fn bad_clock_arguments_say_what_is_wrong() {
        assert!(p("warp").unwrap_err().contains("missing"));
        assert!(p("warp fast").unwrap_err().contains("not a number"));
        assert!(p("warp 99999").unwrap_err().contains("outside"));
        assert!(p("warp 0").unwrap_err().contains("outside"));
        assert!(p("warp 1 2").unwrap_err().contains("too many"));
        assert!(p("seek").unwrap_err().contains("missing"));
        assert!(p("seek soon").is_err());
        assert!(p("step 5q").unwrap_err().contains("unit"));
    }

    #[test]
    fn ui_commands_resolve_names_and_unique_prefixes() {
        assert_eq!(
            p("theme retrowave"),
            Ok(Action::Ui(UiAction::Theme("Retrowave".into())))
        );
        assert_eq!(
            p("theme mod"),
            Ok(Action::Ui(UiAction::Theme("Modern".into())))
        );
        assert!(p("theme re").unwrap_err().contains("ambiguous"));
        assert!(p("theme nope").unwrap_err().contains("known: Phosphor"));
        assert_eq!(
            p("vehicle hub"),
            Ok(Action::Ui(UiAction::Vehicle("hubble".into())))
        );
        assert!(
            p("vehicle dragon")
                .unwrap_err()
                .contains("known: iss, hubble")
        );
        assert_eq!(
            p("page traj ground"),
            Ok(Action::Ui(UiAction::GoPage {
                page: Page::Traj,
                sub: Some(1)
            }))
        );
        assert_eq!(
            p("page sys"),
            Ok(Action::Ui(UiAction::GoPage {
                page: Page::Sys,
                sub: None
            }))
        );
        assert!(p("page zzz").unwrap_err().contains("known: master"));
        assert!(p("page traj 9").unwrap_err().contains("no sub-page"));
        assert_eq!(
            p("fx bloom off"),
            Ok(Action::Ui(UiAction::Fx {
                which: Some(Effect::Bloom),
                on: Some(false)
            }))
        );
        assert_eq!(
            p("fx all"),
            Ok(Action::Ui(UiAction::Fx {
                which: None,
                on: None
            }))
        );
        assert!(p("fx glow").unwrap_err().contains("unknown effect"));
        assert!(p("fx bloom maybe").unwrap_err().contains("on or off"));
        assert_eq!(p("alt 5"), Ok(Action::Ui(UiAction::Altitude(2))));
        assert!(p("alt 3").unwrap_err().contains("choose one of"));
        assert_eq!(
            p("camera earthfixed"),
            Ok(Action::Ui(UiAction::CameraEarthFixed(true)))
        );
        assert_eq!(
            p("focus vehicle"),
            Ok(Action::Ui(UiAction::FocusVehicle(true)))
        );
        assert_eq!(p("frame ecef"), Ok(Action::Ui(UiAction::StateEcef(true))));
        assert_eq!(p("quit"), Ok(Action::Ui(UiAction::Quit)));
        assert_eq!(p("help"), Ok(Action::Ui(UiAction::ToggleHelp)));
    }

    #[test]
    fn commands_accept_unique_prefixes_and_reject_ambiguous_or_unknown_ones() {
        assert_eq!(p("w 10"), Ok(Action::Engine(Command::SetRate(10.0))));
        assert_eq!(
            p("WARP 10"),
            Ok(Action::Engine(Command::SetRate(10.0))),
            "case-insensitive"
        );
        assert!(p("s 5").unwrap_err().contains("ambiguous"), "seek/step");
        assert!(
            p("burn add next-apo vnb 0.5 0 0")
                .unwrap_err()
                .contains("trigger"),
            "a bad trigger names itself"
        );
        assert!(p("").unwrap_err().contains("empty"));
        assert!(p("   ").unwrap_err().contains("empty"));
    }

    #[test]
    fn completion_finishes_unique_words_and_lists_ambiguous_ones() {
        assert_eq!(c("wa").line, "warp ");
        assert_eq!(c("th").line, "theme ");
        assert_eq!(c("theme retrow").line, "theme Retrowave ");
        let amb = c("theme re");
        assert_eq!(amb.line, "theme Retro", "longest common prefix");
        assert_eq!(
            amb.candidates,
            vec!["Retro".to_string(), "Retrowave".to_string()]
        );
        assert_eq!(c("page traj gr").line, "page traj ground ");
        assert_eq!(c("fx b").line, "fx bloom ");
        assert_eq!(
            c("fx bloom o").candidates,
            vec!["on".to_string(), "off".to_string()]
        );
        assert_eq!(c("vehicle h").line, "vehicle hubble ");
        assert!(c("s").candidates.len() > 1, "seek, step");
        assert_eq!(c("zzz").line, "zzz", "no match leaves the text alone");
        assert_eq!(
            c("").candidates.len(),
            COMMANDS.len(),
            "empty line lists every command"
        );
        assert_eq!(c("theme ").candidates.len(), 4);
    }

    #[test]
    fn site_commands_select_by_prefix_and_add_with_validation() {
        assert_eq!(
            p("site home"),
            Ok(Action::Engine(Command::SelectSite("Home".into())))
        );
        assert_eq!(
            p("site nasa"),
            Ok(Action::Engine(Command::SelectSite("NASA HQ".into())))
        );
        assert!(
            p("site atlantis")
                .unwrap_err()
                .contains("known: NASA HQ, Home")
        );
        assert_eq!(
            p("site add TEST 38.883 -77.0163 10"),
            Ok(Action::Engine(Command::AddSite {
                name: "TEST".into(),
                lat_deg: 38.883,
                lon_deg: -77.0163,
                alt_m: 10.0
            }))
        );
        assert_eq!(
            p("site add Sea 0 0"),
            Ok(Action::Engine(Command::AddSite {
                name: "Sea".into(),
                lat_deg: 0.0,
                lon_deg: 0.0,
                alt_m: 0.0
            }))
        );
        assert!(
            p("site add X north 0")
                .unwrap_err()
                .contains("not a number")
        );
        assert!(p("site add X 1").unwrap_err().contains("expected <name>"));
        assert!(p("site").unwrap_err().contains("missing"));
        assert_eq!(c("site h").line, "site Home ");
    }

    #[test]
    fn burn_and_plan_commands_parse_into_plan_edits() {
        use mctl_core::plan::{BurnDef, BurnFrame, PlanCommand};
        let ui = |c| Ok(Action::Ui(UiAction::Plan(c)));
        assert_eq!(
            p("burn add apo vnb 1 0 -0.5"),
            ui(PlanCommand::AddBurn(BurnDef {
                trigger: "apo".into(),
                frame: BurnFrame::Vnb,
                dv_m_s: [1.0, 0.0, -0.5]
            }))
        );
        assert!(p("burn add 2026-10-25T12:00:00 rtn 0 1 0").is_ok());
        assert_eq!(p("burn del 2"), ui(PlanCommand::DelBurn(2)));
        assert_eq!(p("burn clear"), ui(PlanCommand::Clear));
        assert!(p("burn add apo vnb 1 0").unwrap_err().contains("burn:"));
        assert!(p("burn add apo xyz 1 0 0").unwrap_err().contains("frame"));
        assert!(
            p("burn add apo vnb 5000 0 0")
                .unwrap_err()
                .contains("exceeds")
        );
        assert!(p("burn del x").unwrap_err().contains("burn number"));
        assert_eq!(p("plan run"), Ok(Action::Ui(UiAction::PlanRun)));
        assert_eq!(p("plan horizon 5"), ui(PlanCommand::Horizon(5.0)));
        assert_eq!(
            p("plan force Drag off"),
            ui(PlanCommand::Force {
                name: "drag".into(),
                on: false
            })
        );
        assert_eq!(p("plan drag 0.4"), ui(PlanCommand::DragScale(0.4)));
        assert_eq!(
            p("plan save test-1"),
            ui(PlanCommand::Save("test-1".into()))
        );
        assert!(p("plan force drag maybe").is_err());
        assert!(p("plan").unwrap_err().contains("plan:"));
        assert_eq!(c("burn a").line, "burn add ");
    }

    #[test]
    fn every_listed_command_parses_with_valid_arguments() {
        for (name, _) in COMMANDS {
            let arg = match *name {
                "warp" => " 10",
                "seek" => " +1h",
                "step" => " 5s",
                "page" => " master",
                "vehicle" => " iss",
                "theme" => " modern",
                "site" => " home",
                "fx" => " all",
                "alt" => " 2",
                "camera" => " inertial",
                "trend" => " 24h",
                "focus" => " earth",
                "frame" => " eci",
                "burn" => " clear",
                "replay" => " start",
                "plan" => " run",
                _ => "",
            };
            assert!(p(&format!("{name}{arg}")).is_ok(), "{name}{arg}");
        }
    }
}
