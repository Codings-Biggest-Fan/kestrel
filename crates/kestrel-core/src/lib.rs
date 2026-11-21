//! Shared code for the Kestrel service, tray and UI.

pub mod automation;
pub mod creative;
pub mod cpu_power;
pub mod ec;
pub mod fan;
pub mod gigabyte;
pub mod gpu;
pub mod ipc;
pub mod journal;
pub mod laptop;
pub mod model;
pub mod power;
pub mod profiles;
pub mod scenario;
pub mod transport;

pub const APP_NAME: &str = "Kestrel";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
