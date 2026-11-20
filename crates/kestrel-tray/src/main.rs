//! kestrel-tray: a tray icon with a scenario menu. Plain Win32, no UI toolkit,
//! so it idles at a few MB. Left click opens the main window (kestrel-ui).
#![cfg_attr(windows, windows_subsystem = "windows")]

use kestrel_core::ipc::Request;
use kestrel_core::scenario::Scenario;

fn send(req: Request) -> Result<(), String> {
    let mut c = kestrel_core::transport::client().map_err(|e| e.to_string())?;
    c.command(&req).map_err(|e| e.to_string())
}

fn current() -> Option<(Option<Scenario>, Option<bool>)> {
    let s = kestrel_core::transport::client().ok()?.status().ok()?;
    Some((s.scenario, s.cooler_boost))
}

#[cfg(windows)]
fn open_ui() {
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(ui_exe())));
    if let Some(exe) = exe {
        let _ = std::process::Command::new(exe).spawn();
    }
}

#[cfg(windows)]
fn ui_exe() -> &'static str {
    "kestrel-ui.exe"
}

#[cfg(not(windows))]
fn main() {
    // Development stand-in: `kestrel-tray <scenario-index>` switches scenario.
    match std::env::args().nth(1).and_then(|a| a.parse().ok()).and_then(Scenario::from_index) {
        Some(s) => println!("{:?}", send(Request::SetScenario(s))),
        None => println!("{:?}", current()),
    }
}

#[cfg(windows)]
fn main() {
    win::run();
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY: u32 = WM_APP + 1;
    const ID_OPEN: usize = 1;
    const ID_BOOST: usize = 2;
    const ID_QUIT: usize = 3;
    const ID_SCENARIO: usize = 100;

    pub fn run() {
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class = w!("KestrelTray");
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let Ok(hwnd) = CreateWindowExW(
                WINDOW_EX_STYLE(0), class, w!("Kestrel"), WINDOW_STYLE(0), 0, 0, 0, 0,
                Some(HWND_MESSAGE), None, Some(hinst.into()), None,
            ) else { return };
            let mut nid = icon_data(hwnd);
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            nid.hIcon = LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).or_else(|_| LoadIconW(None, IDI_APPLICATION)).unwrap_or_default();
            let tip: Vec<u16> = "Kestrel".encode_utf16().collect();
            nid.szTip[..tip.len()].copy_from_slice(&tip);
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
        }
    }

    fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let (scenario, boost) = current().unwrap_or((None, None));
        let connected = boost.is_some() || scenario.is_some();
        for s in Scenario::ALL {
            let mut flags = MF_STRING;
            if Some(s) == scenario { flags |= MF_CHECKED; }
            let label: Vec<u16> = s.label().encode_utf16().chain(Some(0)).collect();
            let _ = AppendMenuW(menu, flags, ID_SCENARIO + s.index(), PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let boost_flags = if boost == Some(true) { MF_STRING | MF_CHECKED } else if connected { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, boost_flags, ID_BOOST, w!("Cooler Boost"));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN, w!("Open Kestrel"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit tray"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            0 => {}
            ID_OPEN => open_ui(),
            ID_BOOST => { let _ = send(Request::SetCoolerBoost(boost != Some(true))); }
            ID_QUIT => PostQuitMessage(0),
            n if n >= ID_SCENARIO => {
                if let Some(s) = Scenario::from_index(n - ID_SCENARIO) { let _ = send(Request::SetScenario(s)); }
            }
            _ => {}
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_TRAY {
            match lp.0 as u32 {
                WM_LBUTTONUP => open_ui(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}
