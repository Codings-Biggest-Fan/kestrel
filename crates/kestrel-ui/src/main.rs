//! kestrel-ui: main window. Talks to kestrel-svc over IPC and exits when closed.
#![cfg_attr(windows, windows_subsystem = "windows")]

use kestrel_core::automation::{AppRule, Automation};
use kestrel_core::profiles::{self, Profile};
use kestrel_core::ipc::{Request, Response, Status};
use kestrel_core::scenario::Scenario;
use slint::{Model, ModelRc, SharedString, VecModel, Weak};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

slint::include_modules!();

const HISTORY: usize = 60;

fn hint(s: Scenario) -> &'static str {
    match s {
        Scenario::Extreme => "Highest power limits and fan speeds",
        Scenario::Balanced => "MSI's default: performance with sensible noise",
        Scenario::Silent => "Lower power, quieter fans",
        Scenario::SuperBattery => "Lowest power draw for the longest battery life",
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> SharedString {
    v.map(|v| format!("{v}{unit}")).unwrap_or_else(|| "–".into()).into()
}

fn fan(rpm: Option<u32>, pct: Option<u8>) -> SharedString {
    match (rpm, pct) {
        (Some(r), _) => format!("{r} rpm").into(),
        (None, Some(p)) => format!("{p}%").into(),
        _ => "–".into(),
    }
}

fn history(q: &VecDeque<f32>) -> ModelRc<f32> {
    ModelRc::new(VecModel::from(q.iter().copied().collect::<Vec<_>>()))
}

/// 270° ring arc (gap at the bottom) filled to `frac`, in a 100x100 viewbox.
fn arc(frac: f32) -> SharedString {
    let frac = frac.clamp(0.0, 1.0);
    if frac < 0.005 {
        return SharedString::new();
    }
    let (r, a0) = (42.0f32, 135f32.to_radians());
    let a1 = a0 + 270f32.to_radians() * frac.min(0.9999);
    let pt = |a: f32| (50.0 + r * a.cos(), 50.0 + r * a.sin());
    let ((x0, y0), (x1, y1)) = (pt(a0), pt(a1));
    let large = u8::from(270.0 * frac > 180.0);
    format!("M{x0:.2} {y0:.2} A{r} {r} 0 {large} 1 {x1:.2} {y1:.2}").into()
}

fn apply(ui: &AppWindow, s: &Status, temps: &VecDeque<f32>, loads: &VecDeque<f32>) {
    if !ui.get_connected() {
        // Errors from while the service was down no longer apply.
        ui.set_message(SharedString::new());
    }
    ui.set_connected(true);
    ui.set_model_name(if s.model.name.is_empty() { "MSI laptop".into() } else { s.model.name.clone().into() });
    ui.set_firmware(s.model.firmware.clone().into());
    ui.set_read_only(s.model.read_only);
    ui.set_supported(s.model.map_id.is_some());
    if !s.caps.scenarios.is_empty() && *SHOWN.lock().unwrap_or_else(|p| p.into_inner()) != s.caps.scenarios {
        show_scenarios(ui, s.caps.scenarios.clone());
    }
    if CURVE_POINTS.swap(s.caps.curve_points.into(), Ordering::Relaxed) != u64::from(s.caps.curve_points) || ui.get_profiles().row_count() == 0 {
        show_profiles(ui);
    }
    ui.set_scenario(shown_index(s.scenario));
    ui.set_cpu_temp(opt(s.cpu_temp, ""));
    ui.set_gpu_temp(opt(s.gpu_temp, ""));
    ui.set_cpu_temp_f(s.cpu_temp.unwrap_or(0) as f32 / 100.0);
    ui.set_gpu_temp_f(s.gpu_temp.unwrap_or(0) as f32 / 100.0);
    ui.set_cpu_fan(fan(s.cpu_fan_rpm, s.cpu_fan_pct));
    ui.set_gpu_fan(fan(s.gpu_fan_rpm, s.gpu_fan_pct));
    ui.set_cpu_load(s.cpu_load_pct);
    ui.set_cpu_arc(arc(s.cpu_temp.unwrap_or(0) as f32 / 100.0));
    ui.set_gpu_arc(arc(s.gpu_temp.unwrap_or(0) as f32 / 100.0));
    ui.set_load_arc(arc(s.cpu_load_pct / 100.0));
    ui.set_ram_arc(arc(if s.ram_total_mb > 0 { s.ram_used_mb as f32 / s.ram_total_mb as f32 } else { 0.0 }));
    ui.set_ram(format!("{:.1} of {:.0} GB", s.ram_used_mb as f32 / 1024.0, s.ram_total_mb as f32 / 1024.0).into());
    ui.set_ram_f(if s.ram_total_mb > 0 { s.ram_used_mb as f32 / s.ram_total_mb as f32 } else { 0.0 });
    ui.set_battery(opt(s.battery_pct, "%"));
    ui.set_power_source(match s.on_ac { Some(true) => "Plugged in", Some(false) => "On battery", None => "Unknown" }.into());
    ui.set_has_boost(s.caps.cooler_boost);
    if s.caps.fan_curve && !ui.get_has_curve() {
        ui.invoke_load_curve(ui.get_curve_gpu());
    }
    ui.set_has_curve(s.caps.fan_curve);
    ui.set_cooler_boost(s.cooler_boost.unwrap_or(false));
    ui.set_has_charge(s.caps.charge_limit);
    if !CPU_LOADED.swap(true, Ordering::Relaxed) {
        load_cpu_power(ui.as_weak());
    }
    ui.set_charge_limit(s.charge_limit.unwrap_or(100) as i32);
    ui.set_has_kbd(s.caps.kbd_backlight);
    ui.set_kbd_level(s.kbd_backlight.map(i32::from).unwrap_or(-1));
    ui.set_temp_history(history(temps));
    ui.set_cpu_history(history(loads));
}

fn poll(weak: Weak<AppWindow>) {
    let mut temps = VecDeque::with_capacity(HISTORY);
    let mut loads = VecDeque::with_capacity(HISTORY);
    let mut client = None;
    loop {
        if client.is_none() {
            client = kestrel_core::transport::client().ok();
        }
        let status = client.as_mut().and_then(|c| c.status().ok());
        if status.is_none() {
            client = None;
        }
        if let Some(s) = &status {
            // A missing reading is skipped, not plotted as 0 °C.
            for (q, v) in [(&mut temps, s.cpu_temp.map(|t| t as f32 / 100.0)), (&mut loads, Some(s.cpu_load_pct / 100.0))] {
                let Some(v) = v else { continue };
                if q.len() == HISTORY {
                    q.pop_front();
                }
                q.push_back(v.clamp(0.0, 1.0));
            }
        }
        let (t, l) = (temps.clone(), loads.clone());
        let alive = weak
            .upgrade_in_event_loop(move |ui| match status {
                Some(s) => apply(&ui, &s, &t, &l),
                None => {
                    ui.set_connected(false);
                    ui.set_model_name("Service not running".into());
                }
            })
            .is_ok();
        if !alive {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn send(weak: Weak<AppWindow>, req: Request) {
    std::thread::spawn(move || {
        let r = kestrel_core::transport::client().and_then(|mut c| c.command(&req));
        let msg: SharedString = r.err().map(|e| e.to_string()).unwrap_or_default().into();
        let _ = weak.upgrade_in_event_loop(move |ui| ui.set_message(msg));
    });
}

const SWATCHES: [[u8; 3]; 8] = [
    [255, 32, 32], [255, 128, 0], [255, 220, 0], [32, 255, 64],
    [0, 220, 255], [32, 64, 255], [170, 48, 255], [255, 255, 255],
];

fn load_lights(weak: Weak<AppWindow>) {
    std::thread::spawn(move || {
        let r = kestrel_core::transport::client().and_then(|mut c| c.call(&Request::GetLighting));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if let Ok(Response::Lighting(devs)) = r {
                let first = devs.iter().position(|d| d.supported).unwrap_or(0);
                let items: Vec<LightItem> = devs
                    .into_iter()
                    .map(|d| LightItem { id: d.id.into(), name: d.name.into(), kind: d.kind.into(), supported: d.supported })
                    .collect();
                ui.set_lights(ModelRc::new(VecModel::from(items)));
                ui.set_light_sel(first as i32);
            }
        });
    });
}

fn show_rules(ui: &AppWindow, rules: &[AppRule]) {
    let items: Vec<SharedString> = rules.iter().map(|r| format!("{}  →  {}", r.exe, r.scenario.label()).into()).collect();
    ui.set_rules(ModelRc::new(VecModel::from(items)));
}

fn load_lid(weak: Weak<AppWindow>) {
    std::thread::spawn(move || {
        let lid = kestrel_core::power::lid_actions();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let (ac, dc) = lid.as_ref().map_or((-1, -1), |&(a, d)| (i32::from(a), i32::from(d)));
            ui.set_lid_ac(ac);
            ui.set_lid_dc(dc);
            ui.set_lid_error(lid.err().unwrap_or_default().into());
        });
    });
}

