//! GIGABYTE laptops: `root\wmi` classes `GB_WMIACPI_Get` / `GB_WMIACPI_Set`, installed by the BIOS.
//! Getters return `Data`; setters take `Data`. See docs/HARDWARE.md.

use crate::wmi_ec::{connect, err, get_bstr, hr_err, w};
use kestrel_core::ec::{EcError, Result};
use kestrel_core::gigabyte::GbIo;
use windows::core::{BSTR, PCWSTR};
use windows::Win32::System::Variant::{
    VariantClear, VARIANT, VT_BOOL, VT_BSTR, VT_I1, VT_I2, VT_I4, VT_UI1, VT_UI2, VT_UI4,
};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE,
};

const HINT: &str = "GIGABYTE WMI interface not found (GB_WMIACPI_Get); this laptop is not a supported GIGABYTE model";

struct Target {
    path: BSTR,
    class: IWbemClassObject,
}

pub struct WmiGb {
    svc: IWbemServices,
    get: Target,
    set: Target,
}

// COM objects are created in the multithreaded apartment and only used under the service's lock.
unsafe impl Send for WmiGb {}

unsafe fn first_instance(svc: &IWbemServices, query: &str) -> Result<IWbemClassObject> {
    let en = svc
        .ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )
        .map_err(err)?;
    let mut objs = [None];
    let mut n = 0u32;
    let hr = en.Next(WBEM_INFINITE, &mut objs, &mut n);
    if (hr.0 as u32) & 0x8000_0000 != 0 {
        return Err(hr_err(hr.0 as u32, query));
    }
    objs[0]
        .take()
        .ok_or_else(|| EcError::Io(format!("{query}: no instance")))
}

unsafe fn target(svc: &IWbemServices, class: &str) -> Result<Target> {
    let inst = first_instance(svc, &format!("SELECT * FROM {class}"))
        .map_err(|e| EcError::Io(format!("{HINT}: {e}")))?;
    let path = get_bstr(&inst, "__PATH")?;
    let mut obj = None;
    svc.GetObject(
        &BSTR::from(class),
        WBEM_GENERIC_FLAG_TYPE(0),
        None,
        Some(&mut obj),
        None,
    )
    .map_err(err)?;
    Ok(Target {
        path,
        class: obj.ok_or(EcError::Unsupported("GIGABYTE WMI class"))?,
    })
}

fn byte_variant(v: u8) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        (*var.Anonymous.Anonymous).vt = VT_UI1;
        (*var.Anonymous.Anonymous).Anonymous.bVal = v;
    }
    var
}

unsafe fn get_u32(obj: &IWbemClassObject, name: &str) -> Result<u32> {
    let mut v = VARIANT::default();
    obj.Get(PCWSTR(w(name).as_ptr()), 0, &mut v, None, None)
        .map_err(err)?;
    let a = &(*v.Anonymous.Anonymous);
    let r = match a.vt {
        VT_UI1 => Ok(a.Anonymous.bVal as u32),
        VT_I1 => Ok(a.Anonymous.cVal as u8 as u32),
        VT_UI2 => Ok(a.Anonymous.uiVal as u32),
        VT_I2 => Ok(a.Anonymous.iVal as u16 as u32),
        VT_UI4 => Ok(a.Anonymous.ulVal),
        VT_I4 => Ok(a.Anonymous.lVal as u32),
        VT_BOOL => Ok(u32::from(a.Anonymous.boolVal.as_bool())),
        vt => Err(EcError::Io(format!(
            "{name}: unexpected VARIANT type {}",
            vt.0
        ))),
    };
    let _ = VariantClear(&mut v);
    r
}

impl WmiGb {
    pub fn open() -> Result<Self> {
        let svc = connect(r"root\wmi")?;
        unsafe {
            let get = target(&svc, "GB_WMIACPI_Get")?;
            let set = target(&svc, "GB_WMIACPI_Set")?;
            Ok(Self { svc, get, set })
        }
    }

    /// Run `method` on the Get (`set == false`) or Set object with byte arguments.
    fn call(
        &self,
        set: bool,
        method: &str,
        args: &[(&str, u8)],
    ) -> Result<Option<IWbemClassObject>> {
        let t = if set { &self.set } else { &self.get };
        unsafe {
            let mut sig = None;
            t.class
                .GetMethod(
                    PCWSTR(w(method).as_ptr()),
                    0,
                    &mut sig,
                    std::ptr::null_mut(),
                )
                .map_err(|e| EcError::Io(format!("{method}: {}", err(e))))?;
            let params = match sig {
                Some(sig) => {
                    let p = sig.SpawnInstance(0).map_err(err)?;
                    for (name, v) in args {
                        let mut var = byte_variant(*v);
                        p.Put(PCWSTR(w(name).as_ptr()), 0, &var, 0).map_err(err)?;
                        let _ = VariantClear(&mut var);
                    }
                    Some(p)
                }
                None if args.is_empty() => None,
                None => return Err(EcError::Io(format!("{method} takes no arguments"))),
            };
            let mut out = None;
            self.svc
                .ExecMethod(
                    &t.path,
                    &BSTR::from(method),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    params.as_ref(),
                    Some(&mut out),
                    None,
                )
                .map_err(|e| EcError::Io(format!("{method}: {}", err(e))))?;
            Ok(out)
        }
    }
}

impl GbIo for WmiGb {
    fn get(&mut self, method: &str) -> Result<u32> {
        let out = self
            .call(false, method, &[])?
            .ok_or_else(|| EcError::Io(format!("{method} returned nothing")))?;
        unsafe { get_u32(&out, "Data") }
    }

    fn set(&mut self, method: &str, value: u8) -> Result<()> {
        self.call(true, method, &[("Data", value)]).map(|_| ())
    }

    fn fan_point(&mut self, index: u8) -> Result<(u8, u8)> {
        let out = self
            .call(false, "GetFanIndexValue", &[("Index", index)])?
            .ok_or_else(|| EcError::Io("GetFanIndexValue returned nothing".into()))?;
        // "Temperture" is GIGABYTE's spelling in the BIOS MOF.
        unsafe {
            Ok((
                get_u32(&out, "Temperture")? as u8,
                get_u32(&out, "Value")? as u8,
            ))
        }
    }

    fn set_fan_point(&mut self, index: u8, temp: u8, duty: u8) -> Result<()> {
        // SetFanIndexValue lives on the Get class in the MOF; fall back to Set on older BIOSes.
        let args = [("Index", index), ("Temperture", temp), ("Value", duty)];
        self.call(false, "SetFanIndexValue", &args)
            .or_else(|_| self.call(true, "SetFanIndexValue", &args))
            .map(|_| ())
    }
}

/// (manufacturer, model, SKU) from `Win32_ComputerSystem`.
pub fn system_identity() -> Result<(String, String, String)> {
    let svc = connect(r"root\cimv2")?;
    unsafe {
        let cs = first_instance(
            &svc,
            "SELECT Manufacturer, Model, SystemSKUNumber FROM Win32_ComputerSystem",
        )?;
        let s = |n: &str| -> String {
            let mut v = VARIANT::default();
            if cs
                .Get(PCWSTR(w(n).as_ptr()), 0, &mut v, None, None)
                .is_err()
                || (*v.Anonymous.Anonymous).vt != VT_BSTR
            {
                let _ = VariantClear(&mut v);
                return String::new();
            }
            let out = (*(*v.Anonymous.Anonymous).Anonymous.bstrVal).to_string();
            let _ = VariantClear(&mut v);
            out.trim().to_string()
        };
        Ok((s("Manufacturer"), s("Model"), s("SystemSKUNumber")))
    }
}
