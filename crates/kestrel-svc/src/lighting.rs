//! USB HID RGB devices: SteelSeries-controlled laptop keyboards / light bars and MSI mice.
//! Protocols follow OpenRGB (GPL-2.0-or-later). Only exact VID:PID matches are ever written;
//! other SteelSeries/MSI HID devices are listed as unsupported so owners can report them.

use kestrel_core::ipc::LightDevice;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Proto {
    /// Per-key keyboard (KLC): one 525-byte feature report, 4 bytes per key.
    Klc,
    /// ALC light bar: KLC framing with packet id 0x06.
    Alc,
    /// GE68HX-style light bar: 30 LEDs per feature report, then a commit write.
    KlcLightbar,
    /// MSI Clutch GM11 mouse: 8-byte checksummed feature reports.
    ClutchGm11,
}

struct Known {
    vid: u16,
    pid: u16,
    #[cfg_attr(not(windows), allow(dead_code))]
    interface: i32,
    proto: Proto,
    name: &'static str,
    kind: &'static str,
}

const KNOWN: &[Known] = &[
    Known { vid: 0x1038, pid: 0x1122, interface: 0, proto: Proto::Klc, name: "SteelSeries per-key keyboard", kind: "keyboard" },
    Known { vid: 0x1038, pid: 0x113a, interface: 0, proto: Proto::Klc, name: "SteelSeries per-key keyboard", kind: "keyboard" },
    Known { vid: 0x1038, pid: 0x1161, interface: 0, proto: Proto::Alc, name: "SteelSeries light bar", kind: "lightbar" },
    Known { vid: 0x1038, pid: 0x114b, interface: 0, proto: Proto::KlcLightbar, name: "SteelSeries light bar", kind: "lightbar" },
    Known { vid: 0x0db0, pid: 0x0d11, interface: 2, proto: Proto::ClutchGm11, name: "MSI Clutch GM11", kind: "mouse" },
];

/// Vendors whose unknown HID devices are listed (unsupported) to help add them.
#[cfg_attr(not(windows), allow(dead_code))]
const REPORT_VENDORS: &[u16] = &[0x1038, 0x1462, 0x0db0];

const KLC_KEYS: &[u8] = &[
    0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
    0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x29, 0x2B,
    0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40,
    0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x49, 0x4B, 0x4C, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56,
    0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, 0x62, 0x63, 0x66, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4,
    0xF0, 0x28, 0x2A, 0x31, 0x64, 0xE5, 0xE6,
];

#[derive(Debug, PartialEq)]
pub enum Packet {
    Feature(Vec<u8>),
    Write(Vec<u8>),
}

pub fn device_id(vid: u16, pid: u16) -> String {
    format!("{vid:04x}:{pid:04x}")
}

fn scale(c: [u8; 3], brightness: u8) -> [u8; 3] {
    let b = u16::from(brightness.min(100));
    c.map(|v| (u16::from(v) * b / 100) as u8)
}

/// The exact reports sent for a static colour. `brightness` is 0–100; 0 turns the device off.
pub fn frames(proto: Proto, color: [u8; 3], brightness: u8) -> Vec<Packet> {
    let [r, g, b] = scale(color, brightness);
    match proto {
        Proto::Klc | Proto::Alc => {
            let ids: Vec<u8> = if proto == Proto::Klc { KLC_KEYS.to_vec() } else { (0..4).collect() };
            let mut buf = vec![0u8; 525];
            buf[1] = 0x0c;
            buf[3] = if proto == Proto::Klc { 0x66 } else { 0x06 };
            for slot in 0..130 {
                buf[5 + slot * 4] = 0xff;
            }
            for (i, id) in ids.into_iter().enumerate() {
                buf[5 + i * 4..9 + i * 4].copy_from_slice(&[id, r, g, b]);
            }
            vec![Packet::Feature(buf)]
        }
        Proto::KlcLightbar => {
            let ids: Vec<u8> = (0..53).collect();
            let mut out: Vec<Packet> = ids
                .chunks(30)
                .map(|chunk| {
                    let mut buf = vec![0u8; 525];
                    buf[1] = 0x0e;
                    buf[3] = chunk.len() as u8;
                    for (i, &id) in chunk.iter().enumerate() {
                        let o = 5 + i * 12;
                        buf[o..o + 3].copy_from_slice(&[r, g, b]);
                        buf[o + 9] = 0x01;
                        buf[o + 11] = id;
                    }
                    Packet::Feature(buf)
                })
                .collect();
            let mut commit = vec![0u8; 65];
            commit[1] = 0x0d;
            commit[3] = 0x02;
            out.push(Packet::Write(commit));
            out
        }
        Proto::ClutchGm11 => {
            let pkt = |d: [u8; 7]| {
                let sum = d.iter().fold(0u8, |a, &v| a.wrapping_add(v));
                let mut buf = vec![0u8];
                buf.extend_from_slice(&d);
                buf.push(0xff_u8.wrapping_sub(sum));
                Packet::Feature(buf)
            };
            let handshake = pkt([0x03, 0x03, 0x0f, 0xf2, 0x40, 0x00, 0x00]);
            if brightness == 0 {
                return vec![handshake, pkt([0x05, 0, 0, 0, 0, 0, 0])];
            }
            let mut palette = vec![0u8; 33];
            palette[1..22].copy_from_slice(&[
                0xff, 0, 0, 0xff, 0xff, 0, 0, 0xff, 0, 0, 0xff, 0xff, 0, 0, 0xff, 0xff, 0, 0xff, 0xff, 0xff, 0xff,
            ]);
            palette[22..25].copy_from_slice(&color);
            palette[32] = 0xd0;
            vec![
                handshake,
                pkt([0x85, 0x02, 0, 0, 0, 0, 0]),
                pkt([0x05, 0x02, 0x00, (u16::from(brightness.min(100)) * 255 / 100) as u8, 0, 0, 0]),
                pkt([0x10, 0x00, 0x18, 0, 0, 0, 0]),
                Packet::Write(palette),
                pkt([0x05, 0x02, 0x08, 0x07, 0, 0, 0]),
            ]
        }
    }
}