static DRIVER_IDS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn driver_call(weak: Weak<AppWindow>, req: Request) {
    if let Some(ui) = weak.upgrade() {
        ui.set_driver_busy(true);
        ui.set_driver_status(if matches!(req, Request::GetDriverUpdates) { "Searching Windows Update…" } else { "Downloading and installing…" }.into());
    }
    std::thread::spawn(move || {
        let r = kestrel_core::transport::client().and_then(|mut c| c.call(&req));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_driver_busy(false);
            let status = match r {
                Ok(Response::Drivers(list)) => {
                    let titles: Vec<SharedString> = list.iter().map(|d| d.title.clone().into()).collect();
                    *DRIVER_IDS.lock().unwrap_or_else(|p| p.into_inner()) = list.into_iter().map(|d| d.id).collect();
                    ui.set_drivers(ModelRc::new(VecModel::from(titles.clone())));
                    if titles.is_empty() { "All drivers are up to date.".to_string() } else { format!("{} update(s) available.", titles.len()) }
                }
                Ok(Response::Installed { reboot, failed }) => {
                    DRIVER_IDS.lock().unwrap_or_else(|p| p.into_inner()).clear();
                    ui.set_drivers(ModelRc::default());
                    let done = if failed {
                        "Some drivers did not install. Click Check for updates to see which are left."
                    } else {
                        "Installed."
                    };
                    if reboot { format!("{done} Restart Windows to finish.") } else { done.to_string() }
                }
                Ok(Response::Error(e)) => e,
                Ok(_) => "Unexpected reply from the service.".into(),
                Err(e) => e.to_string(),
            };
            ui.set_driver_status(status.into());
        });
    });
}

