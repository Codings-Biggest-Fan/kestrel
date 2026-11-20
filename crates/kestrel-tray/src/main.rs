//! kestrel-tray: a tray icon with a scenario menu. Plain Win32, no UI toolkit,
//! so it idles at a few MB. Left click opens the main window (kestrel-ui).
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
use kestrel_core::automation::Automation;
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
    const HK_SCENARIO: i32 = 1;
    const HK_BOOST: i32 = 2;
    static BOOST_HOTKEY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
            let tip: Vec<u16> = "Kestrel - Ctrl+Alt+F11 next scenario".encode_utf16().collect();
            nid.szTip[..tip.len()].copy_from_slice(&tip);
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            register_hotkeys(hwnd);
            std::thread::Builder::new().name("power".into()).stack_size(128 * 1024).spawn(power_watch).ok();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
        }
    }

    fn on_ac() -> Option<bool> {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut s = SYSTEM_POWER_STATUS::default();
        unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
        match s.ACLineStatus {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// Applies the user's power automation on every battery/AC transition.
    fn power_watch() {
        // `None` first, so the settings for the current power source apply when the tray starts.
        let mut last: Option<bool> = None;
        let mut app = AppState::default();
        loop {
            let now = on_ac();
            if let Some(ac) = now {
                if last != Some(ac) {
                    let a = Automation::load().on_power_change(ac);
                    // Retried next poll if the service is not up yet (it starts alongside the tray).
                    let sent = a.scenario.is_none_or(|s| send(Request::SetScenario(s)).is_ok());
                    if let Some(r) = a.refresh {
                        let _ = display::set(r);
                    }
                    if sent {
                        last = Some(ac);
                    }
                }
            }
            apply_app_rules(&mut app);
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }

    /// Auto engine state: the rule scenario in force and the scenario to return to afterwards.
    #[derive(Default)]
    struct AppState {
        active: Option<Scenario>,
        before: Option<Scenario>,
    }

    fn apply_app_rules(st: &mut AppState) {
        let auto = Automation::load();
        if auto.app_rules.is_empty() && st.active.is_none() {
            return;
        }
        let want = auto.app_scenario(&running_exes());
        if want == st.active {
            return;
        }
        if st.active.is_none() {
            st.before = current().and_then(|(s, _)| s);
        }
        match want.or(st.before) {
            Some(s) if send(Request::SetScenario(s)).is_ok() => st.active = want,
            None => st.active = None,
            _ => {}
        }
        if st.active.is_none() {
            st.before = None;
        }
    }

    fn running_exes() -> Vec<String> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::*;
        let mut out = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push(String::from_utf16_lossy(&e.szExeFile[..len]).to_ascii_lowercase());
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        out
    }

    mod display {
        use kestrel_core::automation::{pick_refresh, Refresh};
        use windows::core::PCWSTR;
        use windows::Win32::Graphics::Gdi::*;

        fn mode() -> DEVMODEW {
            DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() }
        }

        /// Changes the primary display's refresh rate, keeping its resolution and colour depth.
        pub fn set(want: Refresh) -> Result<u32, String> {
            unsafe {
                let mut cur = mode();
                if !EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut cur).as_bool() {
                    return Err("cannot read the display mode".into());
                }
                let mut rates = Vec::new();
                let mut m = mode();
                let mut i = 0;
                while EnumDisplaySettingsW(PCWSTR::null(), ENUM_DISPLAY_SETTINGS_MODE(i), &mut m).as_bool() {
                    if (m.dmPelsWidth, m.dmPelsHeight, m.dmBitsPerPel) == (cur.dmPelsWidth, cur.dmPelsHeight, cur.dmBitsPerPel) {
                        rates.push(m.dmDisplayFrequency);
                    }
                    i += 1;
                }
                let hz = pick_refresh(&rates, want).ok_or("no matching refresh rate")?;
                if hz == cur.dmDisplayFrequency {
                    return Ok(hz);
                }
                cur.dmDisplayFrequency = hz;
                cur.dmFields = DM_DISPLAYFREQUENCY;
                let r = ChangeDisplaySettingsExW(PCWSTR::null(), Some(&cur), None, CDS_UPDATEREGISTRY, None);
                if r != DISP_CHANGE_SUCCESSFUL {
                    return Err(format!("ChangeDisplaySettingsEx failed ({})", r.0));
                }
                Ok(hz)
            }
        }
    }

    fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
    }

    /// Ctrl+Alt+F11 cycles scenarios, Ctrl+Alt+F10 toggles Cooler Boost. Function keys, because
    /// AltGr is Ctrl+Alt and Ctrl+Alt+letter would swallow characters on many layouts (F9 is
    /// taken by Excel, F12 is reserved for the debugger). A
    /// combination another program already owns is skipped.
    unsafe fn register_hotkeys(hwnd: HWND) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, VK_F10, VK_F11};
        let mods = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
        for (id, key, name) in [(HK_SCENARIO, VK_F11, "F11"), (HK_BOOST, VK_F10, "F10")] {
            if RegisterHotKey(Some(hwnd), id, mods, key.0 as u32).is_ok() {
                if id == HK_BOOST {
                    BOOST_HOTKEY.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            } else {
                eprintln!("hotkey Ctrl+Alt+{name} is taken by another program");
            }
        }
    }

    fn on_hotkey(id: i32) {
        let Ok(status) = kestrel_core::transport::client().and_then(|mut c| c.status()) else { return };
        match id {
            HK_SCENARIO => {
                let shown = status.caps.scenarios;
                if shown.is_empty() {
                    return;
                }
                let next = match status.scenario.and_then(|s| shown.iter().position(|x| *x == s)) {
                    Some(i) => shown[(i + 1) % shown.len()],
                    None => shown[0],
                };
                let _ = send(Request::SetScenario(next));
            }
            HK_BOOST => {
                let _ = send(Request::SetCoolerBoost(status.cooler_boost != Some(true)));
            }
            _ => {}
        }
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let (scenario, boost) = current().unwrap_or((None, None));
        let connected = boost.is_some() || scenario.is_some();
        let shown = kestrel_core::transport::client()
            .and_then(|mut c| c.status())
            .map(|s| s.caps.scenarios)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| Scenario::ALL.to_vec());
        for s in shown {
            let mut flags = MF_STRING;
            if Some(s) == scenario { flags |= MF_CHECKED; }
            let label: Vec<u16> = s.label().encode_utf16().chain(Some(0)).collect();
            let _ = AppendMenuW(menu, flags, ID_SCENARIO + s.index(), PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let boost_flags = if boost == Some(true) { MF_STRING | MF_CHECKED } else if connected { MF_STRING } else { MF_STRING | MF_GRAYED };
        let boost_label = if BOOST_HOTKEY.load(std::sync::atomic::Ordering::Relaxed) { w!("Cooler Boost\tCtrl+Alt+F10") } else { w!("Cooler Boost") };
        let _ = AppendMenuW(menu, boost_flags, ID_BOOST, boost_label);
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
        if msg == WM_HOTKEY {
            let id = wp.0 as i32;
            std::thread::Builder::new().stack_size(128 * 1024).spawn(move || on_hotkey(id)).ok();
            return LRESULT(0);
        }
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
