//! IPC server: one thread per client, all hardware access behind one lock.

use crate::sysinfo::Sampler;
use kestrel_core::creative::SoundBlaster;
use kestrel_core::ipc::{
    read_frame, write_frame, ModelInfo, Request, Response, Status, PROTOCOL_VERSION,
};
use kestrel_core::laptop::Laptop;
use kestrel_core::scenario::Scenario;
use kestrel_core::transport::{Listener, Stream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct State {
    pub ec: Option<Box<dyn Laptop>>,
    pub model: ModelInfo,
    pub scenario: Option<Scenario>,
    pub sampler: Sampler,
    pub audio: Option<Audio>,
}

pub struct Audio {
    pub dev: SoundBlaster,
    pub refreshed: Instant,
}

impl Audio {
    pub fn new(dev: SoundBlaster) -> Self {
        Self {
            dev,
            refreshed: Instant::now(),
        }
    }
}

/// The G6's front knob and output button change settings behind our back.
const AUDIO_REFRESH: Duration = Duration::from_secs(5);

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
            let hello = Response::Hello {
                protocol: PROTOCOL_VERSION,
                version: kestrel_core::VERSION.into(),
            };
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
    if matches!(
        req,
        Request::GetStatus
            | Request::Hello { .. }
            | Request::GetFanCurve { .. }
            | Request::GetLighting
    ) {
        return dispatch_inner(st, req);
    }
    let desc = format!("{req:?}");
    let resp = dispatch_inner(st, req);
    let regs = st.ec.as_mut().map(|ec| ec.debug_line());
    eprintln!("{desc} -> {resp:?} | {}", regs.unwrap_or_default());
    resp
}

fn dispatch_inner(st: &mut State, req: Request) -> Response {
    match req {
        Request::Hello { .. } => Response::Hello {
            protocol: PROTOCOL_VERSION,
            version: kestrel_core::VERSION.into(),
        },
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
        Request::GetDriverUpdates | Request::InstallDriverUpdates(_) => {
            Response::Error("driver requests are handled per connection".into())
        }
        Request::SetLighting {
            device,
            color,
            brightness,
        } => match crate::lighting::set(&device, color, brightness) {
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
            let r = st
                .ec
                .as_mut()
                .ok_or_else(|| "no supported EC on this laptop".to_string())
                .and_then(|ec| ec.set_fan_curve(gpu, &speeds).map_err(|e| e.to_string()));
            if let Ok(Some(s)) = r {
                st.scenario = Some(s);
            }
            result(r.map(|_| ()))
        }
        Request::SetAudio(a) => match st.audio.as_mut() {
            Some(au) => result(au.dev.set(a).map_err(|e| e.to_string())),
            None => Response::Error("no Sound Blaster connected".into()),
        },
        Request::RestoreDefaults => {
            st.scenario = None;
            result(with_ec(st, |ec| ec.restore().map(|_| ())))
        }
    }
}

fn with_ec(
    st: &mut State,
    f: impl FnOnce(&mut dyn Laptop) -> kestrel_core::ec::Result<()>,
) -> Result<(), String> {
    let ec = st.ec.as_mut().ok_or("no supported EC on this laptop")?;
    f(ec.as_mut()).map_err(|e| e.to_string())
}

fn apply_scenario(st: &mut State, s: Scenario) -> Result<(), String> {
    with_ec(st, |ec| ec.apply_scenario(s))
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
    if let Some(au) = st.audio.as_mut() {
        if au.refreshed.elapsed() >= AUDIO_REFRESH {
            au.dev.refresh();
            au.refreshed = Instant::now();
        }
        s.audio = Some(au.dev.state().clone());
    }
    if let Some(ec) = st.ec.as_mut() {
        s.caps = ec.caps();
        s.cooler_boost = ec.cooler_boost().ok();
        s.charge_limit = ec.charge_limit().ok().flatten();
        s.kbd_backlight = ec.kbd_backlight().ok();
        let r = ec.readings();
        (s.cpu_temp, s.cpu_fan_pct, s.gpu_temp, s.gpu_fan_pct) =
            (r.cpu_temp, r.cpu_fan_pct, r.gpu_temp, r.gpu_fan_pct);
        (s.cpu_fan_rpm, s.gpu_fan_rpm) = (r.cpu_fan_rpm, r.gpu_fan_rpm);
    }
    s
}