fn load_gpu(weak: Weak<AppWindow>) {
    std::thread::spawn(move || {
        let r = kestrel_core::gpu::query();
        let _ = weak.upgrade_in_event_loop(move |ui| match r {
            Ok(g) => {
                let opt = |v: Option<u32>, unit: &str| v.map_or("–".to_string(), |v| format!("{v} {unit}"));
                let power = g.power_w.map_or("–".to_string(), |w| format!("{w:.1} W"));
                ui.set_gpu_stats(
                    format!(
                        "{} · {} · {} load · core {} · memory {} · {}",
                        opt(g.temp_c, "°C"), power, opt(g.load_pct, "%"), opt(g.core_mhz, "MHz"), opt(g.mem_mhz, "MHz"), g.pstate
                    )
                    .into(),
                );
                ui.set_gpu_name(g.name.into());
                ui.set_gpu_driver(g.driver.into());
                ui.set_gpu_error(SharedString::default());
            }
            Err(e) => ui.set_gpu_error(e.into()),
        });
    });
}

static CURVE_GEN: AtomicU64 = AtomicU64::new(0);

/// Only the newest load for the selected fan may fill the editor; Apply stays disabled
/// (`curve-ready`) until it does, so one fan's speeds can never be written to the other.
fn load_curve(weak: Weak<AppWindow>, gpu: bool) {
    let generation = CURVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let r = kestrel_core::transport::client().and_then(|mut c| c.call(&Request::GetFanCurve { gpu }));
        let _ = weak.upgrade_in_event_loop(move |ui| match r {
            _ if CURVE_GEN.load(Ordering::SeqCst) != generation || ui.get_curve_gpu() != gpu => {}
            Ok(Response::FanCurve { temps, speeds }) => {
                show_curve(&ui, &temps, &speeds);
                ui.set_curve_ready(true);
            }
            Ok(Response::Error(e)) => ui.set_message(e.into()),
            Ok(_) => {}
            Err(e) => ui.set_message(e.to_string().into()),
        });
    });
}

