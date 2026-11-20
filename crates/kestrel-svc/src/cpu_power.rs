//! CPU power limits through PawnIO (signed kernel driver, LGPL modules embedded below).
//! AMD: SMU MP1 mailbox writes via the RyzenSMU module. Intel: MSR_PKG_POWER_LIMIT via IntelMSR.

use kestrel_core::cpu_power::{self as cp, AmdFamily, CpuPowerInfo};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cpu {
    Amd(AmdFamily),
    Intel,
    Other,
}

fn detect() -> (Cpu, String) {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        #[allow(unused_unsafe)]
        // SAFETY: CPUID is always available on x86_64.
        let (v, l1) = unsafe { (__cpuid(0), __cpuid(1)) };
        let mut vendor = Vec::with_capacity(12);
        for r in [v.ebx, v.edx, v.ecx] {
            vendor.extend_from_slice(&r.to_le_bytes());
        }
        let (family, model) = cp::family_model(l1.eax);
        return match &vendor[..] {
            b"AuthenticAMD" => match AmdFamily::detect(family, model) {
                Some(f) => (Cpu::Amd(f), format!("AMD {}", f.name())),
                None => (Cpu::Other, format!("AMD family {family:#x} model {model}")),
            },
            b"GenuineIntel" => (Cpu::Intel, "Intel".into()),
            _ => (Cpu::Other, String::from_utf8_lossy(&vendor).into_owned()),
        };
    }
    #[allow(unreachable_code)]
    (Cpu::Other, "unknown CPU".into())
}

pub fn info() -> CpuPowerInfo {
    let (cpu, name) = detect();
    let mut i = CpuPowerInfo { cpu: name, ..Default::default() };
    match cpu {
        Cpu::Other => i.note = "Power limits aren't supported on this CPU.".into(),
        Cpu::Amd(_) => match imp::available() {
            Ok(()) => {
                i.supported = true;
                i.note = "The CPU doesn't report its current limits. Defaults return after a reboot.".into();
            }
            Err(e) => i.note = e,
        },
        Cpu::Intel => match imp::intel_read() {
            Ok(l) if l.locked => i.note = "The BIOS has locked the power limits on this laptop.".into(),
            Ok(l) => {
                i.supported = true;
                i.sustained_w = Some(l.pl1_w);
                i.boost_w = Some(l.pl2_w);
            }
            Err(e) => i.note = e,
        },
    }
    i
}

pub fn set(sustained_w: u32, boost_w: u32) -> Result<(), String> {
    let (s, b) = cp::clamp(sustained_w, boost_w);
    match detect().0 {
        Cpu::Amd(f) => {
            let mb = f.mp1();
            let (stapm, fast, slow) = f.messages();
            for (msg, w) in [(stapm, s), (slow, s), (fast, b)] {
                imp::smu_send(mb, msg, w * 1000)?;
            }
            Ok(())
        }
        Cpu::Intel => imp::intel_write(s, b),
        Cpu::Other => Err("Power limits aren't supported on this CPU.".into()),
    }
}

/// Puts back the limits found before Kestrel first changed them (Intel only).
pub fn reset() -> Result<(), String> {
    match detect().0 {
        Cpu::Intel => imp::intel_restore(),
        _ => Err("This CPU doesn't report its default limits. Reboot to restore them.".into()),
    }
}

#[cfg(windows)]
mod imp {
    use kestrel_core::cpu_power::{self as cp, IntelLimits, Mailbox};
    use std::sync::Mutex;
    use windows::core::{s, w, HRESULT, PCSTR};
    use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

    const RYZEN_SMU: &[u8] = include_bytes!("../pawnio/RyzenSMU.bin");
    const INTEL_MSR: &[u8] = include_bytes!("../pawnio/IntelMSR.bin");
    const NOT_INSTALLED: &str = "Install the PawnIO driver (pawnio.eu) to enable power limits.";

