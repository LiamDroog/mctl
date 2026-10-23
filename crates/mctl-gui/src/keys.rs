//! Remappable single-character key bindings (DESIGN.md §9.7, NFR-8). The defaults live in the
//! embedded `config/default.toml` under `[keys]`; a user config overrides any action.
//! Function keys, Tab, arrows, Enter and Escape keep fixed meanings.

use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    Quit,
    Help,
    CommandLine,
    Theme,
    Vehicle,
    Frame,
    Altitude,
    CameraFrame,
    Focus,
    ZoomIn,
    ZoomOut,
    Scanlines,
    Bloom,
    Persistence,
    Crt,
    AllEffects,
    TrendWindow,
    Live,
    Ack,
    Pause,
    WarpDown,
    WarpUp,
}

/// (action, config name, help text), in help order.
pub const ACTIONS: [(KeyAction, &str, &str); 22] = [
    (
        KeyAction::Pause,
        "pause",
        "pause / resume the simulation clock",
    ),
    (KeyAction::WarpDown, "warp_down", "halve the warp rate"),
    (KeyAction::WarpUp, "warp_up", "double the warp rate"),
    (KeyAction::Live, "live", "return to LIVE"),
    (
        KeyAction::Ack,
        "ack",
        "acknowledge the master caution/warning",
    ),
    (
        KeyAction::CommandLine,
        "command_line",
        "open the command line",
    ),
    (KeyAction::Help, "help", "open or close this help"),
    (KeyAction::Quit, "quit", "quit"),
    (KeyAction::Theme, "theme", "next theme"),
    (KeyAction::Vehicle, "vehicle", "next vehicle"),
    (KeyAction::Frame, "frame", "state vector frame GCRF / ITRF"),
    (
        KeyAction::Altitude,
        "altitude",
        "orbit altitude exaggeration x1/2/5/10",
    ),
    (
        KeyAction::CameraFrame,
        "camera_frame",
        "camera inertial / Earth-fixed",
    ),
    (KeyAction::Focus, "focus", "camera focus Earth / vehicle"),
    (KeyAction::ZoomIn, "zoom_in", "zoom in"),
    (KeyAction::ZoomOut, "zoom_out", "zoom out"),
    (KeyAction::Scanlines, "scanlines", "toggle scanlines"),
    (KeyAction::Bloom, "bloom", "toggle bloom"),
    (KeyAction::Persistence, "persistence", "toggle persistence"),
    (KeyAction::Crt, "crt", "toggle CRT curvature"),
    (KeyAction::AllEffects, "all_effects", "toggle every effect"),
    (
        KeyAction::TrendWindow,
        "trend_window",
        "cycle the trend window (trends page)",
    ),
];

#[derive(Debug, Clone, PartialEq)]
pub struct KeyMap {
    by_char: HashMap<char, KeyAction>,
}

impl KeyMap {
    /// Build from the `[keys]` table: every action must be bound, every binding one character, and no
    /// character may be bound twice. Unknown action names are errors, not ignored.
    pub fn from_config(keys: &BTreeMap<String, Vec<String>>) -> Result<Self, String> {
        for name in keys.keys() {
            if !ACTIONS.iter().any(|(_, n, _)| n == name) {
                return Err(format!(
                    "[keys]: unknown action {name:?}; known: {}",
                    ACTIONS.map(|(_, n, _)| n).join(", ")
                ));
            }
        }
        let mut by_char = HashMap::new();
        for (action, name, _) in ACTIONS {
            let bound = keys
                .get(name)
                .ok_or_else(|| format!("[keys]: action {name:?} has no binding"))?;
            if bound.is_empty() {
                return Err(format!("[keys]: action {name:?} has an empty binding list"));
            }
            for k in bound {
                let mut cs = k.chars();
                let (Some(c), None) = (cs.next(), cs.next()) else {
                    return Err(format!(
                        "[keys]: binding {k:?} for {name:?} must be exactly one character"
                    ));
                };
                if c.is_control() || c.is_whitespace() && c != ' ' {
                    return Err(format!(
                        "[keys]: binding {k:?} for {name:?} is not a usable key"
                    ));
                }
                let c = c.to_ascii_lowercase();
                if let Some(other) = by_char.insert(c, action)
                    && other != action
                {
                    return Err(format!(
                        "[keys]: {c:?} is bound to two actions ({name} and another)"
                    ));
                }
            }
        }
        Ok(Self { by_char })
    }

