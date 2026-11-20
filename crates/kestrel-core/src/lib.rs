//! Shared code for the Kestrel service, tray and UI.

pub mod creative;
pub mod ec;
pub mod fan;
pub mod gigabyte;
pub mod ipc;
pub mod journal;
pub mod laptop;
pub mod model;
pub mod scenario;
pub mod transport;

pub const APP_NAME: &str = "Kestrel";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
