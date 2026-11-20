//! kestrel-ui: main window. Talks to kestrel-svc over IPC and exits when closed.
#![cfg_attr(windows, windows_subsystem = "windows")]

use kestrel_core::ipc::{Request, Status};
use kestrel_core::scenario::Scenario;
use slint::{ModelRc, SharedString, VecModel, Weak};
use std::collections::VecDeque;
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

/// SVG path for values in 0..1 across a 100x100 viewbox; the newest sample is at the right edge.
fn spark(q: &VecDeque<f32>) -> SharedString {
    if q.len() < 2 {
        return SharedString::new();
    }
    let step = 100.0 / (HISTORY - 1) as f32;
    let x0 = 100.0 - step * (q.len() - 1) as f32;
    let mut s = String::with_capacity(q.len() * 14);
    for (i, v) in q.iter().enumerate() {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{}{:.1} {:.1} ", if i == 0 { 'M' } else { 'L' }, x0 + step * i as f32, 100.0 - v * 100.0));
    }
    s.into()
}

fn apply(ui: &AppWindow, s: &Status, temps: &VecDeque<f32>, loads: &VecDeque<f32>) {
    ui.set_connected(true);
    ui.set_model_name(if s.model.name.is_empty() { "MSI laptop".into() } else { s.model.name.clone().into() });
    ui.set_firmware(s.model.firmware.clone().into());
    ui.set_read_only(s.model.read_only);
    ui.set_scenario(s.scenario.map(|x| x.index() as i32).unwrap_or(-1));
    ui.set_cpu_temp(opt(s.cpu_temp, ""));
    ui.set_gpu_temp(opt(s.gpu_temp, ""));
    ui.set_cpu_temp_f(s.cpu_temp.unwrap_or(0) as f32 / 100.0);
    ui.set_gpu_temp_f(s.gpu_temp.unwrap_or(0) as f32 / 100.0);
    ui.set_cpu_fan(fan(s.cpu_fan_rpm, s.cpu_fan_pct));
    ui.set_gpu_fan(fan(s.gpu_fan_rpm, s.gpu_fan_pct));
    ui.set_cpu_load(s.cpu_load_pct);
    ui.set_ram(format!("{:.1} / {:.0} GB", s.ram_used_mb as f32 / 1024.0, s.ram_total_mb as f32 / 1024.0).into());
    ui.set_ram_f(if s.ram_total_mb > 0 { s.ram_used_mb as f32 / s.ram_total_mb as f32 } else { 0.0 });
    ui.set_battery(opt(s.battery_pct, "%"));
    ui.set_power_source(match s.on_ac { Some(true) => "Plugged in", Some(false) => "On battery", None => "Unknown" }.into());
    ui.set_has_boost(s.caps.cooler_boost);
    ui.set_cooler_boost(s.cooler_boost.unwrap_or(false));
    ui.set_has_charge(s.caps.charge_limit);
    ui.set_charge_limit(s.charge_limit.unwrap_or(100) as i32);
    ui.set_has_kbd(s.caps.kbd_backlight);
    ui.set_kbd_level(s.kbd_backlight.map(i32::from).unwrap_or(-1));
    ui.set_temp_history(spark(temps));
    ui.set_cpu_history(spark(loads));
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
            for (q, v) in [(&mut temps, s.cpu_temp.unwrap_or(0) as f32 / 100.0), (&mut loads, s.cpu_load_pct / 100.0)] {
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

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let items: Vec<ScenarioItem> =
        Scenario::ALL.iter().map(|&s| ScenarioItem { label: s.label().into(), hint: hint(s).into() }).collect();
    ui.set_scenarios(ModelRc::new(VecModel::from(items)));

    let w = ui.as_weak();
    ui.on_set_scenario(move |i| {
        if let Some(s) = Scenario::from_index(i as usize) {
            send(w.clone(), Request::SetScenario(s));
        }
    });
    let w = ui.as_weak();
    ui.on_set_boost(move |on| send(w.clone(), Request::SetCoolerBoost(on)));
    let w = ui.as_weak();
    ui.on_set_charge(move |p| send(w.clone(), Request::SetChargeLimit(p.clamp(10, 100) as u8)));
    let w = ui.as_weak();
    ui.on_set_kbd(move |l| send(w.clone(), Request::SetKbdBacklight(l.clamp(0, 3) as u8)));
    let w = ui.as_weak();
    ui.on_restore(move || send(w.clone(), Request::RestoreDefaults));

    let w = ui.as_weak();
    std::thread::Builder::new().name("poll".into()).stack_size(256 * 1024).spawn(move || poll(w)).ok();
    ui.run()
}
