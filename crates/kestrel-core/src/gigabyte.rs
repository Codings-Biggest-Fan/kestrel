//! GIGABYTE AORUS/AERO laptops: the `GB_WMIACPI_Get`/`GB_WMIACPI_Set` WMI methods
//! installed by the BIOS. See docs/HARDWARE.md for the method list and sources.
//!
//! Every write first reads the setting's current value through its getter and
//! journals it, then reads it back and restores the original on mismatch.

use crate::ec::{EcError, Result};
use crate::ipc::Capabilities;
use crate::journal::Journal;
use crate::laptop::{Laptop, Readings};
use crate::scenario::Scenario;
use std::collections::BTreeMap;

/// Fan duty is 0-229 in GIGABYTE's interface.
pub const FAN_MAX: u32 = 229;
pub const CURVE_POINTS: u8 = 15;
pub const CHARGE_MIN: u8 = 60;
const POLICY_FULL: u32 = 0;
const POLICY_CUSTOM_STOP: u32 = 4;

/// Raw access to the GIGABYTE WMI methods.
pub trait GbIo: Send {
    /// Call a `GB_WMIACPI_Get` method without arguments; returns its `Data`.
    fn get(&mut self, method: &str) -> Result<u32>;
    /// Call a `GB_WMIACPI_Set` method with `Data = value`.
    fn set(&mut self, method: &str, value: u8) -> Result<()>;
    /// `GetFanIndexValue(Index)`: (temperature °C, duty 0-229).
    fn fan_point(&mut self, index: u8) -> Result<(u8, u8)>;
    /// `SetFanIndexValue(Index, Temperture, Value)`.
    fn set_fan_point(&mut self, index: u8, temp: u8, duty: u8) -> Result<()>;
}

/// Setter -> the getter that reads the same setting back.
const SETTERS: &[(&str, &str)] = &[
    ("SetAutoFanStatus", "GetAutoFanStatus"),
    ("SetFixedFanStatus", "GetFanFixedStatus"),
    ("SetStepFanStatus", "GetStepFanStatus"),
    ("SetNvThermalTarget", "GetNvThermalTarget"),
    ("SetChargePolicy", "GetChargePolicy"),
    ("SetChargeStop", "GetChargeStop"),
];

fn getter(setter: &str) -> Option<&'static str> {
    SETTERS.iter().find(|(s, _)| *s == setter).map(|(_, g)| *g)
}

/// Fan mode flags in the order they are written: (auto/gaming, fixed, step/custom, NV thermal target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    Quiet,
    Normal,
    Gaming,
    Custom,
    Fixed,
}

