//! Raw Windows HID access (SetupAPI + overlapped file I/O) for one USB interface.

use kestrel_core::creative::{HidIo, PACKET};
use kestrel_core::ec::{EcError, Result};
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED};

pub struct HidDevice {
    handle: HANDLE,
    event: HANDLE,
}

// The handles are only used under the service's lock.
unsafe impl Send for HidDevice {}

fn err(e: windows::core::Error) -> EcError {
    EcError::Io(format!("HID: {e}"))
}

/// Device paths of present HID interfaces whose path names `vid_XXXX&pid_XXXX&mi_XX`.
fn find(vid: u16, pid: u16, interface: u8) -> Result<Vec<Vec<u16>>> {
    let needle = format!("vid_{vid:04x}&pid_{pid:04x}&mi_{interface:02x}");
    let mut found = Vec::new();
    unsafe {
        let guid = HidD_GetHidGuid();
        let set = SetupDiGetClassDevsW(
            Some(&guid),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(err)?;
        let mut i = 0;
        loop {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut data).is_err() {
                break;
            }
            i += 1;
            let mut need = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut need), None);
            if need == 0 {
                continue;
            }
            // u64 storage keeps the detail struct aligned.
            let mut buf = vec![0u64; (need as usize).div_ceil(8)];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), need, None, None).is_err()
            {
                continue;
            }
            let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let len = (0..).take_while(|&k| *p.add(k) != 0).count();
            let path = std::slice::from_raw_parts(p, len).to_vec();
            if String::from_utf16_lossy(&path)
                .to_ascii_lowercase()
                .contains(&needle)
            {
                found.push(path);
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    Ok(found)
}

impl HidDevice {
    /// The first matching interface, or `None` if nothing is plugged in.
    pub fn open(vid: u16, pid: u16, interface: u8) -> Result<Option<Self>> {
        let Some(mut path) = find(vid, pid, interface)?.into_iter().next() else {
            return Ok(None);
        };
        path.push(0);
        unsafe {
            let handle = CreateFileW(
                PCWSTR(path.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
            .map_err(err)?;
            let event = match CreateEventW(None, true, false, PCWSTR::null()) {
                Ok(e) => e,
                Err(e) => {
                    let _ = CloseHandle(handle);
                    return Err(err(e));
                }
            };
            Ok(Some(Self { handle, event }))
        }
    }

    /// Run one overlapped transfer, waiting at most `timeout_ms`; returns bytes moved.
    unsafe fn transfer(
        &mut self,
        write: bool,
        buf: &mut [u8; PACKET + 1],
        timeout_ms: u32,
    ) -> Result<Option<u32>> {
        let mut ov = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        let started = if write {
            WriteFile(self.handle, Some(buf), None, Some(&mut ov))
        } else {
            ReadFile(self.handle, Some(buf), None, Some(&mut ov))
        };
        if let Err(e) = started {
            // ERROR_IO_PENDING is the normal overlapped path.
            if e.code() != windows::Win32::Foundation::ERROR_IO_PENDING.to_hresult() {
                return Err(err(e));
            }
        }
        let mut n = 0u32;
        if WaitForSingleObject(self.event, timeout_ms) != WAIT_OBJECT_0 {
            let _ = CancelIo(self.handle);
            let _ = GetOverlappedResult(self.handle, &ov, &mut n, true);
            return Ok(None);
        }
        GetOverlappedResult(self.handle, &ov, &mut n, false).map_err(err)?;
        Ok(Some(n))
    }
}

impl HidIo for HidDevice {
    fn write(&mut self, packet: &[u8; PACKET]) -> Result<()> {
        let mut buf = [0u8; PACKET + 1];
        buf[1..].copy_from_slice(packet);
        match unsafe { self.transfer(true, &mut buf, 1000)? } {
            Some(_) => Ok(()),
            None => Err(EcError::Io("HID write timed out".into())),
        }
    }

    fn read(&mut self, timeout_ms: u32) -> Result<Option<[u8; PACKET]>> {
        let mut buf = [0u8; PACKET + 1];
        Ok(
            unsafe { self.transfer(false, &mut buf, timeout_ms)? }.map(|_| {
                let mut p = [0u8; PACKET];
                p.copy_from_slice(&buf[1..]);
                p
            }),
        )
    }
}

impl Drop for HidDevice {
    fn drop(&mut self) {
        unsafe {
            let _ = CancelIo(self.handle);
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}
