//! User scenarios, mapped onto whatever shift/fan modes the laptop's EC offers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scenario {
    Extreme,
    Balanced,
    Silent,
    SuperBattery,
}

impl Scenario {
    pub const ALL: [Scenario; 4] = [Scenario::Extreme, Scenario::Balanced, Scenario::Silent, Scenario::SuperBattery];

    pub fn label(self) -> &'static str {
        match self {
            Scenario::Extreme => "Extreme",
            Scenario::Balanced => "Balanced",
            Scenario::Silent => "Silent",
            Scenario::SuperBattery => "Super Battery",
        }
    }

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&s| s == self).unwrap()
    }

    /// Shift modes to try, best match first (names follow msi-ec).
    pub fn shift_modes(self) -> &'static [&'static str] {
        match self {
            Scenario::Extreme => &["turbo", "sport"],
            Scenario::Balanced => &["comfort", "sport"],
            Scenario::Silent => &["comfort", "eco"],
            Scenario::SuperBattery => &["eco"],
        }
    }

    pub fn fan_modes(self) -> &'static [&'static str] {
        match self {
            Scenario::Extreme | Scenario::Balanced => &["auto"],
            Scenario::Silent | Scenario::SuperBattery => &["silent", "auto"],
        }
    }

    pub fn super_battery(self) -> bool {
        self == Scenario::SuperBattery
    }

    /// Scenarios this model's EC can actually do. Super Battery needs its own register: on
    /// boards without one (e.g. Vector A16 HX) MSI Center does not offer it either.
    pub fn available(conf: &crate::model::ModelConf) -> Vec<Scenario> {
        let Some(shift) = conf.shift_mode.as_ref() else { return Vec::new() };
        let has_sb = conf.super_battery.and_then(|f| f.address).is_some();
        Self::ALL
            .into_iter()
            .filter(|s| s.shift_modes().iter().any(|m| shift.modes.contains_key(*m)))
            .filter(|s| !s.super_battery() || has_sb)
            .collect()
    }
}