impl FanMode {
    fn flags(self) -> [(&'static str, u8); 4] {
        let (auto, fixed, step, nv) = match self {
            FanMode::Quiet => (0, 0, 0, 1),
            FanMode::Normal => (0, 0, 0, 0),
            FanMode::Gaming => (1, 0, 0, 0),
            FanMode::Custom => (0, 0, 1, 0),
            FanMode::Fixed => (0, 1, 1, 0),
        };
        // Clear the modes being left before setting the new one, as the firmware expects.
        [
            ("SetAutoFanStatus", auto),
            ("SetFixedFanStatus", fixed),
            ("SetStepFanStatus", step),
            ("SetNvThermalTarget", nv),
        ]
    }
}

fn pct_to_duty(pct: u8) -> u8 {
    ((pct.min(100) as u32 * FAN_MAX + 50) / 100) as u8
}

fn duty_to_pct(duty: u32) -> u8 {
    ((duty.min(FAN_MAX) * 100 + FAN_MAX / 2) / FAN_MAX) as u8
}

pub struct Gigabyte {
    io: Box<dyn GbIo>,
    pub journal: Journal,
    read_only: bool,
    has_nv_target: bool,
}

impl Gigabyte {
    pub fn new(mut io: Box<dyn GbIo>, journal: Journal, read_only: bool) -> Self {
        let has_nv_target = io.get("GetNvThermalTarget").is_ok();
        Self {
            io,
            journal,
            read_only,
            has_nv_target,
        }
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    fn write(&mut self, setter: &'static str, value: u8) -> Result<()> {
        if self.read_only {
            return Err(EcError::ReadOnly);
        }
        let get = getter(setter).ok_or(EcError::Unsupported("unverifiable WMI setter"))?;
        let current = self.io.get(get)?;
        self.journal.record_named(get, current);
        if current == value as u32 {
            return Ok(());
        }
        self.io.set(setter, value)?;
        let back = self.io.get(get)?;
        if back != value as u32 {
            let _ = self.io.set(setter, current as u8);
            return Err(EcError::Io(format!(
                "{get} read back {back} after {setter}({value}); restored {current}"
            )));
        }
        Ok(())
    }

    pub fn set_fan_mode(&mut self, mode: FanMode) -> Result<()> {
        if self.read_only {
            return Err(EcError::ReadOnly);
        }
        let flags = mode.flags();
        // Clear flags first so the firmware never sees two modes at once.
        for (setter, v) in flags
            .iter()
            .filter(|(_, v)| *v == 0)
            .chain(flags.iter().filter(|(_, v)| *v != 0))
        {
            if *setter == "SetNvThermalTarget" && !self.has_nv_target {
                continue;
            }
            self.write(setter, *v)?;
        }
        Ok(())
    }

    pub fn fan_mode(&mut self) -> Result<FanMode> {
        let on = |io: &mut Box<dyn GbIo>, m| io.get(m).map(|v| v != 0);
        if on(&mut self.io, "GetFanFixedStatus")? {
            Ok(FanMode::Fixed)
        } else if on(&mut self.io, "GetStepFanStatus")? {
            Ok(FanMode::Custom)
        } else if on(&mut self.io, "GetAutoFanStatus")? {
            Ok(FanMode::Gaming)
        } else if self.has_nv_target && on(&mut self.io, "GetNvThermalTarget")? {
            Ok(FanMode::Quiet)
        } else {
            Ok(FanMode::Normal)
        }
    }

    fn curve(&mut self) -> Result<Vec<(u8, u8)>> {
        (0..CURVE_POINTS).map(|i| self.io.fan_point(i)).collect()
    }
}

impl Laptop for Gigabyte {
    fn caps(&self) -> Capabilities {
        Capabilities {
            cooler_boost: true,
            charge_limit: true,
            fan_curve: true,
            ..Default::default()
        }
    }

    fn apply_scenario(&mut self, s: Scenario) -> Result<()> {
        self.set_fan_mode(match s {
            Scenario::Extreme => FanMode::Gaming,
            Scenario::Balanced => FanMode::Normal,
            Scenario::Silent | Scenario::SuperBattery => FanMode::Quiet,
        })
    }

    fn readings(&mut self) -> Readings {
        let swap = |v: u32| (v as u16).swap_bytes() as u32;
        let temp = |v: u32| u8::try_from(v).ok().filter(|&t| t > 0 && t < 120);
        let pct = self.io.get("GetFanPWMStatus").ok().map(duty_to_pct);
        Readings {
            cpu_temp: self.io.get("getCpuTemp").ok().and_then(temp),
            gpu_temp: self.io.get("getGpuTemp1").ok().and_then(temp),
            cpu_fan_pct: pct,
            gpu_fan_pct: pct,
            cpu_fan_rpm: self.io.get("getRpm1").ok().map(swap),
            gpu_fan_rpm: self.io.get("getRpm2").ok().map(swap),
        }
    }

    /// Cooler Boost here means GIGABYTE's fixed fan mode at full duty.
    fn cooler_boost(&mut self) -> Result<bool> {
        Ok(self.fan_mode()? == FanMode::Fixed)
    }

    fn set_cooler_boost(&mut self, on: bool) -> Result<()> {
        if !on {
            return self.set_fan_mode(FanMode::Normal);
        }
        self.set_fan_mode(FanMode::Fixed)?;
        // No getter exists for the fixed speed; the fixed-mode flag above is journaled.
        self.io.set("SetFixedFanSpeed", FAN_MAX as u8)?;
        let _ = self.io.set("SetGPUFanDuty", FAN_MAX as u8);
        Ok(())
    }

