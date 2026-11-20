//! One interface over every vendor backend, so the service does not care whether
//! it is talking to an MSI EC or a GIGABYTE WMI interface.

use crate::ec::{EcError, MsiEc, Result};
use crate::ipc::Capabilities;
use crate::scenario::Scenario;

/// Live readings; any field may be missing on a given laptop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Readings {
    pub cpu_temp: Option<u8>,
    pub gpu_temp: Option<u8>,
    pub cpu_fan_pct: Option<u8>,
    pub gpu_fan_pct: Option<u8>,
    pub cpu_fan_rpm: Option<u32>,
    pub gpu_fan_rpm: Option<u32>,
}

pub trait Laptop: Send {
    fn caps(&self) -> Capabilities;
    fn apply_scenario(&mut self, s: Scenario) -> Result<()>;
    fn readings(&mut self) -> Readings;

    fn cooler_boost(&mut self) -> Result<bool> {
        Err(EcError::Unsupported("Cooler Boost"))
    }
    fn set_cooler_boost(&mut self, _on: bool) -> Result<()> {
        Err(EcError::Unsupported("Cooler Boost"))
    }
    fn charge_limit(&mut self) -> Result<Option<u8>> {
        Err(EcError::Unsupported("charge limit"))
    }
    fn set_charge_limit(&mut self, _pct: u8) -> Result<()> {
        Err(EcError::Unsupported("charge limit"))
    }
    fn kbd_backlight(&mut self) -> Result<u8> {
        Err(EcError::Unsupported("keyboard backlight"))
    }
    fn set_kbd_backlight(&mut self, _level: u8) -> Result<()> {
        Err(EcError::Unsupported("keyboard backlight"))
    }
    /// (temps °C, speeds %) for the CPU (`gpu == false`) or GPU fan.
    fn fan_curve(&mut self, _gpu: bool) -> Result<(Vec<u8>, Vec<u8>)> {
        Err(EcError::Unsupported("fan curve"))
    }
    /// Write speeds (0-100 %, non-decreasing) and make the firmware follow them.
    /// Returns the scenario the laptop ended up in, if applying forced one.
    fn set_fan_curve(&mut self, _gpu: bool, _speeds: &[u8]) -> Result<Option<Scenario>> {
        Err(EcError::Unsupported("fan curve"))
    }
    /// Put every setting Kestrel changed back; returns how many were restored.
    fn restore(&mut self) -> Result<usize>;
    /// One line of raw state for the service log after each change.
    fn debug_line(&mut self) -> String {
        String::new()
    }
}

impl Laptop for MsiEc {
    fn caps(&self) -> Capabilities {
        let c = self.conf();
        Capabilities {
            cooler_boost: c.cooler_boost.and_then(|f| f.address).is_some(),
            charge_limit: c.charge_control_address.is_some(),
            kbd_backlight: c.kbd_bl.as_ref().and_then(|k| k.bl_state_address).is_some(),
            super_battery: c.super_battery.and_then(|f| f.address).is_some(),
            shift_modes: c
                .shift_mode
                .as_ref()
                .map(|m| m.modes.keys().cloned().collect())
                .unwrap_or_default(),
            fan_curve: c.fan_curve.is_some(),
        }
    }

    fn apply_scenario(&mut self, s: Scenario) -> Result<()> {
        let shift = self.conf().shift_mode.clone();
        let fan = self.conf().fan_mode.clone();
        if let Some(name) = shift.and_then(|m| {
            s.shift_modes()
                .iter()
                .find(|n| m.modes.contains_key(**n))
                .copied()
        }) {
            self.set_shift_mode(name)?;
        }
        if let Some(name) = fan.and_then(|m| {
            s.fan_modes()
                .iter()
                .find(|n| m.modes.contains_key(**n))
                .copied()
        }) {
            self.set_fan_mode(name)?;
        }
        if self.conf().super_battery.and_then(|f| f.address).is_some() {
            self.set_super_battery(s.super_battery())?;
        }
        Ok(())
    }

    fn readings(&mut self) -> Readings {
        let mut r = Readings::default();
        if let Ok([ct, cf, gt, gf]) = self.sensors() {
            (r.cpu_temp, r.cpu_fan_pct, r.gpu_temp, r.gpu_fan_pct) = (ct, cf, gt, gf);
        }
        if let Ok([c, g]) = self.fan_rpm() {
            (r.cpu_fan_rpm, r.gpu_fan_rpm) = (c, g);
        }
        r
    }

    fn cooler_boost(&mut self) -> Result<bool> {
        MsiEc::cooler_boost(self)
    }
    fn set_cooler_boost(&mut self, on: bool) -> Result<()> {
        MsiEc::set_cooler_boost(self, on)
    }
    fn charge_limit(&mut self) -> Result<Option<u8>> {
        MsiEc::charge_limit(self)
    }
    fn set_charge_limit(&mut self, pct: u8) -> Result<()> {
        MsiEc::set_charge_limit(self, pct)
    }
    fn kbd_backlight(&mut self) -> Result<u8> {
        MsiEc::kbd_backlight(self)
    }
    fn set_kbd_backlight(&mut self, level: u8) -> Result<()> {
        MsiEc::set_kbd_backlight(self, level)
    }
    fn fan_curve(&mut self, gpu: bool) -> Result<(Vec<u8>, Vec<u8>)> {
        MsiEc::fan_curve(self, gpu)
    }

    fn set_fan_curve(&mut self, gpu: bool, speeds: &[u8]) -> Result<Option<Scenario>> {
        let extreme = self.conf().fan_curve.is_some_and(|f| f.requires_extreme);
        self.set_fan_curve_speeds(gpu, speeds)?;
        if extreme {
            self.set_shift_mode("turbo")?;
        }
        if self
            .conf()
            .fan_mode
            .as_ref()
            .is_some_and(|m| m.modes.contains_key("advanced"))
        {
            self.set_fan_mode("advanced")?;
        }
        Ok(extreme.then_some(Scenario::Extreme))
    }

    fn restore(&mut self) -> Result<usize> {
        MsiEc::restore(self)
    }

    fn debug_line(&mut self) -> String {
        let c = self.conf().clone();
        let addrs = [
            c.cooler_boost.and_then(|f| f.address),
            c.shift_mode.and_then(|m| m.address),
            c.fan_mode.and_then(|m| m.address),
        ];
        let mut line = addrs
            .into_iter()
            .flatten()
            .map(|a| {
                format!(
                    "0x{a:02x}={}",
                    self.peek(a).map_or("??".into(), |v| format!("0x{v:02x}"))
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        // Bytes just before each speed table hold the duty the EC is currently applying.
        if let Some(fc) = c.fan_curve {
            let mut duty = |a: u8| self.peek(a).map_or("??".into(), |v| v.to_string());
            let (cd, gd) = (duty(fc.cpu_speed_base - 1), duty(fc.gpu_speed_base - 1));
            line += &format!(" duty cpu={cd}% gpu={gd}%");
        }
        if let Ok([cpu, gpu]) = self.fan_rpm() {
            line += &format!(" rpm cpu={} gpu={}", cpu.unwrap_or(0), gpu.unwrap_or(0));
        }
        line
    }
}