/// x of each point as a fraction of the graph width: temperature on a 20–100 °C axis,
/// or evenly spaced if the firmware's temperatures are missing.
fn curve_xs(temps: &[i32], n: usize) -> Vec<f32> {
    if temps.len() == n && n > 0 {
        temps.iter().map(|&t| ((t as f32 - 20.0) / 80.0).clamp(0.03, 0.97)).collect()
    } else {
        (0..n).map(|i| (i as f32 + 0.5) / n.max(1) as f32).collect()
    }
}

/// Graph size in pixels; Slint keeps a path's aspect ratio, so paths are drawn in pixels.
static GRAPH: std::sync::Mutex<(f32, f32)> = std::sync::Mutex::new((100.0, 100.0));

/// SVG paths in a w×h viewbox: the curve held flat to both edges, and the area under it.
fn curve_paths(xs: &[f32], speeds: &[i32]) -> (String, String) {
    if xs.is_empty() || xs.len() != speeds.len() {
        return (String::new(), String::new());
    }
    let (w, h) = *GRAPH.lock().unwrap_or_else(|p| p.into_inner());
    let pt = |x: f32, v: i32| format!("{:.2} {:.2}", x * w, h * (1.0 - v.clamp(0, 100) as f32 / 100.0));
    let mut line = format!("M {}", pt(0.0, speeds[0]));
    for (x, v) in xs.iter().zip(speeds) {
        line += &format!(" L {}", pt(*x, *v));
    }
    line += &format!(" L {}", pt(1.0, speeds[speeds.len() - 1]));
    let fill = format!("{line} L {w:.2} {h:.2} L 0 {h:.2} Z");
    (line, fill)
}

fn show_curve(ui: &AppWindow, temps: &[u8], speeds: &[u8]) {
    let temps: Vec<i32> = temps.iter().map(|&t| i32::from(t)).collect();
    let speeds: Vec<i32> = speeds.iter().map(|&v| i32::from(v)).collect();
    let xs = curve_xs(&temps, speeds.len());
    let (line, fill) = curve_paths(&xs, &speeds);
    ui.set_curve_temps(ModelRc::new(VecModel::from(temps)));
    ui.set_curve_speeds(ModelRc::new(VecModel::from(speeds)));
    ui.set_curve_xs(ModelRc::new(VecModel::from(xs)));
    ui.set_curve_line(line.into());
    ui.set_curve_fill(fill.into());
}

/// Moves point `i` to `v` and pushes its neighbours so the curve stays non-decreasing.
fn drag_point(speeds: &mut [i32], i: usize, v: i32) {
    speeds[i] = v;
    for s in &mut speeds[i + 1..] {
        *s = (*s).max(v);
    }
    for s in &mut speeds[..i] {
        *s = (*s).min(v);
    }
}

static DRAG_I: AtomicU64 = AtomicU64::new(0);

/// Clamp to 0-100 and make the curve non-decreasing, as the service requires.
fn monotonic(speeds: impl Iterator<Item = i32>) -> Vec<u8> {
    let mut floor = 0u8;
    speeds
        .map(|v| {
            floor = floor.max(v.clamp(0, 100) as u8);
            floor
        })
        .collect()
}

/// Scenarios the UI lists; UI indices refer to this, not to `Scenario::ALL`.
static SHOWN: std::sync::Mutex<Vec<Scenario>> = std::sync::Mutex::new(Vec::new());

fn shown_index(s: Option<Scenario>) -> i32 {
    let shown = SHOWN.lock().unwrap_or_else(|p| p.into_inner());
    s.and_then(|s| shown.iter().position(|&x| x == s)).map_or(-1, |i| i as i32)
}

