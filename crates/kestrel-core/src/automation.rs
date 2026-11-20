//! Per-user power automation: what to change when the laptop switches between battery and
//! AC. Written by the UI, applied by the tray (display modes belong to the user's session).

use crate::scenario::Scenario;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    #[serde(default)]
    pub battery_scenario: Option<Scenario>,
    #[serde(default)]
    pub ac_scenario: Option<Scenario>,
    /// Drop the built-in display to 60 Hz on battery and back to its highest rate on AC.
    #[serde(default)]
    pub battery_60hz: bool,
    /// Auto engine: while one of these programs runs, the first matching rule's scenario is active.
    #[serde(default)]
    pub app_rules: Vec<AppRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppRule {
    /// Executable file name, e.g. `cyberpunk2077.exe` (case-insensitive).
    pub exe: String,
    pub scenario: Scenario,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Refresh {
    Max,
    Hz60,
}

#[derive(Debug, PartialEq)]
pub struct Actions {
    pub scenario: Option<Scenario>,
    pub refresh: Option<Refresh>,
}

pub fn user_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("KESTREL_USER_DIR") {
        return d.into();
    }
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("APPDATA") {
        return PathBuf::from(d).join("Kestrel");
    }
    std::env::temp_dir().join("kestrel-user")
}

impl Automation {
    pub fn load() -> Self {
        std::fs::read_to_string(user_dir().join("automation.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = user_dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join("automation.json"), json)
    }

    /// The scenario of the first rule whose program is in `running` (lower-case exe names).
    pub fn app_scenario(&self, running: &[String]) -> Option<Scenario> {
        self.app_rules
            .iter()
            .find(|r| running.iter().any(|p| p.eq_ignore_ascii_case(&r.exe)))
            .map(|r| r.scenario)
    }

    pub fn on_power_change(&self, on_ac: bool) -> Actions {
        Actions {
            scenario: if on_ac {
                self.ac_scenario
            } else {
                self.battery_scenario
            },
            refresh: self
                .battery_60hz
                .then_some(if on_ac { Refresh::Max } else { Refresh::Hz60 }),
        }
    }
}

/// Picks a refresh rate among `rates` (Hz) available at the current resolution:
/// the highest for `Max`, or the lowest rate of at least 59 Hz (59.94 panels report 59) for `Hz60`.
pub fn pick_refresh(rates: &[u32], want: Refresh) -> Option<u32> {
    match want {
        Refresh::Max => rates.iter().copied().max(),
        Refresh::Hz60 => rates.iter().copied().filter(|&r| r >= 59).min(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_follow_power_source() {
        let a = Automation {
            battery_scenario: Some(Scenario::SuperBattery),
            battery_60hz: true,
            ..Default::default()
        };
        assert_eq!(
            a.on_power_change(false),
            Actions {
                scenario: Some(Scenario::SuperBattery),
                refresh: Some(Refresh::Hz60)
            }
        );
        assert_eq!(
            a.on_power_change(true),
            Actions {
                scenario: None,
                refresh: Some(Refresh::Max)
            }
        );
        assert_eq!(
            Automation::default().on_power_change(false),
            Actions {
                scenario: None,
                refresh: None
            }
        );
    }

    #[test]
    fn first_running_rule_wins() {
        let a = Automation {
            app_rules: vec![
                AppRule {
                    exe: "Game.exe".into(),
                    scenario: Scenario::Extreme,
                },
                AppRule {
                    exe: "obs64.exe".into(),
                    scenario: Scenario::Balanced,
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            a.app_scenario(&["obs64.exe".into(), "game.exe".into()]),
            Some(Scenario::Extreme)
        );
        assert_eq!(
            a.app_scenario(&["obs64.exe".into()]),
            Some(Scenario::Balanced)
        );
        assert_eq!(a.app_scenario(&["explorer.exe".into()]), None);
    }

    #[test]
    fn refresh_choice() {
        assert_eq!(pick_refresh(&[60, 165, 240, 48], Refresh::Max), Some(240));
        assert_eq!(pick_refresh(&[48, 60, 240], Refresh::Hz60), Some(60));
        assert_eq!(pick_refresh(&[59, 144], Refresh::Hz60), Some(59));
        assert_eq!(pick_refresh(&[], Refresh::Max), None);
    }

    #[test]
    fn missing_fields_default() {
        let a: Automation = serde_json::from_str("{}").unwrap();
        assert_eq!(a, Automation::default());
    }
}
