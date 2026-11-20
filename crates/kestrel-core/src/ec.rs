//! Embedded-controller access. Every write goes through [`MsiEc`], which only
//! touches registers named in the matched model map and journals the original
//! value first so it can be restored.

use crate::journal::Journal;
use crate::model::{BitField, ModeField, ModelConf, FW_VERSION_ADDRESS, FW_VERSION_LENGTH};
use std::fmt;

#[derive(Debug)]
pub enum EcError {
    Io(String),
    Unsupported(&'static str),
    InvalidValue(String),
    ReadOnly,
}

impl fmt::Display for EcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EcError::Io(e) => write!(f, "EC I/O error: {e}"),
            EcError::Unsupported(what) => write!(f, "{what} is not supported on this laptop"),
            EcError::InvalidValue(v) => write!(f, "invalid value: {v}"),
            EcError::ReadOnly => f.write_str("this laptop is in read-only mode"),
        }
    }
}

impl std::error::Error for EcError {}

pub type Result<T> = std::result::Result<T, EcError>;

/// Raw byte access to the EC (ACPI-WMI `Get_EC`/`Set_EC`, PawnIO, or a mock).
pub trait EcIo: Send {
    fn read(&mut self, addr: u8) -> Result<u8>;
    fn write(&mut self, addr: u8, value: u8) -> Result<()>;

    fn read_seq(&mut self, addr: u8, out: &mut [u8]) -> Result<()> {
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.read(addr.wrapping_add(i as u8))?;
        }
        Ok(())
    }
}

/// In-memory EC used by tests and `--mock` runs.
#[derive(Clone)]
pub struct MockEc {
    pub regs: [u8; 256],
    pub writes: Vec<(u8, u8)>,
}

impl MockEc {
    pub fn with_firmware(fw: &str) -> Self {
        let mut regs = [0u8; 256];
        for (i, b) in fw.bytes().take(FW_VERSION_LENGTH).enumerate() {
            regs[FW_VERSION_ADDRESS as usize + i] = b;
        }
        Self { regs, writes: Vec::new() }
    }
}

impl EcIo for MockEc {
    fn read(&mut self, addr: u8) -> Result<u8> {
        Ok(self.regs[addr as usize])
    }
    fn write(&mut self, addr: u8, value: u8) -> Result<()> {
        self.regs[addr as usize] = value;
        self.writes.push((addr, value));
        Ok(())
    }
}

pub fn read_firmware_id(io: &mut dyn EcIo) -> Result<String> {
    let mut buf = [0u8; FW_VERSION_LENGTH];
    io.read_seq(FW_VERSION_ADDRESS, &mut buf)?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).trim().to_string())
}

pub struct MsiEc {
    io: Box<dyn EcIo>,
    conf: ModelConf,
    pub journal: Journal,
    read_only: bool,
}

impl MsiEc {
    pub fn new(io: Box<dyn EcIo>, conf: ModelConf, journal: Journal, read_only: bool) -> Self {
        Self { io, conf, journal, read_only }
    }