    /// The defaults from the embedded configuration.
    pub fn embedded_default() -> Self {
        let cfg = mctl_core::config::Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None)
            .expect("the embedded configuration is valid (checked by tests)");
        Self::from_config(&cfg.keys).expect("the embedded [keys] table is valid (checked by tests)")
    }

    pub fn action(&self, c: char) -> Option<KeyAction> {
        self.by_char.get(&c.to_ascii_lowercase()).copied()
    }

    /// Characters bound to `action`, sorted, for help text.
    pub fn keys_for(&self, action: KeyAction) -> Vec<char> {
        let mut v: Vec<char> = self
            .by_char
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| *c)
            .collect();
        v.sort_unstable();
        v
    }

    /// `T`, `+ =`, `SPACE`, ... for display.
    pub fn label(&self, action: KeyAction) -> String {
        self.keys_for(action)
            .iter()
            .map(|c| {
                if *c == ' ' {
                    "SPACE".to_owned()
                } else {
                    c.to_ascii_uppercase().to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> BTreeMap<String, Vec<String>> {
        mctl_core::config::Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None)
            .unwrap()
            .keys
    }

    #[test]
    fn the_embedded_defaults_bind_every_action_to_the_documented_keys() {
        let m = KeyMap::embedded_default();
        for (c, a) in [
            (' ', KeyAction::Pause),
            ('[', KeyAction::WarpDown),
            (']', KeyAction::WarpUp),
            ('0', KeyAction::Live),
            ('a', KeyAction::Ack),
            (':', KeyAction::CommandLine),
            ('?', KeyAction::Help),
            ('q', KeyAction::Quit),
            ('t', KeyAction::Theme),
            ('v', KeyAction::Vehicle),
            ('x', KeyAction::Frame),
            ('z', KeyAction::Altitude),
            ('f', KeyAction::CameraFrame),
            ('c', KeyAction::Focus),
            ('+', KeyAction::ZoomIn),
            ('-', KeyAction::ZoomOut),
            ('e', KeyAction::AllEffects),
            ('w', KeyAction::TrendWindow),
        ] {
            assert_eq!(m.action(c), Some(a), "{c:?}");
        }
        assert_eq!(m.action('T'), Some(KeyAction::Theme), "case-insensitive");
        assert_eq!(
            m.action('='),
            Some(KeyAction::ZoomIn),
            "several keys can share an action"
        );
    }

    #[test]
    fn a_user_can_rebind_an_action_and_the_old_key_is_free() {
        let mut k = defaults();
        k.insert("theme".into(), vec!["h".into()]);
        let m = KeyMap::from_config(&k).unwrap();
        assert_eq!(m.action('h'), Some(KeyAction::Theme));
        assert_eq!(m.action('t'), None, "t no longer does anything");
        assert_eq!(m.label(KeyAction::Theme), "H");
    }

    #[test]
    fn bad_keymaps_fail_loudly_with_the_reason() {
        let mut k = defaults();
        k.insert("themes".into(), vec!["h".into()]);
        assert!(
            KeyMap::from_config(&k)
                .unwrap_err()
                .contains("unknown action")
        );
        let mut k = defaults();
        k.insert("theme".into(), vec!["ab".into()]);
        assert!(
            KeyMap::from_config(&k)
                .unwrap_err()
                .contains("exactly one character")
        );
        let mut k = defaults();
        k.insert("theme".into(), vec!["v".into()]);
        assert!(
            KeyMap::from_config(&k)
                .unwrap_err()
                .contains("bound to two actions")
        );
        let mut k = defaults();
        k.remove("quit");
        assert!(KeyMap::from_config(&k).unwrap_err().contains("no binding"));
        let mut k = defaults();
        k.insert("quit".into(), vec![]);
        assert!(KeyMap::from_config(&k).unwrap_err().contains("empty"));
    }
}