fn known(vid: u16, pid: u16) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.vid == vid && k.pid == pid)
}

#[cfg(windows)]
mod hid {
    use super::*;
    use hidapi::HidApi;

    pub fn list() -> Vec<LightDevice> {
        let Ok(api) = HidApi::new() else { return Vec::new() };
        let mut out: Vec<LightDevice> = Vec::new();
        for d in api.device_list() {
            let (vid, pid) = (d.vendor_id(), d.product_id());
            if !REPORT_VENDORS.contains(&vid) {
                continue;
            }
            let id = device_id(vid, pid);
            if out.iter().any(|o| o.id == id) {
                continue;
            }
            out.push(match known(vid, pid) {
                Some(k) => LightDevice { id, name: k.name.into(), kind: k.kind.into(), supported: true },
                None => LightDevice {
                    name: d.product_string().unwrap_or("HID device").to_string(),
                    id,
                    kind: "unknown".into(),
                    supported: false,
                },
            });
        }
        out
    }

    pub fn send(vid: u16, pid: u16, k: &Known, packets: &[Packet]) -> Result<(), String> {
        let api = HidApi::new().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == vid && d.product_id() == pid && d.interface_number() == k.interface)
            .ok_or_else(|| format!("{} not found", device_id(vid, pid)))?;
        let dev = info.open_device(&api).map_err(|e| e.to_string())?;
        for p in packets {
            match p {
                Packet::Feature(b) => dev.send_feature_report(b).map_err(|e| e.to_string())?,
                Packet::Write(b) => {
                    dev.write(b).map_err(|e| e.to_string())?;
                }
            }
            if k.proto == Proto::ClutchGm11 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        Ok(())
    }
}

/// Development builds (Linux) show one fake device of each kind.
#[cfg(not(windows))]
mod hid {
    use super::*;

    pub fn list() -> Vec<LightDevice> {
        KNOWN
            .iter()
            .filter(|k| k.pid != 0x1161 && k.pid != 0x1122)
            .map(|k| LightDevice { id: device_id(k.vid, k.pid), name: k.name.into(), kind: k.kind.into(), supported: true })
            .collect()
    }

    pub fn send(_vid: u16, _pid: u16, _k: &Known, _packets: &[Packet]) -> Result<(), String> {
        Ok(())
    }
}

pub fn list() -> Vec<LightDevice> {
    hid::list()
}

pub fn set(id: &str, color: [u8; 3], brightness: u8) -> Result<(), String> {
    let (v, p) = id.split_once(':').ok_or("bad device id")?;
    let vid = u16::from_str_radix(v, 16).map_err(|_| "bad device id")?;
    let pid = u16::from_str_radix(p, 16).map_err(|_| "bad device id")?;
    let k = known(vid, pid).ok_or_else(|| format!("{id} is not a supported lighting device"))?;
    if brightness > 100 {
        return Err(format!("brightness {brightness} out of range"));
    }
    hid::send(vid, pid, k, &frames(k.proto, color, brightness))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn klc_frame_layout() {
        let f = frames(Proto::Klc, [255, 0, 128], 100);
        let Packet::Feature(b) = &f[0] else { panic!() };
        assert_eq!(b.len(), 525);
        assert_eq!(&b[..4], &[0x00, 0x0c, 0x00, 0x66]);
        assert_eq!(&b[5..9], &[0x04, 255, 0, 128]);
        let after = 5 + KLC_KEYS.len() * 4;
        assert_eq!(b[after], 0xff);
    }

    #[test]
    fn brightness_scales_and_zero_is_black() {
        let Packet::Feature(b) = &frames(Proto::Klc, [200, 100, 50], 50)[0] else { panic!() };
        assert_eq!(&b[6..9], &[100, 50, 25]);
        let Packet::Feature(b) = &frames(Proto::Klc, [200, 100, 50], 0)[0] else { panic!() };
        assert_eq!(&b[6..9], &[0, 0, 0]);
    }

    #[test]
    fn lightbar_chunks_and_commits() {
        let f = frames(Proto::KlcLightbar, [1, 2, 3], 100);
        assert_eq!(f.len(), 3);
        let Packet::Feature(b) = &f[1] else { panic!() };
        assert_eq!(b[3], 23);
        assert!(matches!(&f[2], Packet::Write(w) if w[1] == 0x0d && w.len() == 65));
    }

    #[test]
    fn gm11_packets_are_checksummed() {
        for p in frames(Proto::ClutchGm11, [10, 20, 30], 100) {
            if let Packet::Feature(b) = p {
                assert_eq!(b.len(), 9);
                let sum = b[1..8].iter().fold(0u8, |a, &v| a.wrapping_add(v));
                assert_eq!(b[8], 0xff_u8.wrapping_sub(sum));
            }
        }
    }

    #[test]
    fn unknown_devices_are_refused() {
        assert!(set("1038:9999", [0, 0, 0], 50).is_err());
        assert!(set("1038:113a", [0, 0, 0], 101).is_err());
    }
}