    fn charge_limit(&mut self) -> Result<Option<u8>> {
        if self.io.get("GetChargePolicy")? != POLICY_CUSTOM_STOP {
            return Ok(Some(100));
        }
        Ok(u8::try_from(self.io.get("GetChargeStop")?).ok())
    }

    fn set_charge_limit(&mut self, pct: u8) -> Result<()> {
        if !(CHARGE_MIN..=100).contains(&pct) {
            return Err(EcError::InvalidValue(format!(
                "charge limit {pct}% (GIGABYTE allows {CHARGE_MIN}-100)"
            )));
        }
        if pct == 100 {
            return self.write("SetChargePolicy", POLICY_FULL as u8);
        }
        self.write("SetChargeStop", pct)?;
        self.write("SetChargePolicy", POLICY_CUSTOM_STOP as u8)
    }

    /// One curve drives both fans, so CPU and GPU return the same table.
    fn fan_curve(&mut self, _gpu: bool) -> Result<(Vec<u8>, Vec<u8>)> {
        let pts = self.curve()?;
        Ok((
            pts.iter().map(|p| p.0).collect(),
            pts.iter().map(|p| duty_to_pct(p.1.into())).collect(),
        ))
    }

    fn set_fan_curve(&mut self, _gpu: bool, speeds: &[u8]) -> Result<Option<Scenario>> {
        if self.read_only {
            return Err(EcError::ReadOnly);
        }
        if speeds.len() != CURVE_POINTS as usize
            || speeds.iter().any(|&v| v > 100)
            || speeds.windows(2).any(|w| w[1] < w[0])
        {
            return Err(EcError::InvalidValue(format!("fan curve {speeds:?}")));
        }
        let current = self.curve()?;
        if current
            .iter()
            .any(|&(t, d)| t == 0 || t > 110 || d as u32 > FAN_MAX)
            || current.windows(2).any(|w| w[1].0 < w[0].0)
        {
            return Err(EcError::InvalidValue(format!(
                "fan curve table holds {current:?}; not writing"
            )));
        }
        for (i, &(t, d)) in current.iter().enumerate() {
            self.journal
                .record_named(&format!("fan_point_{i}"), (t as u32) << 8 | d as u32);
        }
        for (i, &pct) in speeds.iter().enumerate() {
            let (t, _) = current[i];
            let duty = pct_to_duty(pct);
            let ok = self
                .io
                .set_fan_point(i as u8, t, duty)
                .and_then(|_| self.io.fan_point(i as u8));
            if ok.as_ref().map_or(true, |&p| p != (t, duty)) {
                let failed: Vec<usize> = (0..=i)
                    .filter(|&j| {
                        self.io
                            .set_fan_point(j as u8, current[j].0, current[j].1)
                            .is_err()
                    })
                    .collect();
                let why = match ok {
                    Err(e) => e.to_string(),
                    Ok(p) => format!("point {i} read back {p:?}"),
                };
                return Err(EcError::Io(if failed.is_empty() {
                    format!("{why}; fan curve rolled back")
                } else {
                    format!("{why}; rollback failed at points {failed:?}")
                }));
            }
        }
        self.set_fan_mode(FanMode::Custom)?;
        Ok(None)
    }

    fn restore(&mut self) -> Result<usize> {
        if self.read_only {
            return Err(EcError::ReadOnly);
        }
        let entries: Vec<(String, u32)> = self
            .journal
            .named_entries()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        // Fan points first, then the settings, then the mode flags last so the
        // firmware switches back to its own mode only once everything else is back.
        let mut points = BTreeMap::new();
        for (k, v) in &entries {
            if let Some(i) = k
                .strip_prefix("fan_point_")
                .and_then(|i| i.parse::<u8>().ok())
            {
                points.insert(i, *v);
            }
        }
        for (i, v) in points {
            self.io.set_fan_point(i, (v >> 8) as u8, v as u8)?;
        }
        for (setter, get) in SETTERS.iter().rev() {
            if let Some((_, v)) = entries.iter().find(|(k, _)| k == get) {
                self.io.set(setter, *v as u8)?;
            }
        }
        self.journal.clear();
        Ok(entries.len())
    }

