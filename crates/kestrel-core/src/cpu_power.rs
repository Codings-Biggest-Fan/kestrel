//! CPU package power limits. AMD goes through the SMU mailboxes (message IDs and addresses
//! from RyzenAdj), Intel through MSR_PKG_POWER_LIMIT. Hardware access lives in the service;
//! this module is the pure, testable part.

use serde::{Deserialize, Serialize};

/// Watts accepted from clients. Firmware thermal limits still apply above this.
pub const MIN_W: u32 = 5;
pub const MAX_W: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmdFamily {
    Raven,
    Picasso,
    Dali,
    Renoir,
    Lucienne,
    Cezanne,
    VanGogh,
    Rembrandt,
    Mendocino,
    Phoenix,
    HawkPoint,
    DragonRange,
    KrackanPoint,
    StrixPoint,
    StrixHalo,
    FireRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mailbox {
    pub msg: u32,
    pub rsp: u32,
    pub args: u32,
}

/// `(family, model)` from CPUID leaf 1 EAX, as RyzenAdj computes it.
pub fn family_model(eax: u32) -> (u32, u32) {
    let family = ((eax >> 8) & 0xf) + ((eax >> 20) & 0xff);
    let model = ((eax >> 4) & 0xf) | ((eax >> 12) & 0xf0);
    (family, model)
}

impl AmdFamily {
    pub fn detect(family: u32, model: u32) -> Option<Self> {
        use AmdFamily::*;
        Some(match (family, model) {
            (0x17, 17) => Raven,
            (0x17, 24) => Picasso,
            (0x17, 32) => Dali,
            (0x17, 96) => Renoir,
            (0x17, 104) => Lucienne,
            (0x17, 144 | 145) => VanGogh,
            (0x17, 160) => Mendocino,
            (0x19, 80) => Cezanne,
            (0x19, 64 | 68) => Rembrandt,
            (0x19, 97) => DragonRange,
            (0x19, 116 | 120) => Phoenix,
            (0x19, 117) => HawkPoint,
            (0x1a, 32 | 36) => StrixPoint,
            (0x1a, 68) => FireRange,
            (0x1a, 96) => KrackanPoint,
            (0x1a, 112) => StrixHalo,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        use AmdFamily::*;
        match self {
            Raven => "Raven Ridge",
            Picasso => "Picasso",
            Dali => "Dali",
            Renoir => "Renoir",
            Lucienne => "Lucienne",
            Cezanne => "Cezanne",
            VanGogh => "Van Gogh",
            Rembrandt => "Rembrandt",
            Mendocino => "Mendocino",
            Phoenix => "Phoenix",
            HawkPoint => "Hawk Point",
            DragonRange => "Dragon Range",
            KrackanPoint => "Krackan Point",
            StrixPoint => "Strix Point",
            StrixHalo => "Strix Halo",
            FireRange => "Fire Range",
        }
    }

    /// MP1 mailbox (RyzenAdj `nb_smu_ops.c`).
    pub fn mp1(self) -> Mailbox {
        use AmdFamily::*;
        match self {
            Rembrandt | VanGogh | Mendocino | Phoenix | HawkPoint => {
                Mailbox { msg: 0x3B1_0528, rsp: 0x3B1_0578, args: 0x3B1_0998 }
            }
            KrackanPoint | StrixPoint | StrixHalo => {
                Mailbox { msg: 0x3B1_0928, rsp: 0x3B1_0978, args: 0x3B1_0998 }
            }
            DragonRange | FireRange => Mailbox { msg: 0x3B1_0530, rsp: 0x3B1_057C, args: 0x3B1_09C4 },
            _ => Mailbox { msg: 0x3B1_0528, rsp: 0x3B1_0564, args: 0x3B1_0998 },
        }
    }

    /// MP1 message IDs `(stapm, fast, slow)` (RyzenAdj `api.c`).
    pub fn messages(self) -> (u32, u32, u32) {
        use AmdFamily::*;
        match self {
            Raven | Picasso | Dali => (0x1a, 0x1b, 0x1c),
            DragonRange | FireRange => (0x4f, 0x3e, 0x5f),
            _ => (0x14, 0x15, 0x16),
        }
    }
}

pub const MSR_RAPL_POWER_UNIT: u32 = 0x606;
pub const MSR_PKG_POWER_LIMIT: u32 = 0x610;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntelLimits {
    pub pl1_w: u32,
    pub pl2_w: u32,
    pub locked: bool,
}

fn unit_w(units: u64) -> f64 {
    1.0 / f64::from(1u32 << (units & 0xf))
}

pub fn intel_decode(units: u64, limit: u64) -> IntelLimits {
    let u = unit_w(units);
    IntelLimits {
        pl1_w: ((limit & 0x7fff) as f64 * u).round() as u32,
        pl2_w: (((limit >> 32) & 0x7fff) as f64 * u).round() as u32,
        locked: limit >> 63 == 1,
    }
}

/// New MSR value with PL1/PL2 replaced and enabled; time windows and clamp bits are kept.
pub fn intel_encode(units: u64, old: u64, pl1_w: u32, pl2_w: u32) -> u64 {
    let u = unit_w(units);
    let raw = |w: u32| ((f64::from(w) / u).round() as u64).min(0x7fff);
    let keep = old & !(0x7fff | (0x7fff << 32)) & !(1 << 63);
    keep | raw(pl1_w) | (1 << 15) | (raw(pl2_w) << 32) | (1 << 47)
}

/// Clamp a request: both in bounds and boost never below sustained.
pub fn clamp(sustained_w: u32, boost_w: u32) -> (u32, u32) {
    let s = sustained_w.clamp(MIN_W, MAX_W);
    (s, boost_w.clamp(s, MAX_W))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuPowerInfo {
    /// Limits can be written on this CPU.
    pub supported: bool,
    /// e.g. "AMD Dragon Range" or "Intel".
    pub cpu: String,
    /// Current limits when the hardware reports them (Intel).
    pub sustained_w: Option<u32>,
    pub boost_w: Option<u32>,
    /// Why it is unsupported, or a caveat.
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dragon_range() {
        // Ryzen 9 7945HX: CPUID EAX 0x00A60F12 -> family 0x19, model 0x61 (97).
        let (f, m) = family_model(0x00A6_0F12);
        assert_eq!((f, m), (0x19, 97));
        let fam = AmdFamily::detect(f, m).unwrap();
        assert_eq!(fam, AmdFamily::DragonRange);
        assert_eq!(fam.messages(), (0x4f, 0x3e, 0x5f));
        assert_eq!(fam.mp1().msg, 0x3B1_0530);
    }

    #[test]
    fn intel_roundtrip_keeps_other_bits() {
        let units = 0x000A_0E03; // power unit 1/8 W
        let old: u64 = (1 << 16) | (0x6e << 17) | (1 << 48) | (0x21 << 49) | 360 | (480 << 32);
        let l = intel_decode(units, old);
        assert_eq!((l.pl1_w, l.pl2_w, l.locked), (45, 60, false));
        let new = intel_encode(units, old, 55, 90);
        let l = intel_decode(units, new);
        assert_eq!((l.pl1_w, l.pl2_w), (55, 90));
        assert_eq!(new & ((1 << 16) | (0x6e << 17) | (1 << 48) | (0x21 << 49)), old & ((1 << 16) | (0x6e << 17) | (1 << 48) | (0x21 << 49)));
        assert!(new & (1 << 15) != 0 && new & (1 << 47) != 0);
    }

    #[test]
    fn clamps() {
        assert_eq!(clamp(1, 0), (MIN_W, MIN_W));
        assert_eq!(clamp(80, 60), (80, 80));
        assert_eq!(clamp(500, 900), (MAX_W, MAX_W));
    }
}
