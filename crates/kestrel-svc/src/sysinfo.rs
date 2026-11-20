//! CPU load, memory and battery without a sysinfo crate.

#[derive(Default)]
pub struct Sampler {
    last: Option<(u64, u64)>, // (idle, total)
}

pub struct Sample {
    pub cpu_load_pct: f32,
    pub ram_used_mb: u32,
    pub ram_total_mb: u32,
    pub on_ac: Option<bool>,
    pub battery_pct: Option<u8>,
}

impl Sampler {
    pub fn sample(&mut self) -> Sample {
        let load = match (imp::cpu_times(), self.last) {
            (Some((idle, total)), Some((pi, pt))) if total > pt => {
                let dt = (total - pt) as f32;
                let di = idle.saturating_sub(pi) as f32;
                self.last = Some((idle, total));
                (100.0 * (1.0 - di / dt)).clamp(0.0, 100.0)
            }
            (t, _) => {
                self.last = t;
                0.0
            }
        };
        let (used, total) = imp::memory().unwrap_or((0, 0));
        let (on_ac, battery_pct) = imp::power();
        Sample {
            cpu_load_pct: load,
            ram_used_mb: used,
            ram_total_mb: total,
            on_ac,
            battery_pct,
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::Threading::GetSystemTimes;

    fn ft(f: FILETIME) -> u64 {
        (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
    }

    pub fn cpu_times() -> Option<(u64, u64)> {
        let (mut idle, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        // Kernel time includes idle time.
        Some((ft(idle), ft(kernel) + ft(user)))
    }

    pub fn memory() -> Option<(u32, u32)> {
        let mut m = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
        let mb = |b: u64| (b / (1024 * 1024)) as u32;
        Some((mb(m.ullTotalPhys - m.ullAvailPhys), mb(m.ullTotalPhys)))
    }

    pub fn power() -> (Option<bool>, Option<u8>) {
        let mut s = SYSTEM_POWER_STATUS::default();
        if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
            return (None, None);
        }
        let ac = match s.ACLineStatus {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        (
            ac,
            (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
        )
    }
}

#[cfg(not(windows))]
mod imp {
    use std::fs;

    pub fn cpu_times() -> Option<(u64, u64)> {
        let s = fs::read_to_string("/proc/stat").ok()?;
        let v: Vec<u64> = s
            .lines()
            .next()?
            .split_whitespace()
            .skip(1)
            .filter_map(|x| x.parse().ok())
            .collect();
        let idle = v.get(3)? + v.get(4).unwrap_or(&0);
        Some((idle, v.iter().sum()))
    }

    pub fn memory() -> Option<(u32, u32)> {
        let s = fs::read_to_string("/proc/meminfo").ok()?;
        let kb = |key: &str| -> Option<u64> {
            s.lines()
                .find(|l| l.starts_with(key))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        };
        let (total, avail) = (kb("MemTotal:")?, kb("MemAvailable:")?);
        Some((((total - avail) / 1024) as u32, (total / 1024) as u32))
    }

    pub fn power() -> (Option<bool>, Option<u8>) {
        let ac = fs::read_to_string("/sys/class/power_supply/AC/online")
            .ok()
            .map(|s| s.trim() == "1");
        let pct = fs::read_to_string("/sys/class/power_supply/BAT0/capacity")
            .ok()
            .and_then(|s| s.trim().parse().ok());
        (ac, pct)
    }
}
