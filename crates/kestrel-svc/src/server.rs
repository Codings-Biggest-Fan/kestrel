//! IPC server: one thread per client, all hardware access behind one lock.

use crate::sysinfo::Sampler;
use kestrel_core::ec::MsiEc;
use kestrel_core::ipc::{read_frame, write_frame, Capabilities, ModelInfo, Request, Response, Status, PROTOCOL_VERSION};
use kestrel_core::scenario::Scenario;
use kestrel_core::transport::{Listener, Stream};
use std::sync::{Arc, Mutex};

pub struct State {
    pub ec: Option<MsiEc>,
    pub model: ModelInfo,
    pub scenario: Option<Scenario>,
    pub sampler: Sampler,
}

pub type Shared = Arc<Mutex<State>>;

pub fn serve(state: Shared) -> std::io::Result<()> {
    let mut listener = Listener::bind()?;
    loop {
        match listener.accept() {
            Ok(stream) => {
                let st = state.clone();
                std::thread::Builder::new()
                    .name("kestrel-client".into())
                    .stack_size(128 * 1024)
                    .spawn(move || handle(stream, st))?;
            }
            Err(e) => {
                eprintln!("accept failed: {e}");
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}

fn handle(mut stream: Stream, state: Shared) {
    match read_frame::<_, Request>(&mut stream) {
        Ok(Request::Hello { .. }) => {
            let hello = Response::Hello { protocol: PROTOCOL_VERSION, version: kestrel_core::VERSION.into() };
            if write_frame(&mut stream, &hello).is_err() {
                return;
            }
        }
        _ => return,
    }
    while let Ok(req) = read_frame::<_, Request>(&mut stream) {
        let resp = match req {
            Request::GetDriverUpdates => match crate::drivers::list() {
                Ok(d) => Response::Drivers(d),
                Err(e) => Response::Error(e),
            },
            Request::InstallDriverUpdates(ids) => {
                let r = crate::drivers::install(&ids);
                eprintln!("InstallDriverUpdates {ids:?} -> {r:?}");
                match r {
                    Ok((reboot, failed)) => Response::Installed { reboot, failed },
                    Err(e) => Response::Error(e),
                }
            }
            req => {
                let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                dispatch(&mut st, req)
            }
        };
        if write_frame(&mut stream, &resp).is_err() {
            break;
        }
    }
}

fn result(r: Result<(), String>) -> Response {
    match r {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error(e),
    }
}

/// Every hardware-changing request is logged with its result and the control registers
/// afterwards, so a console run on a real laptop shows exactly what the EC accepted.
pub fn dispatch(st: &mut State, req: Request) -> Response {
    if matches!(req, Request::GetStatus | Request::Hello { .. } | Request::GetFanCurve { .. } | Request::GetLighting) {
        return dispatch_inner(st, req);
    }
    let desc = format!("{req:?}");
    let resp = dispatch_inner(st, req);
    let regs = st.ec.as_mut().map(|ec| {
        let c = ec.conf().clone();
        let addrs = [c.cooler_boost.and_then(|f| f.address), c.shift_mode.and_then(|m| m.address), c.fan_mode.and_then(|m| m.address)];
        let mut line = addrs.into_iter().flatten().map(|a| format!("0x{a:02x}={}", ec.peek(a).map_or("??".into(), |v| format!("0x{v:02x}")))).collect::<Vec<_>>().join(" ");
        // Bytes just before each speed table hold the duty the EC is currently applying.
        if let Some(fc) = c.fan_curve {
            let mut duty = |a: u8| ec.peek(a).map_or("??".into(), |v| v.to_string());
            let (cd, gd) = (duty(fc.cpu_speed_base - 1), duty(fc.gpu_speed_base - 1));
            line += &format!(" duty cpu={cd}% gpu={gd}%");
        }
        if let Ok([cpu, gpu]) = ec.fan_rpm() {
            line += &format!(" rpm cpu={} gpu={}", cpu.unwrap_or(0), gpu.unwrap_or(0));
        }
        line
    });
    eprintln!("{desc} -> {resp:?} | {}", regs.unwrap_or_default());
    resp
}

fn dispatch_inner(st: &mut State, req: Request) -> Response {
    match req {
        Request::Hello { .. } => Response::Hello { protocol: PROTOCOL_VERSION, version: kestrel_core::VERSION.into() },
        Request::GetStatus => Response::Status(Box::new(status(st))),
        Request::SetScenario(s) => {
            let r = apply_scenario(st, s);
            if r.is_ok() {
                st.scenario = Some(s);
            }
            result(r)
        }
        Request::SetCoolerBoost(on) => result(with_ec(st, |ec| ec.set_cooler_boost(on))),
        Request::SetChargeLimit(p) => result(with_ec(st, |ec| ec.set_charge_limit(p))),
        Request::GetLighting => Response::Lighting(crate::lighting::list()),
        Request::GetDriverUpdates | Request::InstallDriverUpdates(_) => Response::Error("driver requests are handled per connection".into()),
        Request::SetLighting { device, color, brightness } => match crate::lighting::set(&device, color, brightness) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error(e),
        },
        Request::SetKbdBacklight(l) => result(with_ec(st, |ec| ec.set_kbd_backlight(l))),
        Request::GetFanCurve { gpu } => match st.ec.as_mut().map(|ec| ec.fan_curve(gpu)) {
            Some(Ok((temps, speeds))) => Response::FanCurve { temps, speeds },
            Some(Err(e)) => Response::Error(e.to_string()),
            None => Response::Error("no supported EC on this laptop".into()),
        },
        Request::SetFanCurve { gpu, speeds } => {
            let extreme = st.ec.as_ref().and_then(|ec| ec.conf().fan_curve).is_some_and(|f| f.requires_extreme);
            let r = with_ec(st, |ec| {
                ec.set_fan_curve_speeds(gpu, &speeds)?;
                if extreme {
                    ec.set_shift_mode("turbo")?;
                }
                if ec.conf().fan_mode.as_ref().is_some_and(|m| m.modes.contains_key("advanced")) {
                    ec.set_fan_mode("advanced")?;
                }
                Ok(())
            });
            if r.is_ok() && extreme {
                st.scenario = Some(Scenario::Extreme);
            }
            result(r)
        }
        Request::RestoreDefaults => {
            st.scenario = None;
            result(with_ec(st, |ec| ec.restore().map(|_| ())))
        }
    }
}

fn with_ec(st: &mut State, f: impl FnOnce(&mut MsiEc) -> kestrel_core::ec::Result<()>) -> Result<(), String> {
    let ec = st.ec.as_mut().ok_or("no supported EC on this laptop")?;
    f(ec).map_err(|e| e.to_string())
}

fn apply_scenario(st: &mut State, s: Scenario) -> Result<(), String> {
    with_ec(st, |ec| {
        if !Scenario::available(ec.conf()).contains(&s) {
            return Err(kestrel_core::ec::EcError::Unsupported(s.label()));
        }
        let shift = ec.conf().shift_mode.clone();
        let fan = ec.conf().fan_mode.clone();
        if let Some(name) = shift.and_then(|m| s.shift_modes().iter().find(|n| m.modes.contains_key(**n)).copied()) {
            ec.set_shift_mode(name)?;
        }
        if let Some(name) = fan.and_then(|m| s.fan_modes().iter().find(|n| m.modes.contains_key(**n)).copied()) {
            ec.set_fan_mode(name)?;
        }
        if ec.conf().super_battery.and_then(|f| f.address).is_some() {
            ec.set_super_battery(s.super_battery())?;
        }
        Ok(())
    })
}

fn status(st: &mut State) -> Status {
    let sys = st.sampler.sample();
    let mut s = Status {
        model: st.model.clone(),
        scenario: st.scenario,
        cpu_load_pct: sys.cpu_load_pct,
        ram_used_mb: sys.ram_used_mb,
        ram_total_mb: sys.ram_total_mb,
        on_ac: sys.on_ac,
        battery_pct: sys.battery_pct,
        ..Default::default()
    };
    if let Some(ec) = st.ec.as_mut() {
        let c = ec.conf();
        s.caps = Capabilities {
            cooler_boost: c.cooler_boost.and_then(|f| f.address).is_some(),
            charge_limit: c.charge_control_address.is_some(),
            kbd_backlight: c.kbd_bl.as_ref().and_then(|k| k.bl_state_address).is_some(),
            super_battery: c.super_battery.and_then(|f| f.address).is_some(),
            shift_modes: c.shift_mode.as_ref().map(|m| m.modes.keys().cloned().collect()).unwrap_or_default(),
            fan_curve: c.fan_curve.is_some(),
            scenarios: Scenario::available(c),
            curve_points: c.fan_curve.map_or(0, |f| f.points),
        };
        s.cooler_boost = ec.cooler_boost().ok();
        s.charge_limit = ec.charge_limit().ok().flatten();
        s.kbd_backlight = ec.kbd_backlight().ok();
        if let Ok([ct, cf, gt, gf]) = ec.sensors() {
            (s.cpu_temp, s.cpu_fan_pct, s.gpu_temp, s.gpu_fan_pct) = (ct, cf, gt, gf);
        }
        if let Ok([c, g]) = ec.fan_rpm() {
            (s.cpu_fan_rpm, s.gpu_fan_rpm) = (c, g);
        }
    }
    s
}