fn shown_at(i: i32) -> Option<Scenario> {
    let shown = SHOWN.lock().unwrap_or_else(|p| p.into_inner());
    usize::try_from(i).ok().and_then(|i| shown.get(i).copied())
}

fn show_scenarios(ui: &AppWindow, list: Vec<Scenario>) {
    let items: Vec<ScenarioItem> = list.iter().map(|&s| ScenarioItem { label: s.label().into(), hint: hint(s).into() }).collect();
    *SHOWN.lock().unwrap_or_else(|p| p.into_inner()) = list;
    ui.set_scenarios(ModelRc::new(VecModel::from(items)));
    let auto = Automation::load();
    ui.set_auto_bat(shown_index(auto.battery_scenario));
    ui.set_auto_ac(shown_index(auto.ac_scenario));
}

static CURVE_POINTS: AtomicU64 = AtomicU64::new(0);
static CPU_OK: AtomicBool = AtomicBool::new(false);
static CPU_LOADED: AtomicBool = AtomicBool::new(false);
/// Limits applied from the CPU card this session; "Save current" stores them in the profile.
static LAST_CPU: std::sync::Mutex<Option<(u32, u32)>> = std::sync::Mutex::new(None);

fn load_cpu_power(weak: Weak<AppWindow>) {
    std::thread::spawn(move || {
        let r = kestrel_core::transport::client().and_then(|mut c| c.call(&Request::GetCpuPower));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let Ok(Response::CpuPower(i)) = r else { return };
            CPU_OK.store(i.supported, Ordering::Relaxed);
            ui.set_cpu_ok(i.supported);
            ui.set_cpu_name(i.cpu.into());
            ui.set_cpu_note(i.note.into());
            if let (Some(s), Some(b)) = (i.sustained_w, i.boost_w) {
                ui.set_cpu_sustained(s as i32);
                ui.set_cpu_boost(b as i32);
            }
            show_profiles(&ui);
        });
    });
}

fn cpu_command(weak: Weak<AppWindow>, req: Request, applied: Option<(u32, u32)>) {
    std::thread::spawn(move || {
        let r = kestrel_core::transport::client().and_then(|mut c| c.command(&req));
        let _ = weak.upgrade_in_event_loop(move |ui| match r {
            Ok(()) => {
                *LAST_CPU.lock().unwrap_or_else(|p| p.into_inner()) = applied;
                ui.set_message(SharedString::new());
                load_cpu_power(ui.as_weak());
            }
            Err(e) => ui.set_message(e.to_string().into()),
        });
    });
}
/// Profiles as listed: presets for this model first, then the user's own.
static PROFILES: std::sync::Mutex<Vec<(Profile, bool)>> = std::sync::Mutex::new(Vec::new());

fn show_profiles(ui: &AppWindow) {
    let shown = SHOWN.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let points = CURVE_POINTS.load(Ordering::Relaxed) as usize;
    let mut list: Vec<(Profile, bool)> = profiles::presets(&shown, points, CPU_OK.load(Ordering::Relaxed)).into_iter().map(|p| (p, true)).collect();
    list.extend(profiles::load().into_iter().filter(|p| shown.contains(&p.scenario)).map(|p| (p, false)));
    let items: Vec<ProfileItem> = list
        .iter()
        .map(|(p, preset)| ProfileItem { name: p.name.clone().into(), desc: profiles::describe(p).into(), preset: *preset })
        .collect();
    *PROFILES.lock().unwrap_or_else(|p| p.into_inner()) = list;
    ui.set_profiles(ModelRc::new(VecModel::from(items)));
}

