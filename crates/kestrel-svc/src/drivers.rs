//! Driver updates from Windows Update (Wi-Fi, Bluetooth, chipset, audio, GPU…), through the
//! Windows Update Agent COM API. Runs outside the state lock because a search can take a minute.

use kestrel_core::ipc::DriverUpdate;

#[cfg(windows)]
mod imp {
    use super::DriverUpdate;
    use windows::core::{Interface, BSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::UpdateAgent::*;

    const QUERY: &str = "IsInstalled=0 and Type='Driver' and IsHidden=0";

    fn e(err: windows::core::Error) -> String {
        format!("Windows Update: {}", err.message())
    }

    unsafe fn search() -> windows::core::Result<(IUpdateSession, IUpdateCollection)> {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let session: IUpdateSession = CoCreateInstance(&UpdateSession, None, CLSCTX_INPROC_SERVER)?;
        let _ = session.SetClientApplicationID(&BSTR::from("Kestrel"));
        let found = session
            .CreateUpdateSearcher()?
            .Search(&BSTR::from(QUERY))?
            .Updates()?;
        Ok((session, found))
    }

    unsafe fn id(u: &IUpdate) -> String {
        u.Identity()
            .and_then(|i| i.UpdateID())
            .map(|b| b.to_string())
            .unwrap_or_default()
    }

    pub fn list() -> Result<Vec<DriverUpdate>, String> {
        unsafe {
            let (_, found) = search().map_err(e)?;
            let mut out = Vec::new();
            for i in 0..found.Count().map_err(e)? {
                let u = found.get_Item(i).map_err(e)?;
                let (class, manufacturer) = match u.cast::<IWindowsDriverUpdate>() {
                    Ok(d) => (
                        d.DriverClass().map(|b| b.to_string()).unwrap_or_default(),
                        d.DriverManufacturer()
                            .map(|b| b.to_string())
                            .unwrap_or_default(),
                    ),
                    Err(_) => Default::default(),
                };
                out.push(DriverUpdate {
                    id: id(&u),
                    title: u.Title().map(|b| b.to_string()).unwrap_or_default(),
                    class,
                    manufacturer,
                });
            }
            Ok(out)
        }
    }

    /// Returns (reboot needed, some updates failed).
    pub fn install(ids: &[String]) -> Result<(bool, bool), String> {
        unsafe {
            let (session, found) = search().map_err(e)?;
            let pick: IUpdateCollection =
                CoCreateInstance(&UpdateCollection, None, CLSCTX_INPROC_SERVER).map_err(e)?;
            for i in 0..found.Count().map_err(e)? {
                let u = found.get_Item(i).map_err(e)?;
                if ids.contains(&id(&u)) {
                    if u.EulaAccepted().map(|b| b.0 == 0).unwrap_or(false) {
                        u.AcceptEula().map_err(e)?;
                    }
                    pick.Add(&u).map_err(e)?;
                }
            }
            if pick.Count().map_err(e)? == 0 {
                return Err("Those driver updates are no longer offered; check again.".into());
            }
            let dl = session.CreateUpdateDownloader().map_err(e)?;
            dl.SetUpdates(&pick).map_err(e)?;
            dl.Download().map_err(e)?;
            let inst = session.CreateUpdateInstaller().map_err(e)?;
            inst.SetUpdates(&pick).map_err(e)?;
            let r = inst.Install().map_err(e)?;
            let code = r.ResultCode().map_err(e)?;
            if code != orcSucceeded && code != orcSucceededWithErrors {
                return Err(format!(
                    "Windows Update could not install the drivers (result {}).",
                    code.0
                ));
            }
            Ok((
                r.RebootRequired().map(|b| b.0 != 0).unwrap_or(false),
                code == orcSucceededWithErrors,
            ))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::DriverUpdate;

    pub fn list() -> Result<Vec<DriverUpdate>, String> {
        let d = |id: &str, title: &str, class: &str, m: &str| DriverUpdate {
            id: id.into(),
            title: title.into(),
            class: class.into(),
            manufacturer: m.into(),
        };
        Ok(vec![
            d(
                "dev-wifi",
                "Intel Corporation - Net - 23.110.0.5",
                "Net",
                "Intel Corporation",
            ),
            d(
                "dev-bt",
                "Intel(R) Wireless Bluetooth(R) - 23.110.0.3",
                "Bluetooth",
                "Intel Corporation",
            ),
            d(
                "dev-chip",
                "Advanced Micro Devices, Inc. - System - 6.0.0.28",
                "System",
                "Advanced Micro Devices, Inc.",
            ),
        ])
    }

    pub fn install(_ids: &[String]) -> Result<(bool, bool), String> {
        Ok((false, false))
    }
}

pub use imp::{install, list};