    type Open = unsafe extern "system" fn(*mut HANDLE) -> HRESULT;
    type Load = unsafe extern "system" fn(HANDLE, *const u8, usize) -> HRESULT;
    type Exec = unsafe extern "system" fn(HANDLE, PCSTR, *const u64, usize, *mut u64, usize, *mut usize) -> HRESULT;

    struct Module {
        exec: Exec,
        h: HANDLE,
    }
    // SAFETY: the PawnIO handle is a plain kernel handle; calls are serialized by the mutexes below.
    unsafe impl Send for Module {}

    fn open(blob: &[u8]) -> Result<Module, String> {
        // SAFETY: loading PawnIOLib and resolving its documented exports with matching signatures.
        unsafe {
            let lib = LoadLibraryW(w!("C:\\Program Files\\PawnIO\\PawnIOLib.dll"))
                .or_else(|_| LoadLibraryW(w!("PawnIOLib.dll")))
                .map_err(|_| NOT_INSTALLED.to_string())?;
            let f = |n: PCSTR| GetProcAddress(lib, n).ok_or_else(|| NOT_INSTALLED.to_string());
            let open: Open = std::mem::transmute(f(s!("pawnio_open"))?);
            let load: Load = std::mem::transmute(f(s!("pawnio_load"))?);
            let exec: Exec = std::mem::transmute(f(s!("pawnio_execute"))?);
            let mut h = HANDLE::default();
            open(&mut h).ok().map_err(|e| format!("PawnIO: {e}"))?;
            load(h, blob.as_ptr(), blob.len()).ok().map_err(|e| format!("PawnIO module: {e}"))?;
            Ok(Module { exec, h })
        }
    }

    impl Module {
        fn call(&self, name: PCSTR, input: &[u64], out: &mut [u64]) -> Result<(), String> {
            let mut n = 0usize;
            // SAFETY: buffers are valid for the given element counts.
            unsafe { (self.exec)(self.h, name, input.as_ptr(), input.len(), out.as_mut_ptr(), out.len(), &mut n) }
                .ok()
                .map_err(|e| format!("PawnIO: {e}"))
        }
    }

    static SMU: Mutex<Option<Module>> = Mutex::new(None);
    static MSR: Mutex<Option<Module>> = Mutex::new(None);
    static INTEL_ORIGINAL: Mutex<Option<u64>> = Mutex::new(None);

    fn with<T>(m: &Mutex<Option<Module>>, blob: &[u8], f: impl FnOnce(&Module) -> Result<T, String>) -> Result<T, String> {
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        if g.is_none() {
            *g = Some(open(blob)?);
        }
        f(g.as_ref().unwrap())
    }

    pub fn available() -> Result<(), String> {
        with(&SMU, RYZEN_SMU, |_| Ok(()))
    }