fn save_profile(weak: Weak<AppWindow>, name: String, curves: bool) {
    std::thread::spawn(move || {
        let r = (|| -> std::io::Result<Profile> {
            let mut c = kestrel_core::transport::client()?;
            let st = c.status()?;
            let scenario = st.scenario.ok_or_else(|| std::io::Error::other("Choose a scenario first, then save the profile."))?;
            let curve = |c: &mut kestrel_core::ipc::Client<kestrel_core::transport::Stream>, gpu| match c.call(&Request::GetFanCurve { gpu })? {
                Response::FanCurve { speeds, .. } => Ok(Some(speeds)),
                Response::Error(e) => Err(std::io::Error::other(e)),
                other => Err(std::io::Error::other(format!("unexpected reply: {other:?}"))),
            };
            let (cpu_curve, gpu_curve) = if curves && st.caps.fan_curve { (curve(&mut c, false)?, curve(&mut c, true)?) } else { (None, None) };
            Ok(Profile {
                name,
                scenario,
                cooler_boost: st.cooler_boost.unwrap_or(false),
                cpu_curve,
                gpu_curve,
                cpu_power: *LAST_CPU.lock().unwrap_or_else(|p| p.into_inner()),
            })
        })();
        let _ = weak.upgrade_in_event_loop(move |ui| match r {
            Ok(p) => {
                let mut list = profiles::load();
                list.retain(|x| x.name != p.name);
                list.push(p);
                match profiles::save(&list) {
                    Ok(()) => {
                        ui.set_message(SharedString::new());
                        ui.set_profile_name(SharedString::new());
                    }
                    Err(e) => ui.set_message(e.to_string().into()),
                }
                show_profiles(&ui);
            }
            Err(e) => ui.set_message(e.to_string().into()),
        });
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    show_scenarios(&ui, Scenario::ALL.to_vec());
    ui.set_track(arc(1.0));

    let w = ui.as_weak();
    ui.on_set_scenario(move |i| {
        if let Some(s) = shown_at(i) {
            send(w.clone(), Request::SetScenario(s));
        }
    });
    let w = ui.as_weak();
    ui.on_set_boost(move |on| send(w.clone(), Request::SetCoolerBoost(on)));
    let w = ui.as_weak();
    ui.on_set_charge(move |p| send(w.clone(), Request::SetChargeLimit(p.clamp(10, 100) as u8)));
    let w = ui.as_weak();
    ui.on_set_kbd(move |l| send(w.clone(), Request::SetKbdBacklight(l.clamp(0, 3) as u8)));
    ui.set_swatches(ModelRc::new(VecModel::from(
        SWATCHES.iter().map(|&[r, g, b]| slint::Color::from_rgb_u8(r, g, b)).collect::<Vec<_>>(),
    )));
    let w = ui.as_weak();
    ui.on_load_lights(move || load_lights(w.clone()));
    let w = ui.as_weak();
    ui.on_set_light(move |id, swatch, bright| {
        let color = SWATCHES[(swatch.max(0) as usize).min(SWATCHES.len() - 1)];
        send(w.clone(), Request::SetLighting { device: id.to_string(), color, brightness: bright.clamp(0, 100) as u8 });
    });
    load_lights(ui.as_weak());
    let auto = Automation::load();
    ui.set_auto_60hz(auto.battery_60hz);
    let w = ui.as_weak();
    ui.on_set_auto(move |bat, ac, hz60| {
        let a = Automation { battery_scenario: shown_at(bat), ac_scenario: shown_at(ac), battery_60hz: hz60, ..Automation::load() };
        let msg: SharedString = a.save().err().map(|e| format!("Could not save automation: {e}")).unwrap_or_default().into();
        if let Some(ui) = w.upgrade() {
            ui.set_message(msg);
        }
    });
    let w = ui.as_weak();
    ui.on_set_lid(move |ac, v| {
        let w = w.clone();
        std::thread::spawn(move || {
            let r = kestrel_core::power::set_lid_action(ac, v.clamp(0, 3) as u8);
            let msg: SharedString = r.err().unwrap_or_default().into();
            let w2 = w.clone();
            let _ = w.upgrade_in_event_loop(move |ui| ui.set_message(msg));
            load_lid(w2);
        });
    });
    load_lid(ui.as_weak());
    show_rules(&ui, &auto.app_rules);
    let w = ui.as_weak();
    ui.on_add_rule(move |exe, scenario| {
        let Some(ui) = w.upgrade() else { return };
        let exe = exe.trim().to_ascii_lowercase();
        let exe = if exe.ends_with(".exe") { exe } else { format!("{exe}.exe") };
        let Some(scenario) = shown_at(scenario) else { return };
        if exe == ".exe" {
            return;
        }
        let mut a = Automation::load();
        a.app_rules.retain(|r| !r.exe.eq_ignore_ascii_case(&exe));
        a.app_rules.push(AppRule { exe, scenario });
        ui.set_message(a.save().err().map(|e| e.to_string()).unwrap_or_default().into());
        show_rules(&ui, &a.app_rules);
    });
    let w = ui.as_weak();
    ui.on_remove_rule(move |i| {
        let Some(ui) = w.upgrade() else { return };
        let mut a = Automation::load();
        if let Ok(i) = usize::try_from(i) {
            if i < a.app_rules.len() {
                a.app_rules.remove(i);
            }
        }
        ui.set_message(a.save().err().map(|e| e.to_string()).unwrap_or_default().into());
        show_rules(&ui, &a.app_rules);
    });
    let w = ui.as_weak();
    ui.on_load_gpu(move || load_gpu(w.clone()));
    ui.on_open_driver_page(|| {
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(kestrel_core::gpu::DRIVER_PAGE).spawn();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("xdg-open").arg(kestrel_core::gpu::DRIVER_PAGE).spawn();
    });
    load_gpu(ui.as_weak());
    let w = ui.as_weak();
    ui.on_check_drivers(move || driver_call(w.clone(), Request::GetDriverUpdates));
    let w = ui.as_weak();
    ui.on_install_drivers(move || {
        let ids = DRIVER_IDS.lock().unwrap_or_else(|p| p.into_inner()).clone();
        driver_call(w.clone(), Request::InstallDriverUpdates(ids));
    });
    let w = ui.as_weak();
    ui.on_restore(move || send(w.clone(), Request::RestoreDefaults));
    let w = ui.as_weak();
    ui.on_load_curve(move |gpu| load_curve(w.clone(), gpu));
    let w = ui.as_weak();
    ui.on_apply_profile(move |i| {
        let Some(p) = usize::try_from(i).ok().and_then(|i| PROFILES.lock().unwrap_or_else(|p| p.into_inner()).get(i).map(|x| x.0.clone())) else { return };
        let w = w.clone();
        std::thread::spawn(move || {
            let r = kestrel_core::transport::client().and_then(|mut c| profiles::requests(&p).iter().try_for_each(|req| c.command(req)));
            let msg: SharedString = r.err().map(|e| format!("{}: {e}", p.name)).unwrap_or_default().into();
            let gpu = w.upgrade().is_some_and(|ui| ui.get_curve_gpu());
            let _ = w.upgrade_in_event_loop(move |ui| ui.set_message(msg));
            load_curve(w, gpu);
        });
    });
    let w = ui.as_weak();
    ui.on_set_cpu_power(move |s, b| {
        let (s, b) = kestrel_core::cpu_power::clamp(s.max(0) as u32, b.max(0) as u32);
        cpu_command(w.clone(), Request::SetCpuPower { sustained_w: s, boost_w: b }, Some((s, b)));
    });
    let w = ui.as_weak();
    ui.on_reset_cpu_power(move || cpu_command(w.clone(), Request::ResetCpuPower, None));
    let w = ui.as_weak();
    ui.on_delete_profile(move |i| {
        let Some(ui) = w.upgrade() else { return };
        let Some((p, false)) = usize::try_from(i).ok().and_then(|i| PROFILES.lock().unwrap_or_else(|p| p.into_inner()).get(i).cloned()) else { return };
        let mut list = profiles::load();
        list.retain(|x| x.name != p.name);
        ui.set_message(profiles::save(&list).err().map(|e| e.to_string()).unwrap_or_default().into());
        show_profiles(&ui);
    });
    let w = ui.as_weak();
    ui.on_save_profile(move |name, curves| {
        let name = name.trim().to_string();
        if !name.is_empty() {
            save_profile(w.clone(), name, curves);
        }
    });
    let w = ui.as_weak();
    ui.on_curve_resize(move |gw, gh| {
        let Some(ui) = w.upgrade() else { return };
        *GRAPH.lock().unwrap_or_else(|p| p.into_inner()) = (gw.max(1.0), gh.max(1.0));
        let xs: Vec<f32> = ui.get_curve_xs().iter().collect();
        let speeds: Vec<i32> = ui.get_curve_speeds().iter().collect();
        let (line, fill) = curve_paths(&xs, &speeds);
        ui.set_curve_line(line.into());
        ui.set_curve_fill(fill.into());
    });
    let w = ui.as_weak();
    ui.on_curve_drag(move |down, fx, fy| {
        let Some(ui) = w.upgrade() else { return };
        let xs: Vec<f32> = ui.get_curve_xs().iter().collect();
        let mut speeds: Vec<i32> = ui.get_curve_speeds().iter().collect();
        if speeds.is_empty() || xs.len() != speeds.len() {
            return;
        }
        if down {
            let near = xs.iter().enumerate().min_by(|a, b| (a.1 - fx).abs().total_cmp(&(b.1 - fx).abs())).map_or(0, |(i, _)| i);
            DRAG_I.store(near as u64, Ordering::Relaxed);
        }
        let i = (DRAG_I.load(Ordering::Relaxed) as usize).min(speeds.len() - 1);
        let v = (((1.0 - fy) * 20.0).round() as i32 * 5).clamp(0, 100);
        drag_point(&mut speeds, i, v);
        let (line, fill) = curve_paths(&xs, &speeds);
        ui.set_curve_speeds(ModelRc::new(VecModel::from(speeds)));
        ui.set_curve_line(line.into());
        ui.set_curve_fill(fill.into());
    });
    let w = ui.as_weak();
    ui.on_apply_curve(move |gpu, m| {
        let speeds = monotonic(m.iter());
        let w = w.clone();
        if let Some(ui) = w.upgrade() {
            let temps: Vec<u8> = ui.get_curve_temps().iter().map(|t| t.clamp(0, 255) as u8).collect();
            show_curve(&ui, &temps, &speeds);
        }
        std::thread::spawn(move || {
            let r = kestrel_core::transport::client().and_then(|mut c| c.command(&Request::SetFanCurve { gpu, speeds }));
            let msg: SharedString = r.err().map(|e| e.to_string()).unwrap_or_default().into();
            let _ = w.upgrade_in_event_loop(move |ui| ui.set_message(msg));
            load_curve(w, gpu);
        });
    });

    let w = ui.as_weak();
    std::thread::Builder::new().name("poll".into()).stack_size(256 * 1024).spawn(move || poll(w)).ok();
    ui.show()?;
    #[cfg(windows)]
    win::style_title_bar(&ui);
    slint::run_event_loop()?;
    ui.hide()
}

#[cfg(windows)]
mod win {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWCP_ROUND: u32 = 2;
    const CAPTION_BGR: u32 = 0x0011_0d0c; // Theme.bg #0c0d11

    /// Dark, rounded Windows 11 title bar in the app's background colour. No-op on older Windows.
    pub fn style_title_bar(ui: &super::AppWindow) {
        use slint::ComponentHandle;
        let handle = ui.window().window_handle();
        let Ok(h) = handle.window_handle() else { return };
        let RawWindowHandle::Win32(h) = h.as_raw() else { return };
        let hwnd = h.hwnd.get() as _;
        for (attr, val) in [
            (DWMWA_USE_IMMERSIVE_DARK_MODE, 1u32),
            (DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND),
            (DWMWA_CAPTION_COLOR, CAPTION_BGR),
        ] {
            unsafe { DwmSetWindowAttribute(hwnd, attr, &val as *const u32 as _, 4) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_keeps_curve_rising() {
        let mut s = vec![20, 30, 40, 50, 60, 70];
        drag_point(&mut s, 2, 80);
        assert_eq!(s, vec![20, 30, 80, 80, 80, 80]);
        drag_point(&mut s, 4, 10);
        assert_eq!(s, vec![10, 10, 10, 10, 10, 80]);
    }

    #[test]
    fn paths_span_the_graph() {
        let xs = curve_xs(&[40, 60], 2);
        assert_eq!(xs, vec![0.25, 0.5]);
        let (line, fill) = curve_paths(&xs, &[30, 90]);
        assert_eq!(line, "M 0.00 70.00 L 25.00 70.00 L 50.00 10.00 L 100.00 10.00");
        assert!(fill.ends_with("L 100.00 100.00 L 0 100.00 Z"));
    }
}