    fn debug_line(&mut self) -> String {
        let mut line = SETTERS
            .iter()
            .filter_map(|(_, g)| self.io.get(g).ok().map(|v| format!("{g}={v}")))
            .collect::<Vec<_>>()
            .join(" ");
        if let Ok(d) = self.io.get("GetFanPWMStatus") {
            line += &format!(" pwm={d}/{FAN_MAX}");
        }
        line
    }
}

/// In-memory GIGABYTE interface for tests and `--mock gigabyte`.
#[derive(Clone)]
pub struct MockGb {
    pub values: BTreeMap<String, u32>,
    pub points: Vec<(u8, u8)>,
    pub sets: Vec<(String, u8)>,
}

impl Default for MockGb {
    fn default() -> Self {
        let mut values = BTreeMap::new();
        for (k, v) in [
            ("GetAutoFanStatus", 0),
            ("GetFanFixedStatus", 0),
            ("GetStepFanStatus", 0),
            ("GetNvThermalTarget", 0),
            ("GetChargePolicy", POLICY_FULL),
            ("GetChargeStop", 100),
            ("GetFanPWMStatus", 92),
            ("getCpuTemp", 61),
            ("getGpuTemp1", 54),
            ("getRpm1", 0x6c0a), // byte-swapped 2668 rpm
            ("getRpm2", 0x4c09), // 2380 rpm
        ] {
            values.insert(k.to_string(), v);
        }
        let points = (0..CURVE_POINTS)
            .map(|i| (40 + i * 4, (i as u32 * FAN_MAX / 14) as u8))
            .collect();
        Self {
            values,
            points,
            sets: Vec::new(),
        }
    }
}

impl GbIo for MockGb {
    fn get(&mut self, method: &str) -> Result<u32> {
        self.values
            .get(method)
            .copied()
            .ok_or_else(|| EcError::Io(format!("{method}: not found")))
    }
    fn set(&mut self, method: &str, value: u8) -> Result<()> {
        self.sets.push((method.to_string(), value));
        if let Some(g) = getter(method) {
            self.values.insert(g.to_string(), value.into());
        }
        Ok(())
    }
    fn fan_point(&mut self, index: u8) -> Result<(u8, u8)> {
        self.points
            .get(index as usize)
            .copied()
            .ok_or_else(|| EcError::InvalidValue(format!("fan point {index}")))
    }
    fn set_fan_point(&mut self, index: u8, temp: u8, duty: u8) -> Result<()> {
        let p = self
            .points
            .get_mut(index as usize)
            .ok_or_else(|| EcError::InvalidValue(format!("fan point {index}")))?;
        *p = (temp, duty);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gb(read_only: bool) -> Gigabyte {
        Gigabyte::new(Box::new(MockGb::default()), Journal::in_memory(), read_only)
    }

    #[test]
    fn readings_swap_rpm_bytes() {
        let r = gb(true).readings();
        assert_eq!((r.cpu_fan_rpm, r.gpu_fan_rpm), (Some(2668), Some(2380)));
        assert_eq!(
            (r.cpu_temp, r.gpu_temp, r.cpu_fan_pct),
            (Some(61), Some(54), Some(40))
        );
    }

    #[test]
    fn read_only_blocks_every_write() {
        let mut g = gb(true);
        assert!(matches!(
            g.apply_scenario(Scenario::Extreme),
            Err(EcError::ReadOnly)
        ));
        assert!(matches!(g.set_charge_limit(80), Err(EcError::ReadOnly)));
        assert!(matches!(
            g.set_fan_curve(false, &[50; 15]),
            Err(EcError::ReadOnly)
        ));
        assert!(g.journal.is_empty());
    }

    #[test]
    fn scenarios_switch_fan_modes_and_restore() {
        let mut g = gb(false);
        g.apply_scenario(Scenario::Extreme).unwrap();
        assert_eq!(g.fan_mode().unwrap(), FanMode::Gaming);
        g.apply_scenario(Scenario::Silent).unwrap();
        assert_eq!(g.fan_mode().unwrap(), FanMode::Quiet);
        g.restore().unwrap();
        assert_eq!(g.fan_mode().unwrap(), FanMode::Normal);
    }

    #[test]
    fn charge_limit_uses_custom_policy() {
        let mut g = gb(false);
        assert_eq!(g.charge_limit().unwrap(), Some(100));
        assert!(g.set_charge_limit(40).is_err());
        g.set_charge_limit(80).unwrap();
        assert_eq!(g.charge_limit().unwrap(), Some(80));
        g.set_charge_limit(100).unwrap();
        assert_eq!(g.charge_limit().unwrap(), Some(100));
    }

    #[test]
    fn cooler_boost_is_fixed_full_speed() {
        let mut g = gb(false);
        g.set_cooler_boost(true).unwrap();
        assert!(g.cooler_boost().unwrap());
        g.set_cooler_boost(false).unwrap();
        assert!(!g.cooler_boost().unwrap());
    }

    #[test]
    fn fan_curve_round_trip_validated_and_restored() {
        let mut g = gb(false);
        let (temps, before) = g.fan_curve(false).unwrap();
        assert_eq!(temps.len(), 15);
        assert!(g.set_fan_curve(false, &[50; 14]).is_err());
        let mut falling = [50u8; 15];
        falling[3] = 10;
        assert!(g.set_fan_curve(false, &falling).is_err());
        let speeds: Vec<u8> = (0..15).map(|i| 20 + i * 5).collect();
        g.set_fan_curve(false, &speeds).unwrap();
        assert_eq!(g.fan_curve(true).unwrap(), (temps.clone(), speeds));
        assert_eq!(g.fan_mode().unwrap(), FanMode::Custom);
        g.restore().unwrap();
        assert_eq!(g.fan_curve(false).unwrap(), (temps, before));
        assert_eq!(g.fan_mode().unwrap(), FanMode::Normal);
    }

    struct Ignores(MockGb, &'static str);
    impl GbIo for Ignores {
        fn get(&mut self, m: &str) -> Result<u32> {
            self.0.get(m)
        }
        fn set(&mut self, m: &str, v: u8) -> Result<()> {
            if m == self.1 {
                Ok(())
            } else {
                self.0.set(m, v)
            }
        }
        fn fan_point(&mut self, i: u8) -> Result<(u8, u8)> {
            self.0.fan_point(i)
        }
        fn set_fan_point(&mut self, i: u8, t: u8, d: u8) -> Result<()> {
            if i == 5 {
                Ok(())
            } else {
                self.0.set_fan_point(i, t, d)
            }
        }
    }

    #[test]
    fn write_that_does_not_stick_is_an_error() {
        let mut g = Gigabyte::new(
            Box::new(Ignores(MockGb::default(), "SetChargeStop")),
            Journal::in_memory(),
            false,
        );
        assert!(g.set_charge_limit(80).is_err());
        assert_eq!(g.charge_limit().unwrap(), Some(100));
    }

    #[test]
    fn failed_curve_point_rolls_back_whole_table() {
        let mut g = Gigabyte::new(
            Box::new(Ignores(MockGb::default(), "")),
            Journal::in_memory(),
            false,
        );
        let before = g.fan_curve(false).unwrap();
        assert!(g.set_fan_curve(false, &[100; 15]).is_err());
        assert_eq!(g.fan_curve(false).unwrap(), before);
    }

    #[test]
    fn duty_conversion() {
        assert_eq!(pct_to_duty(100), 229);
        assert_eq!(duty_to_pct(229), 100);
        assert_eq!(duty_to_pct(pct_to_duty(37).into()), 37);
    }
}