    /// Holds the cross-tool PCI lock that HWiNFO, LibreHardwareMonitor and others honour.
    struct PciLock(HANDLE);
    impl PciLock {
        fn take() -> Result<Self, String> {
            // SAFETY: named mutex create/wait with a valid name and timeout.
            unsafe {
                let h = CreateMutexW(None, false, w!("Global\\Access_PCI")).map_err(|e| e.to_string())?;
                let r = WaitForSingleObject(h, 2000);
                if r != WAIT_OBJECT_0 && r != WAIT_ABANDONED {
                    return Err("Another tool is using the CPU's management interface; try again.".into());
                }
                Ok(PciLock(h))
            }
        }
    }
    impl Drop for PciLock {
        fn drop(&mut self) {
            // SAFETY: we own the mutex and the handle.
            unsafe {
                let _ = ReleaseMutex(self.0);
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }

    pub fn smu_send(mb: Mailbox, msg: u32, arg0: u32) -> Result<(), String> {
        with(&SMU, RYZEN_SMU, |m| {
            let _lock = PciLock::take()?;
            let rd = |a: u32| -> Result<u32, String> {
                let mut o = [0u64; 1];
                m.call(s!("ioctl_read_smu_register"), &[a.into()], &mut o)?;
                Ok(o[0] as u32)
            };
            let wr = |a: u32, v: u32| m.call(s!("ioctl_write_smu_register"), &[a.into(), v.into()], &mut []);
            let wait = || -> Result<u32, String> {
                for _ in 0..20_000 {
                    let r = rd(mb.rsp)?;
                    if r != 0 {
                        return Ok(r);
                    }
                }
                Err("The CPU's management unit didn't respond.".into())
            };
            wait()?;
            wr(mb.rsp, 0)?;
            for i in 0..6 {
                wr(mb.args + 4 * i, if i == 0 { arg0 } else { 0 })?;
            }
            wr(mb.msg, msg)?;
            match wait()? {
                1 => Ok(()),
                0xfe => Err(format!("This CPU doesn't accept SMU message {msg:#x}.")),
                r => Err(format!("The CPU rejected the limit (SMU response {r:#x}).")),
            }
        })
    }

    fn msr(m: &Module, reg: u32) -> Result<u64, String> {
        let mut o = [0u64; 1];
        m.call(s!("ioctl_read_msr"), &[reg.into()], &mut o)?;
        Ok(o[0])
    }

    pub fn intel_read() -> Result<IntelLimits, String> {
        with(&MSR, INTEL_MSR, |m| Ok(cp::intel_decode(msr(m, cp::MSR_RAPL_POWER_UNIT)?, msr(m, cp::MSR_PKG_POWER_LIMIT)?)))
    }

    pub fn intel_write(pl1: u32, pl2: u32) -> Result<(), String> {
        with(&MSR, INTEL_MSR, |m| {
            let units = msr(m, cp::MSR_RAPL_POWER_UNIT)?;
            let old = msr(m, cp::MSR_PKG_POWER_LIMIT)?;
            if old >> 63 == 1 {
                return Err("The BIOS has locked the power limits on this laptop.".into());
            }
            INTEL_ORIGINAL.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(old);
            let new = cp::intel_encode(units, old, pl1, pl2);
            m.call(s!("ioctl_write_msr"), &[cp::MSR_PKG_POWER_LIMIT.into(), new], &mut [])?;
            let back = msr(m, cp::MSR_PKG_POWER_LIMIT)?;
            if back & !(1 << 63) != new & !(1 << 63) {
                let _ = m.call(s!("ioctl_write_msr"), &[cp::MSR_PKG_POWER_LIMIT.into(), old], &mut []);
                return Err("The CPU didn't keep the new limits; restored the previous ones.".into());
            }
            Ok(())
        })
    }

    pub fn intel_restore() -> Result<(), String> {
        let Some(orig) = *INTEL_ORIGINAL.lock().unwrap_or_else(|p| p.into_inner()) else { return Ok(()) };
        with(&MSR, INTEL_MSR, |m| m.call(s!("ioctl_write_msr"), &[cp::MSR_PKG_POWER_LIMIT.into(), orig], &mut []))
    }
}

/// Development stand-in: remembers the last limits so the UI can be exercised.
#[cfg(not(windows))]
mod imp {
    use kestrel_core::cpu_power::{IntelLimits, Mailbox};
    use std::sync::Mutex;

    static LAST: Mutex<(u32, u32)> = Mutex::new((45, 65));

    pub fn available() -> Result<(), String> {
        Ok(())
    }
    pub fn smu_send(_mb: Mailbox, msg: u32, arg0: u32) -> Result<(), String> {
        eprintln!("mock SMU msg {msg:#x} arg {arg0}");
        Ok(())
    }
    pub fn intel_read() -> Result<IntelLimits, String> {
        let (a, b) = *LAST.lock().unwrap();
        Ok(IntelLimits { pl1_w: a, pl2_w: b, locked: false })
    }
    pub fn intel_write(pl1: u32, pl2: u32) -> Result<(), String> {
        *LAST.lock().unwrap() = (pl1, pl2);
        Ok(())
    }
    pub fn intel_restore() -> Result<(), String> {
        *LAST.lock().unwrap() = (45, 65);
        Ok(())
    }
}
