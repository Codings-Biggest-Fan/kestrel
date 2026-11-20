//! kestrel-svc: the only Kestrel process that touches hardware.
//!
//! Usage:
//!   kestrel-svc                    run as a Windows service (started by the SCM)
//!   kestrel-svc --console          run in the foreground
//!   kestrel-svc --mock <FW_ID>     foreground with an in-memory EC (development, CI)
//!   kestrel-svc install|uninstall  register / remove the Windows service

mod server;
mod settings;
mod sysinfo;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod wmi_ec;

use kestrel_core::ec::{read_firmware_id, EcIo, MockEc, MsiEc};
use kestrel_core::ipc::ModelInfo;
use kestrel_core::journal::Journal;
use kestrel_core::model::{ModelDb, Tier};
use server::State;
use std::sync::{Arc, Mutex};

pub const SERVICE_NAME: &str = "kestrel-svc";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("--console") => run(None),
        Some("--mock") => run(Some(args.get(1).cloned().unwrap_or_else(|| "15MMIMS1.103".into()))),
        #[cfg(windows)]
        Some("install") => service::install(),
        #[cfg(windows)]
        Some("uninstall") => service::uninstall(),
        #[cfg(windows)]
        None => service::dispatch(),
        _ => {
            eprintln!("usage: kestrel-svc [--console | --mock <FW_ID> | install | uninstall]");
            Err("bad arguments".into())
        }
    };
    if let Err(e) = code {
        eprintln!("kestrel-svc: {e}");
        std::process::exit(1);
    }
}

pub fn run(mock_fw: Option<String>) -> Result<(), String> {
    let state = Arc::new(Mutex::new(init(mock_fw)));
    {
        let st = state.lock().unwrap();
        eprintln!(
            "kestrel-svc {}: firmware {:?} map {:?} read_only={}",
            kestrel_core::VERSION, st.model.firmware, st.model.map_id, st.model.read_only
        );
    }
    server::serve(state).map_err(|e| e.to_string())
}

fn open_backend(mock_fw: Option<String>) -> Result<(Box<dyn EcIo>, String), String> {
    if let Some(fw) = mock_fw {
        let mut io = MockEc::with_firmware(&fw);
        // Plausible power-on values so mode writes pass the map checks.
        io.regs[0xd2] = 0xc1;
        io.regs[0xd4] = 0x0d;
        io.regs[0xf2] = 0xc1;
        io.regs[0xf4] = 0x0d;
        io.regs[0x68] = 52;
        io.regs[0x80] = 47;
        io.regs[0xc9] = 0xc8;
        io.regs[0xcb] = 0xd2;
        for (i, (t, sp)) in [(50, 0), (58, 30), (65, 45), (72, 60), (80, 75), (88, 90)].into_iter().enumerate() {
            io.regs[0x69 + i] = t;
            io.regs[0x72 + i] = sp;
            io.regs[0x81 + i] = t - 5;
            io.regs[0x8a + i] = sp;
        }
        let id = read_firmware_id(&mut io).map_err(|e| e.to_string())?;
        return Ok((Box::new(io), id));
    }
    #[cfg(windows)]
    {
        let ec = wmi_ec::WmiEc::open().map_err(|e| e.to_string())?;
        let id = ec.firmware_id().map_err(|e| e.to_string())?;
        Ok((Box::new(ec), id))
    }
    #[cfg(not(windows))]
    Err("no EC backend on this platform; use --mock <FW_ID>".into())
}

fn init(mock_fw: Option<String>) -> State {
    let settings = settings::Settings::load();
    let mut model = ModelInfo { read_only: true, ..Default::default() };
    let ec = match open_backend(mock_fw) {
        Ok((io, fw)) => {
            model.firmware = fw.clone();
            let db = ModelDb::embedded();
            match db.find(&fw) {
                Some(m) => {
                    let allowed = m.conf.tier == Tier::Tested || settings.allows_experimental(&m.firmware.prefix);
                    model.name = m.firmware.models.clone();
                    model.map_id = Some(m.conf.id.clone());
                    model.wmi = Some(m.conf.wmi);
                    model.read_only = !allowed;
                    let journal = Journal::open(settings::data_dir().join("journal.json")).unwrap_or_default();
                    Some(MsiEc::new(io, m.conf.clone(), journal, !allowed))
                }
                None => {
                    model.name = "Unknown model".into();
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("EC unavailable: {e}");
            None
        }
    };
    State { ec, model, scenario: None, sampler: Default::default() }
}
