//! Laptop model database: EC register maps keyed by EC firmware ID.

use serde::Deserialize;
use std::collections::BTreeMap;

include!(concat!(env!("OUT_DIR"), "/models.rs"));

/// EC address where the firmware ID string (e.g. `17S1IMS1.114`) is stored.
pub const FW_VERSION_ADDRESS: u8 = 0xa0;
pub const FW_VERSION_LENGTH: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WmiGen {
    Wmi1,
    Wmi2,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Firmware {
    pub prefix: String,
    #[serde(default)]
    pub models: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct BitField {
    pub address: Option<u8>,
    pub bit: u8,
    #[serde(default)]
    pub invert: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Webcam {
    pub address: Option<u8>,
    pub block_address: Option<u8>,
    pub bit: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModeField {
    pub address: Option<u8>,
    #[serde(default)]
    pub modes: BTreeMap<String, u8>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct MaskField {
    pub address: Option<u8>,
    pub mask: u8,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Sensor {
    pub rt_temp_address: Option<u8>,
    pub rt_fan_speed_address: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KbdBacklight {
    pub bl_state_address: Option<u8>,
    #[serde(default = "default_kbd_base")]
    pub state_base_value: u8,
    #[serde(default = "default_kbd_max")]
    pub max_state: u8,
}

fn default_kbd_base() -> u8 {
    0x80
}
fn default_kbd_max() -> u8 {
    3
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Confirmed on owner hardware.
    Tested,
    /// Taken from a family map; read-only until the user opts in.
    #[default]
    Experimental,
}

/// Fan tachometers: 16-bit big-endian divisor at `addr:addr+1` when `wide`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct FanRpm {
    pub cpu_address: Option<u8>,
    pub gpu_address: Option<u8>,
    #[serde(default)]
    pub wide: bool,
    #[serde(default = "default_rpm_const")]
    pub rpm_const: u32,
}

fn default_rpm_const() -> u32 {
    480_000
}

/// EC fan-curve tables, followed when fan mode is `advanced`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct FanCurveMap {
    pub cpu_temp_base: u8,
    pub cpu_speed_base: u8,
    pub gpu_temp_base: u8,
    pub gpu_speed_base: u8,
    pub points: u8,
    /// The EC only follows the curve while the shift mode is turbo (Extreme).
    #[serde(default)]
    pub requires_extreme: bool,
}

/// One EC register map, shared by every firmware listed in `firmware`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelConf {
    pub id: String,
    pub wmi: WmiGen,
    #[serde(default)]
    pub tier: Tier,
    pub firmware: Vec<Firmware>,
    pub charge_control_address: Option<u8>,
    pub webcam: Option<Webcam>,
    pub fn_win_swap: Option<BitField>,
    pub cooler_boost: Option<BitField>,
    pub shift_mode: Option<ModeField>,
    pub super_battery: Option<MaskField>,
    pub fan_mode: Option<ModeField>,
    pub cpu: Option<Sensor>,
    pub gpu: Option<Sensor>,
    pub kbd_bl: Option<KbdBacklight>,
    pub fan_rpm: Option<FanRpm>,
    pub fan_curve: Option<FanCurveMap>,
}

#[derive(Debug, Clone)]
pub struct Matched<'a> {
    pub conf: &'a ModelConf,
    pub firmware: &'a Firmware,
}

pub struct ModelDb {
    confs: Vec<ModelConf>,
}

impl ModelDb {
    pub fn embedded() -> Self {
        Self::from_sources(EMBEDDED_MODELS.iter().copied()).expect("embedded model database is valid")
    }

    pub fn from_sources<'a>(sources: impl IntoIterator<Item = &'a str>) -> Result<Self, toml::de::Error> {
        let confs = sources.into_iter().map(toml::from_str).collect::<Result<_, _>>()?;
        Ok(Self { confs })
    }

    pub fn confs(&self) -> &[ModelConf] {
        &self.confs
    }

    /// Match a firmware ID read from the EC. The trailing update digits are
    /// ignored, so `17S1IMS1.114` matches the `17S1IMS1.1` entry.
    pub fn find(&self, fw_id: &str) -> Option<Matched<'_>> {
        let fw_id = fw_id.trim_end_matches(['\0', ' ']);
        let truncated = truncate_fw(fw_id);
        self.confs.iter().find_map(|conf| {
            conf.firmware
                .iter()
                .find(|f| f.prefix == truncated)
                .map(|firmware| Matched { conf, firmware })
        })
    }
}

/// `15M3EMS1.113` -> `15M3EMS1.1` (msi-ec drops the last two characters).
pub fn truncate_fw(fw_id: &str) -> &str {
    let n = FW_VERSION_LENGTH - 2;
    if fw_id.len() > n && fw_id.is_char_boundary(n) {
        &fw_id[..n]
    } else {
        fw_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_db_loads_every_map() {
        let db = ModelDb::embedded();
        assert!(db.confs().len() >= 21);
        assert!(db.confs().iter().map(|c| c.firmware.len()).sum::<usize>() >= 180);
    }

    #[test]
    fn matches_raider_ge78() {
        let db = ModelDb::embedded();
        let m = db.find("17S1IMS1.114").unwrap();
        assert_eq!(m.conf.id, "G2_10");
        assert_eq!(m.conf.wmi, WmiGen::Wmi2);
        assert_eq!(m.conf.cooler_boost.unwrap().address, Some(0x98));
        assert_eq!(m.conf.shift_mode.as_ref().unwrap().modes["eco"], 0xc2);
    }

    #[test]
    fn vector_a16_hx_a8w_uses_verified_map() {
        let db = ModelDb::embedded();
        let m = db.find("15MMIMS1.103").unwrap();
        assert_eq!(m.conf.id, "VECTOR_A16_HX_A8W");
        assert_eq!(m.conf.tier, Tier::Tested);
        assert!(m.conf.super_battery.is_none());
        assert_eq!(m.conf.fan_curve.unwrap().cpu_speed_base, 0x72);
        assert!(m.conf.fan_curve.unwrap().requires_extreme);
        use crate::scenario::Scenario;
        assert_eq!(Scenario::available(m.conf), vec![Scenario::Extreme, Scenario::Balanced, Scenario::Silent]);
        assert_eq!(m.conf.fan_rpm.unwrap().cpu_address, Some(0xc8));
    }

    #[test]
    fn generated_maps_are_experimental() {
        assert_eq!(ModelDb::embedded().find("17S1IMS1.114").unwrap().conf.tier, Tier::Experimental);
    }

    #[test]
    fn unknown_firmware_is_none() {
        assert!(ModelDb::embedded().find("ZZZZZMS1.100").is_none());
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_fw("15M3EMS1.113"), "15M3EMS1.1");
        assert_eq!(truncate_fw("15M3EMS1.1"), "15M3EMS1.1");
    }
}