    pub fn conf(&self) -> &ModelConf {
        &self.conf
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    fn write(&mut self, addr: u8, value: u8) -> Result<()> {
        if self.read_only {
            return Err(EcError::ReadOnly);
        }
        let current = self.io.read(addr)?;
        if !self.journal.contains(addr) {
            self.journal.record(addr, current);
        }
        if current == value {
            return Ok(());
        }
        self.io.write(addr, value)?;
        let back = self.io.read(addr)?;
        if back != value {
            let _ = self.io.write(addr, current);
            return Err(EcError::Io(format!(
                "register 0x{addr:02x} read back 0x{back:02x} after writing 0x{value:02x}; restored 0x{current:02x}"
            )));
        }
        Ok(())
    }

    fn get_bit(&mut self, f: Option<BitField>, what: &'static str) -> Result<bool> {
        let f = f.ok_or(EcError::Unsupported(what))?;
        let addr = f.address.ok_or(EcError::Unsupported(what))?;
        let v = self.io.read(addr)? & (1 << f.bit) != 0;
        Ok(v != f.invert)
    }

    fn set_bit(&mut self, f: Option<BitField>, what: &'static str, on: bool) -> Result<()> {
        let f = f.ok_or(EcError::Unsupported(what))?;
        let addr = f.address.ok_or(EcError::Unsupported(what))?;
        let cur = self.io.read(addr)?;
        let set = on != f.invert;
        let new = if set { cur | (1 << f.bit) } else { cur & !(1 << f.bit) };
        self.write(addr, new)
    }

    fn get_mode(&mut self, f: Option<&ModeField>, what: &'static str) -> Result<Option<String>> {
        let f = f.ok_or(EcError::Unsupported(what))?.clone();
        let addr = f.address.ok_or(EcError::Unsupported(what))?;
        let v = self.io.read(addr)?;
        Ok(f.modes.iter().find(|(_, &m)| m == v).map(|(k, _)| k.clone()))
    }

    fn set_mode(&mut self, f: Option<&ModeField>, what: &'static str, name: &str) -> Result<()> {
        let f = f.ok_or(EcError::Unsupported(what))?.clone();
        let addr = f.address.ok_or(EcError::Unsupported(what))?;
        let v = *f.modes.get(name).ok_or_else(|| EcError::InvalidValue(format!("{what} mode {name}")))?;
        let cur = self.io.read(addr)?;
        if !f.modes.values().any(|&m| m == cur) {
            // The register holds a byte the map does not know: the map is wrong for this board.
            return Err(EcError::InvalidValue(format!(
                "{what} register 0x{addr:02x} holds unknown value 0x{cur:02x}; not writing"
            )));
        }
        self.write(addr, v)
    }

    pub fn cooler_boost(&mut self) -> Result<bool> {
        self.get_bit(self.conf.cooler_boost, "Cooler Boost")
    }
    pub fn set_cooler_boost(&mut self, on: bool) -> Result<()> {
        self.set_bit(self.conf.cooler_boost, "Cooler Boost", on)
    }

    pub fn shift_mode(&mut self) -> Result<Option<String>> {
        let f = self.conf.shift_mode.clone();
        self.get_mode(f.as_ref(), "Shift mode")
    }
    pub fn set_shift_mode(&mut self, name: &str) -> Result<()> {
        let f = self.conf.shift_mode.clone();
        self.set_mode(f.as_ref(), "Shift mode", name)
    }

    pub fn fan_mode(&mut self) -> Result<Option<String>> {
        let f = self.conf.fan_mode.clone();
        self.get_mode(f.as_ref(), "Fan mode")
    }
    pub fn set_fan_mode(&mut self, name: &str) -> Result<()> {
        let f = self.conf.fan_mode.clone();
        self.set_mode(f.as_ref(), "Fan mode", name)
    }

    pub fn super_battery(&mut self) -> Result<bool> {
        let f = self.conf.super_battery.ok_or(EcError::Unsupported("Super Battery"))?;
        let addr = f.address.ok_or(EcError::Unsupported("Super Battery"))?;
        Ok(self.io.read(addr)? & f.mask == f.mask)
    }
    pub fn set_super_battery(&mut self, on: bool) -> Result<()> {
        let f = self.conf.super_battery.ok_or(EcError::Unsupported("Super Battery"))?;
        let addr = f.address.ok_or(EcError::Unsupported("Super Battery"))?;
        let cur = self.io.read(addr)?;
        self.write(addr, if on { cur | f.mask } else { cur & !f.mask })
    }

    /// Battery charge end threshold in percent (10-100).
    pub fn charge_limit(&mut self) -> Result<Option<u8>> {
        let addr = self.conf.charge_control_address.ok_or(EcError::Unsupported("Charge limit"))?;
        let v = self.io.read(addr)? & 0x7f;
        Ok((10..=100).contains(&v).then_some(v))
    }
    pub fn set_charge_limit(&mut self, pct: u8) -> Result<()> {
        let addr = self.conf.charge_control_address.ok_or(EcError::Unsupported("Charge limit"))?;
        if !(10..=100).contains(&pct) {
            return Err(EcError::InvalidValue(format!("charge limit {pct}%")));
        }
        let cur = self.io.read(addr)? & 0x7f;
        if cur != 0 && !(10..=100).contains(&cur) {
            return Err(EcError::InvalidValue(format!("charge register holds {cur}; not writing")));
        }
        self.write(addr, pct | 0x80)
    }

    pub fn kbd_backlight(&mut self) -> Result<u8> {
        let kb = self.conf.kbd_bl.clone().ok_or(EcError::Unsupported("Keyboard backlight"))?;
        let addr = kb.bl_state_address.ok_or(EcError::Unsupported("Keyboard backlight"))?;
        Ok(self.io.read(addr)? & 0x03)
    }
    pub fn set_kbd_backlight(&mut self, level: u8) -> Result<()> {
        let kb = self.conf.kbd_bl.clone().ok_or(EcError::Unsupported("Keyboard backlight"))?;
        let addr = kb.bl_state_address.ok_or(EcError::Unsupported("Keyboard backlight"))?;
        if level > kb.max_state {
            return Err(EcError::InvalidValue(format!("backlight level {level}")));
        }
        self.write(addr, kb.state_base_value | level)
    }

    pub fn webcam(&mut self) -> Result<bool> {
        let w = self.conf.webcam.ok_or(EcError::Unsupported("Webcam switch"))?;
        let f = BitField { address: w.address, bit: w.bit, invert: false };
        self.get_bit(Some(f), "Webcam switch")
    }
    pub fn set_webcam(&mut self, on: bool) -> Result<()> {
        let w = self.conf.webcam.ok_or(EcError::Unsupported("Webcam switch"))?;
        let f = BitField { address: w.address, bit: w.bit, invert: false };
        self.set_bit(Some(f), "Webcam switch", on)
    }

    pub fn fn_win_swap(&mut self) -> Result<bool> {
        self.get_bit(self.conf.fn_win_swap, "Fn/Win swap")
    }
    pub fn set_fn_win_swap(&mut self, on: bool) -> Result<()> {
        self.set_bit(self.conf.fn_win_swap, "Fn/Win swap", on)
    }

    /// (CPU temp °C, CPU fan %, GPU temp °C, GPU fan %).
    pub fn sensors(&mut self) -> Result<[Option<u8>; 4]> {
        let mut rd = |a: Option<u8>| -> Result<Option<u8>> { a.map(|a| self.io.read(a)).transpose() };
        let cpu = self.conf.cpu;
        let gpu = self.conf.gpu;
        Ok([
            rd(cpu.and_then(|s| s.rt_temp_address))?,
            rd(cpu.and_then(|s| s.rt_fan_speed_address))?,
            rd(gpu.and_then(|s| s.rt_temp_address))?,
            rd(gpu.and_then(|s| s.rt_fan_speed_address))?,
        ])
    }

    /// (CPU fan rpm, GPU fan rpm) from the tachometer registers, if the map has them.
    pub fn fan_rpm(&mut self) -> Result<[Option<u32>; 2]> {
        let Some(t) = self.conf.fan_rpm else { return Ok([None, None]) };
        let mut rpm = |a: Option<u8>| -> Result<Option<u32>> {
            let Some(a) = a else { return Ok(None) };
            let raw = if t.wide {
                (self.io.read(a)? as u32) << 8 | self.io.read(a.wrapping_add(1))? as u32
            } else {
                self.io.read(a)? as u32
            };
            Ok(Some(t.rpm_const.checked_div(raw).unwrap_or(0)))
        };
        Ok([rpm(t.cpu_address)?, rpm(t.gpu_address)?])
    }

    /// Write every journaled register back to its original value.
    pub fn restore(&mut self) -> Result<usize> {
        let entries: Vec<_> = self.journal.entries().collect();
        for &(addr, value) in &entries {
            self.io.write(addr, value)?;
        }
        self.journal.clear();
        Ok(entries.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelDb;

    fn ec(fw: &str) -> MsiEc {
        let mut io = MockEc::with_firmware(fw);
        io.regs[0xd2] = 0xc1;
        io.regs[0xd4] = 0x0d;
        let id = read_firmware_id(&mut io).unwrap();
        let conf = ModelDb::embedded().find(&id).unwrap().conf.clone();
        MsiEc::new(Box::new(io), conf, Journal::in_memory(), false)
    }

    #[test]
    fn cooler_boost_sets_bit_7() {
        let mut e = ec("17S1IMS1.114");
        assert!(!e.cooler_boost().unwrap());
        e.set_cooler_boost(true).unwrap();
        assert!(e.cooler_boost().unwrap());
        assert_eq!(e.journal.entries().collect::<Vec<_>>(), vec![(0x98, 0)]);
    }

    #[test]
    fn shift_mode_round_trip() {
        let mut e = ec("17S1IMS1.114");
        e.set_shift_mode("eco").unwrap();
        assert_eq!(e.shift_mode().unwrap().as_deref(), Some("eco"));
        assert!(e.set_shift_mode("ludicrous").is_err());
    }

    #[test]
    fn charge_limit_validated() {
        let mut e = ec("17S1IMS1.114");
        e.set_charge_limit(80).unwrap();
        assert_eq!(e.charge_limit().unwrap(), Some(80));
        assert!(e.set_charge_limit(5).is_err());
    }

    #[test]
    fn restore_writes_originals() {
        let mut e = ec("17S1IMS1.114");
        e.set_cooler_boost(true).unwrap();
        e.set_shift_mode("turbo").unwrap();
        assert_eq!(e.restore().unwrap(), 2);
        assert!(!e.cooler_boost().unwrap());
        assert_eq!(e.shift_mode().unwrap().as_deref(), Some("comfort"));
    }

    #[test]
    fn refuses_mode_write_when_register_holds_unknown_byte() {
        let mut io = MockEc::with_firmware("15MMIMS1.103");
        io.regs[0xd2] = 0x37;
        let conf = ModelDb::embedded().find("15MMIMS1.103").unwrap().conf.clone();
        let mut e = MsiEc::new(Box::new(io), conf, Journal::in_memory(), false);
        assert!(e.set_shift_mode("turbo").is_err());
        assert!(e.journal.is_empty());
    }

    struct StuckEc(MockEc);
    impl EcIo for StuckEc {
        fn read(&mut self, a: u8) -> Result<u8> {
            self.0.read(a)
        }
        fn write(&mut self, a: u8, v: u8) -> Result<()> {
            // Simulates firmware that ignores writes to 0x98.
            if a == 0x98 { Ok(()) } else { self.0.write(a, v) }
        }
    }

    #[test]
    fn write_that_does_not_stick_is_an_error() {
        let conf = ModelDb::embedded().find("15MMIMS1.103").unwrap().conf.clone();
        let mut e = MsiEc::new(Box::new(StuckEc(MockEc::with_firmware("15MMIMS1.103"))), conf, Journal::in_memory(), false);
        assert!(e.set_cooler_boost(true).is_err());
    }

    #[test]
    fn wide_tachometer() {
        let mut io = MockEc::with_firmware("15MMIMS1.103");
        io.regs[0xc8] = 0x00;
        io.regs[0xc9] = 0xc8; // 200 -> 2400 rpm
        let conf = ModelDb::embedded().find("15MMIMS1.103").unwrap().conf.clone();
        let mut e = MsiEc::new(Box::new(io), conf, Journal::in_memory(), true);
        assert_eq!(e.fan_rpm().unwrap(), [Some(2400), Some(0)]);
    }

    #[test]
    fn read_only_blocks_writes() {
        let mut io = MockEc::with_firmware("17S1IMS1.114");
        let conf = ModelDb::embedded().find(&read_firmware_id(&mut io).unwrap()).unwrap().conf.clone();
        let mut e = MsiEc::new(Box::new(io), conf, Journal::in_memory(), true);
        assert!(matches!(e.set_cooler_boost(true), Err(EcError::ReadOnly)));
    }
}
