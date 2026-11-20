//! Versioned request/response protocol between kestrel-svc and its clients.
//! Frames are a little-endian u32 length followed by a JSON message.

use crate::model::WmiGen;
use crate::scenario::Scenario;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Bump on any incompatible change to [`Request`] or [`Response`].
pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Hello { protocol: u32 },
    GetStatus,
    SetScenario(Scenario),
    SetCoolerBoost(bool),
    SetChargeLimit(u8),
    SetKbdBacklight(u8),
    GetFanCurve { gpu: bool },
    /// Speeds only, one per curve point; also switches the EC to its advanced (curve) fan mode.
    SetFanCurve { gpu: bool, speeds: Vec<u8> },
    /// Put every register Kestrel changed back to its original value.
    RestoreDefaults,
    GetLighting,
    /// Static colour on a USB RGB device; `brightness` 0–100, 0 turns it off.
    SetLighting { device: String, color: [u8; 3], brightness: u8 },
    /// Driver updates Windows Update offers for this PC.
    GetDriverUpdates,
    /// Download and install the driver updates with these Windows Update IDs.
    InstallDriverUpdates(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Hello { protocol: u32, version: String },
    Status(Box<Status>),
    FanCurve { temps: Vec<u8>, speeds: Vec<u8> },
    Lighting(Vec<LightDevice>),
    Drivers(Vec<DriverUpdate>),
    /// `failed`: Windows Update installed only some of them.
    Installed { reboot: bool, failed: bool },
    Ok,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverUpdate {
    pub id: String,
    pub title: String,
    /// Windows driver class, e.g. "Net", "Bluetooth", "Display", "System".
    pub class: String,
    pub manufacturer: String,
}

/// A USB RGB device. `id` is `vid:pid` in hex; unsupported devices are listed so owners can report them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightDevice {
    pub id: String,
    pub name: String,
    /// "keyboard", "lightbar", "mouse" or "unknown".
    pub kind: String,
    pub supported: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub firmware: String,
    pub name: String,
    pub map_id: Option<String>,
    pub wmi: Option<WmiGen>,
    /// Unknown models and models without a verified map never get EC writes.
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub cooler_boost: bool,
    pub charge_limit: bool,
    pub kbd_backlight: bool,
    pub super_battery: bool,
    pub shift_modes: Vec<String>,
    #[serde(default)]
    pub fan_curve: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub model: ModelInfo,
    pub caps: Capabilities,
    pub scenario: Option<Scenario>,
    pub cooler_boost: Option<bool>,
    pub charge_limit: Option<u8>,
    pub kbd_backlight: Option<u8>,
    pub cpu_temp: Option<u8>,
    pub cpu_fan_pct: Option<u8>,
    pub gpu_temp: Option<u8>,
    pub gpu_fan_pct: Option<u8>,
    pub cpu_fan_rpm: Option<u32>,
    pub gpu_fan_rpm: Option<u32>,
    pub cpu_load_pct: f32,
    pub ram_used_mb: u32,
    pub ram_total_mb: u32,
    pub on_ac: Option<bool>,
    pub battery_pct: Option<u8>,
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A connected, version-checked client.
pub struct Client<S: Read + Write> {
    stream: S,
    pub server_version: String,
}

impl<S: Read + Write> Client<S> {
    pub fn handshake(mut stream: S) -> io::Result<Self> {
        write_frame(&mut stream, &Request::Hello { protocol: PROTOCOL_VERSION })?;
        match read_frame(&mut stream)? {
            Response::Hello { protocol, version } if protocol == PROTOCOL_VERSION => {
                Ok(Self { stream, server_version: version })
            }
            Response::Hello { protocol, version } => Err(io::Error::other(format!(
                "kestrel-svc {version} speaks protocol {protocol}, this app needs {PROTOCOL_VERSION}"
            ))),
            other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
        }
    }

    pub fn call(&mut self, req: &Request) -> io::Result<Response> {
        write_frame(&mut self.stream, req)?;
        read_frame(&mut self.stream)
    }

    pub fn status(&mut self) -> io::Result<Status> {
        match self.call(&Request::GetStatus)? {
            Response::Status(s) => Ok(*s),
            Response::Error(e) => Err(io::Error::other(e)),
            other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
        }
    }

    /// Send a command that answers `Ok`; maps `Error` to an `io::Error`.
    pub fn command(&mut self, req: &Request) -> io::Result<()> {
        match self.call(req)? {
            Response::Ok => Ok(()),
            Response::Error(e) => Err(io::Error::other(e)),
            other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_round_trip() {
        let mut buf = Vec::new();
        let req = Request::SetScenario(Scenario::Silent);
        write_frame(&mut buf, &req).unwrap();
        let back: Request = read_frame(&mut Cursor::new(buf)).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut buf = (u32::MAX).to_le_bytes().to_vec();
        buf.extend_from_slice(b"{}");
        assert!(read_frame::<_, Request>(&mut Cursor::new(buf)).is_err());
    }
}
