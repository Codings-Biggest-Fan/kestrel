//! EC access through MSI's ACPI-WMI interface (`root\wmi` `MSI_ACPI`).
//! See docs/HARDWARE.md. Calls are serialized by the caller (one `Mutex` around the backend).

use kestrel_core::ec::{EcError, EcIo, Result};
use std::mem::ManuallyDrop;
use windows::core::{Interface, BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
    SAFEARRAY,
};
use windows::Win32::System::Ole::{SafeArrayAccessData, SafeArrayCreateVector, SafeArrayGetUBound, SafeArrayUnaccessData};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_ARRAY, VT_BSTR, VT_UI1, VT_UNKNOWN};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE,
};

fn err(e: windows::core::Error) -> EcError {
    EcError::Io(format!("WMI: {e}"))
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct WmiEc {
    svc: IWbemServices,
    path: BSTR,
    pkg_class: IWbemClassObject,
    acpi_class: IWbemClassObject,
}

// COM objects are created in the multithreaded apartment and only used under the service's lock.
unsafe impl Send for WmiEc {}

impl WmiEc {
    pub fn open() -> Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            // Fails harmlessly if the process already set security.
            let _ = CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            );
            let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(err)?;
            let svc = loc
                .ConnectServer(&BSTR::from(r"root\wmi"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
                .map_err(err)?;
            CoSetProxyBlanket(
                &svc,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .map_err(err)?;

            let en = svc
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from("SELECT * FROM MSI_ACPI"),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .map_err(err)?;
            let mut objs = [None];
            let mut n = 0u32;
            let _ = en.Next(WBEM_INFINITE, &mut objs, &mut n);
            let inst = objs[0].take().ok_or(EcError::Unsupported("MSI_ACPI WMI interface"))?;
            let path = get_bstr(&inst, "__PATH")?;

            let mut acpi_class = None;
            svc.GetObject(&BSTR::from("MSI_ACPI"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut acpi_class), None)
                .map_err(err)?;
            let mut pkg_class = None;
            svc.GetObject(&BSTR::from("Package_32"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut pkg_class), None)
                .map_err(err)?;
            Ok(Self {
                svc,
                path,
                pkg_class: pkg_class.ok_or(EcError::Unsupported("Package_32 class"))?,
                acpi_class: acpi_class.ok_or(EcError::Unsupported("MSI_ACPI class"))?,
            })
        }
    }

    /// Call `method` with `Data = Package_32 { Bytes = input }` and return the out `Data.Bytes`.
    fn call(&self, method: &str, input: Option<[u8; 32]>) -> Result<Vec<u8>> {
        unsafe {
            let in_params = match input {
                Some(bytes) => {
                    let mut sig = None;
                    self.acpi_class
                        .GetMethod(PCWSTR(w(method).as_ptr()), 0, &mut sig, std::ptr::null_mut())
                        .map_err(err)?;
                    let sig = sig.ok_or(EcError::Unsupported("MSI_ACPI method"))?;
                    let params = sig.SpawnInstance(0).map_err(err)?;
                    let pkg = self.pkg_class.SpawnInstance(0).map_err(err)?;
                    let mut arr = byte_array_variant(&bytes)?;
                    pkg.Put(PCWSTR(w("Bytes").as_ptr()), 0, &arr, 0).map_err(err)?;
                    let _ = VariantClear(&mut arr);
                    let mut obj = VARIANT::default();
                    (*obj.Anonymous.Anonymous).vt = VT_UNKNOWN;
                    (*obj.Anonymous.Anonymous).Anonymous.punkVal = ManuallyDrop::new(Some(pkg.cast().map_err(err)?));
                    params.Put(PCWSTR(w("Data").as_ptr()), 0, &obj, 0).map_err(err)?;
                    let _ = VariantClear(&mut obj);
                    Some(params)
                }
                None => None,
            };
            let mut out = None;
            self.svc
                .ExecMethod(
                    &self.path,
                    &BSTR::from(method),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    in_params.as_ref(),
                    Some(&mut out),
                    None,
                )
                .map_err(err)?;
            let out = out.ok_or_else(|| EcError::Io(format!("{method} returned nothing")))?;
            let mut data = VARIANT::default();
            out.Get(PCWSTR(w("Data").as_ptr()), 0, &mut data, None, None).map_err(err)?;
            let unk = (*data.Anonymous.Anonymous).Anonymous.punkVal.clone();
            let _ = VariantClear(&mut data);
            let pkg: IWbemClassObject = ManuallyDrop::into_inner(unk)
                .ok_or_else(|| EcError::Io(format!("{method}: empty Data")))?
                .cast()
                .map_err(err)?;
            let mut bytes = VARIANT::default();
            pkg.Get(PCWSTR(w("Bytes").as_ptr()), 0, &mut bytes, None, None).map_err(err)?;
            let v = read_byte_array(&bytes);
            let _ = VariantClear(&mut bytes);
            v
        }
    }

    /// EC firmware ID via `Get_EC`.
    pub fn firmware_id(&self) -> Result<String> {
        let b = self.call("Get_EC", None)?;
        let s: String = b.iter().skip(2).take_while(|&&c| c != 0).filter(|c| c.is_ascii_graphic()).map(|&c| c as char).collect();
        Ok(s.chars().take(12).collect())
    }
}

impl EcIo for WmiEc {
    fn read(&mut self, addr: u8) -> Result<u8> {
        let mut b = [0u8; 32];
        b[0] = addr;
        let out = self.call("Get_Data", Some(b))?;
        out.get(1).copied().ok_or_else(|| EcError::Io("Get_Data: short reply".into()))
    }

    fn write(&mut self, addr: u8, value: u8) -> Result<()> {
        let mut b = [0u8; 32];
        b[0] = addr;
        b[1] = value;
        self.call("Set_Data", Some(b)).map(|_| ())
    }
}

unsafe fn get_bstr(obj: &IWbemClassObject, name: &str) -> Result<BSTR> {
    let mut v = VARIANT::default();
    obj.Get(PCWSTR(w(name).as_ptr()), 0, &mut v, None, None).map_err(err)?;
    if (*v.Anonymous.Anonymous).vt != VT_BSTR {
        let _ = VariantClear(&mut v);
        return Err(EcError::Io(format!("{name} is not a string")));
    }
    let s = (*(*v.Anonymous.Anonymous).Anonymous.bstrVal).clone();
    let _ = VariantClear(&mut v);
    Ok(s)
}

unsafe fn byte_array_variant(bytes: &[u8]) -> Result<VARIANT> {
    let sa: *mut SAFEARRAY = SafeArrayCreateVector(VT_UI1, 0, bytes.len() as u32);
    if sa.is_null() {
        return Err(EcError::Io("SafeArrayCreateVector failed".into()));
    }
    let mut p = std::ptr::null_mut();
    SafeArrayAccessData(sa, &mut p).map_err(err)?;
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    SafeArrayUnaccessData(sa).map_err(err)?;
    let mut v = VARIANT::default();
    (*v.Anonymous.Anonymous).vt = VT_ARRAY | VT_UI1;
    (*v.Anonymous.Anonymous).Anonymous.parray = sa;
    Ok(v)
}

unsafe fn read_byte_array(v: &VARIANT) -> Result<Vec<u8>> {
    if (*v.Anonymous.Anonymous).vt != (VT_ARRAY | VT_UI1) {
        return Err(EcError::Io("Bytes is not a byte array".into()));
    }
    let sa = (*v.Anonymous.Anonymous).Anonymous.parray;
    let len = (SafeArrayGetUBound(sa, 1).map_err(err)? + 1) as usize;
    let mut p = std::ptr::null_mut();
    SafeArrayAccessData(sa, &mut p).map_err(err)?;
    let out = std::slice::from_raw_parts(p as *const u8, len).to_vec();
    SafeArrayUnaccessData(sa).map_err(err)?;
    Ok(out)
}
