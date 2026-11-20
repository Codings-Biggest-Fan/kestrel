//! Shared code for the Kestrel service, tray and UI.

pub mod automation;
pub mod ec;
pub mod fan;
pub mod gpu;
pub mod ipc;
pub mod journal;
pub mod model;
pub mod power;
pub mod scenario;
pub mod transport;

pub const APP_NAME: &str = "Kestrel";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
