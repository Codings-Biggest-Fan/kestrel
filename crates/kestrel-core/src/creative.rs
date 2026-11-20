//! Creative Sound BlasterX G6 over USB HID (VID 041e, PID 3256, interface 4).
//!
//! 64-byte packets `5a [family] [payload..]`. Every write is followed by a read of
//! the same setting, and the cached state only ever holds what the device answered.
//! Protocol credit: Nils Skowasch (soundblaster-x-g6-cli), linuxblaster_control and
//! Rusty G6, who captured these commands. See docs/HARDWARE.md.

use crate::ec::{EcError, Result};
use serde::{Deserialize, Serialize};

pub const VID: u16 = 0x041e;
pub const PID_G6: u16 = 0x3256;
pub const INTERFACE: u8 = 4;
pub const PACKET: usize = 64;
const PREFIX: u8 = 0x5a;
const PLAYBACK: u8 = 0x96;
const FAM_FIRMWARE: u8 = 0x07;
const FAM_DSP_READ: u8 = 0x11;
const FAM_DSP_WRITE: u8 = 0x12;
const FAM_GLOBAL: u8 = 0x26;
const FAM_OUTPUT: u8 = 0x2c;

/// One HID interface: 64-byte reports without the report-ID byte.
pub trait HidIo: Send {
    fn write(&mut self, packet: &[u8; PACKET]) -> Result<()>;
    /// Next report from the device, or `None` after `timeout_ms`.
    fn read(&mut self, timeout_ms: u32) -> Result<Option<[u8; PACKET]>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Output {
    #[default]
    Headphones,
    Speakers,
}

/// Playback effects, with their (on, level) DSP parameter ids in family 0x96.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Surround,
    Crystalizer,
    Bass,
    SmartVolume,
    DialogPlus,
}

impl Effect {
    pub const ALL: [Effect; 5] = [
        Effect::Surround,
        Effect::Crystalizer,
        Effect::Bass,
        Effect::SmartVolume,
        Effect::DialogPlus,
    ];

    fn ids(self) -> (u8, u8) {
        match self {
            Effect::Surround => (0x00, 0x01),
            Effect::DialogPlus => (0x02, 0x03),
            Effect::SmartVolume => (0x04, 0x05),
            Effect::Crystalizer => (0x07, 0x08),
            Effect::Bass => (0x18, 0x19),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Effect::Surround => "Surround",
            Effect::Crystalizer => "Crystalizer",
            Effect::Bass => "Bass",
            Effect::SmartVolume => "Smart Volume",
            Effect::DialogPlus => "Dialog+",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EffectState {
    pub on: bool,
    /// 0-100 %.
    pub level: u8,
}

/// What the device last reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AudioState {
    pub device: String,
    pub firmware: String,
    pub output: Option<Output>,
    /// SBX processing master switch.
    pub sbx: Option<bool>,
    pub scout: Option<bool>,
    /// Indexed like `Effect::ALL`.
    pub effects: Vec<Option<EffectState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AudioSetting {
    Output(Output),
    Sbx(bool),
    Scout(bool),
    EffectOn(Effect, bool),
    EffectLevel(Effect, u8),
}

fn packet(family: u8, payload: &[u8]) -> [u8; PACKET] {
    let mut p = [0u8; PACKET];
    p[0] = PREFIX;
    p[1] = family;
    p[2..2 + payload.len()].copy_from_slice(payload);
    p
}

pub fn dsp_read(id: u8) -> [u8; PACKET] {
    packet(FAM_DSP_READ, &[0x03, 0x01, PLAYBACK, id])
}

pub fn dsp_write(id: u8, value: f32) -> [u8; PACKET] {
    let v = value.to_le_bytes();
    packet(
        FAM_DSP_WRITE,
        &[0x07, 0x01, PLAYBACK, id, v[0], v[1], v[2], v[3]],
    )
}

pub fn output_write(o: Output) -> [u8; PACKET] {
    packet(
        FAM_OUTPUT,
        &[0x05, 0x00, if o == Output::Speakers { 0x02 } else { 0x04 }],
    )
}

pub fn output_read() -> [u8; PACKET] {
    packet(FAM_OUTPUT, &[0x01, 0x01])
}

/// `mode`: 1 = SBX, 2 = Scout.
pub fn global_write(mode: u8, on: bool) -> [u8; PACKET] {
    packet(FAM_GLOBAL, &[0x05, 0x07, mode, 0x00, on as u8, 0x00])
}

pub fn global_read() -> [u8; PACKET] {
    packet(FAM_GLOBAL, &[0x03, 0x08, 0xff, 0xff])
}

pub fn firmware_read() -> [u8; PACKET] {
    packet(FAM_FIRMWARE, &[0x01, 0x02])
}

/// A report the G6 sent, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    Dsp(Vec<(u8, f32)>),
    Output(Output),
    Global { sbx: bool, scout: bool },
    Firmware(String),
}

pub fn parse(p: &[u8; PACKET]) -> Option<Report> {
    if p[0] != PREFIX {
        return None;
    }
    match p[1] {
        // 5a 11 [len] [count] 00 then count x [family id f32]; len = 2 + 6 * count.
        FAM_DSP_READ => {
            let count = p[3] as usize;
            if p[2] as usize != 2 + count * 6 || 5 + count * 6 > PACKET {
                return None;
            }
            let vals = p[5..5 + count * 6]
                .as_chunks::<6>()
                .0
                .iter()
                .filter(|b| b[0] == PLAYBACK)
                .map(|b| (b[1], f32::from_le_bytes([b[2], b[3], b[4], b[5]])))
                .collect();
            Some(Report::Dsp(vals))
        }
        FAM_OUTPUT if p[2..4] == [0x05, 0x01] => match p[4] {
            0x02 => Some(Report::Output(Output::Speakers)),
            0x04 => Some(Report::Output(Output::Headphones)),
            _ => None,
        },
        FAM_GLOBAL if p[2..6] == [0x0b, 0x08, 0xff, 0xff] => Some(Report::Global {
            sbx: p[6] & 1 != 0,
            scout: p[6] & 2 != 0,
        }),
        FAM_FIRMWARE => {
            let len = (p[2] as usize).min(PACKET - 3);
            let s: String = p[3..3 + len]
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect();
            Some(Report::Firmware(s.trim().to_string()))
        }
        _ => None,
    }
}

fn level_to_f32(pct: u8) -> f32 {
    pct.min(100) as f32 / 100.0
}

fn f32_to_level(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 100.0).round() as u8
}

pub struct SoundBlaster {
    io: Box<dyn HidIo>,
    state: AudioState,
}

const ANSWER_MS: u32 = 300;

impl SoundBlaster {
    /// Opens only if the device answers the firmware query.
    pub fn new(io: Box<dyn HidIo>, device: &str) -> Result<Self> {
        let mut sb = Self {
            io,
            state: AudioState {
                device: device.into(),
                effects: vec![None; Effect::ALL.len()],
                ..Default::default()
            },
        };
        sb.ask(&firmware_read(), |r| matches!(r, Report::Firmware(_)))
            .map_err(|_| EcError::Io(format!("{device} did not answer the firmware query")))?;
        sb.refresh();
        Ok(sb)
    }

    pub fn state(&self) -> &AudioState {
        &self.state
    }

    fn apply(&mut self, r: &Report) {
        match r {
            Report::Firmware(f) => self.state.firmware = f.clone(),
            Report::Output(o) => self.state.output = Some(*o),
            Report::Global { sbx, scout } => {
                (self.state.sbx, self.state.scout) = (Some(*sbx), Some(*scout))
            }
            Report::Dsp(vals) => {
                for &(id, v) in vals {
                    let Some(i) = Effect::ALL
                        .iter()
                        .position(|e| e.ids().0 == id || e.ids().1 == id)
                    else {
                        continue;
                    };
                    let cur = self.state.effects[i].get_or_insert_with(EffectState::default);
                    if id == Effect::ALL[i].ids().0 {
                        cur.on = v >= 0.5;
                    } else {
                        cur.level = f32_to_level(v);
                    }
                }
            }
        }
    }

    /// Send `req` and apply every report until one satisfies `done`.
    fn ask(&mut self, req: &[u8; PACKET], done: impl Fn(&Report) -> bool) -> Result<Report> {
        self.io.write(req)?;
        for _ in 0..16 {
            let Some(p) = self.io.read(ANSWER_MS)? else {
                break;
            };
            if let Some(r) = parse(&p) {
                self.apply(&r);
                if done(&r) {
                    return Ok(r);
                }
            }
        }
        Err(EcError::Io("no answer from the Sound Blaster".into()))
    }

    fn read_dsp(&mut self, id: u8) -> Result<f32> {
        let r = self.ask(
            &dsp_read(id),
            |r| matches!(r, Report::Dsp(v) if v.iter().any(|x| x.0 == id)),
        )?;
        match r {
            Report::Dsp(v) => Ok(v
                .iter()
                .find(|x| x.0 == id)
                .map(|x| x.1)
                .unwrap_or_default()),
            _ => unreachable!(),
        }
    }

    /// Re-read every setting; unanswered ones stay as last reported.
    pub fn refresh(&mut self) {
        let _ = self.ask(&output_read(), |r| matches!(r, Report::Output(_)));
        let _ = self.ask(&global_read(), |r| matches!(r, Report::Global { .. }));
        for e in Effect::ALL {
            let (on, level) = e.ids();
            let _ = self.read_dsp(on);
            let _ = self.read_dsp(level);
        }
    }

    pub fn set(&mut self, s: AudioSetting) -> Result<()> {
        let confirmed = match s {
            AudioSetting::Output(o) => {
                self.io.write(&output_write(o))?;
                self.ask(&output_read(), |r| matches!(r, Report::Output(_)))?;
                self.state.output == Some(o)
            }
            AudioSetting::Sbx(on) | AudioSetting::Scout(on) => {
                let sbx = matches!(s, AudioSetting::Sbx(_));
                self.io.write(&global_write(if sbx { 1 } else { 2 }, on))?;
                self.ask(&global_read(), |r| matches!(r, Report::Global { .. }))?;
                (if sbx {
                    self.state.sbx
                } else {
                    self.state.scout
                }) == Some(on)
            }
            AudioSetting::EffectOn(e, on) => {
                let id = e.ids().0;
                self.io.write(&dsp_write(id, if on { 1.0 } else { 0.0 }))?;
                (self.read_dsp(id)? >= 0.5) == on
            }
            AudioSetting::EffectLevel(e, pct) => {
                if pct > 100 {
                    return Err(EcError::InvalidValue(format!("{} level {pct}", e.label())));
                }
                let id = e.ids().1;
                self.io.write(&dsp_write(id, level_to_f32(pct)))?;
                f32_to_level(self.read_dsp(id)?) == pct
            }
        };
        if confirmed {
            Ok(())
        } else {
            Err(EcError::Io(format!("Sound Blaster did not take {s:?}")))
        }
    }
}

/// Simulated G6 that answers like the real device, for tests and `--mock-audio`.
#[derive(Default)]
pub struct MockG6 {
    pub dsp: std::collections::BTreeMap<u8, f32>,
    pub output: Option<Output>,
    pub global: u8,
    pending: std::collections::VecDeque<[u8; PACKET]>,
    pub writes: Vec<[u8; PACKET]>,
    /// Ignore writes to this DSP id (to test unconfirmed writes).
    pub stuck: Option<u8>,
}

impl MockG6 {
    pub fn new() -> Self {
        let mut m = Self {
            output: Some(Output::Headphones),
            ..Default::default()
        };
        for (id, v) in [
            (0x00, 1.0),
            (0x01, 0.67),
            (0x07, 0.0),
            (0x08, 0.5),
            (0x18, 1.0),
            (0x19, 0.3),
            (0x04, 0.0),
            (0x05, 0.5),
            (0x02, 0.0),
            (0x03, 0.5),
        ] {
            m.dsp.insert(id, v);
        }
        m.global = 1;
        m
    }

    fn dsp_report(id: u8, v: f32) -> [u8; PACKET] {
        let b = v.to_le_bytes();
        packet(
            FAM_DSP_READ,
            &[0x08, 0x01, 0x00, PLAYBACK, id, b[0], b[1], b[2], b[3]],
        )
    }
}

impl HidIo for MockG6 {
    fn write(&mut self, p: &[u8; PACKET]) -> Result<()> {
        self.writes.push(*p);
        match (p[1], &p[2..8]) {
            (FAM_FIRMWARE, _) => self.pending.push_back(packet(
                FAM_FIRMWARE,
                &[0x06, b'2', b'.', b'1', b'.', b'0', 0],
            )),
            (FAM_DSP_READ, [0x03, 0x01, PLAYBACK, id, ..]) => self.pending.push_back(
                Self::dsp_report(*id, self.dsp.get(id).copied().unwrap_or(0.0)),
            ),
            (FAM_DSP_WRITE, [0x07, 0x01, PLAYBACK, id, a, b]) => {
                if self.stuck != Some(*id) {
                    self.dsp
                        .insert(*id, f32::from_le_bytes([*a, *b, p[8], p[9]]));
                }
            }
            (FAM_OUTPUT, [0x05, 0x00, o, ..]) => {
                self.output = Some(if *o == 0x02 {
                    Output::Speakers
                } else {
                    Output::Headphones
                })
            }
            (FAM_OUTPUT, [0x01, 0x01, ..]) => {
                let o = if self.output == Some(Output::Speakers) {
                    0x02
                } else {
                    0x04
                };
                self.pending.push_back(packet(FAM_OUTPUT, &[0x05, 0x01, o]));
            }
            (FAM_GLOBAL, [0x05, 0x07, mode, 0x00, on, ..]) => {
                let bit = if *mode == 1 { 1 } else { 2 };
                self.global = if *on != 0 {
                    self.global | bit
                } else {
                    self.global & !bit
                };
            }
            (FAM_GLOBAL, [0x03, 0x08, ..]) => self
                .pending
                .push_back(packet(FAM_GLOBAL, &[0x0b, 0x08, 0xff, 0xff, self.global])),
            _ => {}
        }
        Ok(())
    }

    fn read(&mut self, _timeout_ms: u32) -> Result<Option<[u8; PACKET]>> {
        Ok(self.pending.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_match_captures() {
        assert_eq!(&dsp_read(0x08)[..6], &[0x5a, 0x11, 0x03, 0x01, 0x96, 0x08]);
        assert_eq!(
            &dsp_write(0x00, 1.0)[..10],
            &[0x5a, 0x12, 0x07, 0x01, 0x96, 0x00, 0x00, 0x00, 0x80, 0x3f]
        );
        assert_eq!(
            &output_write(Output::Speakers)[..5],
            &[0x5a, 0x2c, 0x05, 0x00, 0x02]
        );
        assert_eq!(
            &global_write(2, true)[..8],
            &[0x5a, 0x26, 0x05, 0x07, 0x02, 0x00, 0x01, 0x00]
        );
        assert_eq!(&global_read()[..6], &[0x5a, 0x26, 0x03, 0x08, 0xff, 0xff]);
        assert!(output_write(Output::Headphones)[5..]
            .iter()
            .all(|&b| b == 0));
    }

    #[test]
    fn parses_multi_block_dsp_report() {
        let mut p = packet(FAM_DSP_READ, &[0x0e, 0x02, 0x00]);
        p[5..11].copy_from_slice(&[0x96, 0x01, 0, 0, 0, 0x3f]);
        p[11..17].copy_from_slice(&[0x95, 0x04, 0, 0, 0x80, 0x3f]);
        assert_eq!(parse(&p), Some(Report::Dsp(vec![(0x01, 0.5)])));
        p[2] = 0x0f;
        assert_eq!(parse(&p), None);
    }

    #[test]
    fn opens_reads_and_confirms_writes() {
        let mut sb = SoundBlaster::new(Box::new(MockG6::new()), "Sound BlasterX G6").unwrap();
        let s = sb.state().clone();
        assert_eq!(s.firmware, "2.1.0");
        assert_eq!(
            (s.output, s.sbx, s.scout),
            (Some(Output::Headphones), Some(true), Some(false))
        );
        assert_eq!(
            s.effects[0],
            Some(EffectState {
                on: true,
                level: 67
            })
        );
        sb.set(AudioSetting::Output(Output::Speakers)).unwrap();
        sb.set(AudioSetting::Scout(true)).unwrap();
        sb.set(AudioSetting::EffectOn(Effect::Crystalizer, true))
            .unwrap();
        sb.set(AudioSetting::EffectLevel(Effect::Bass, 80)).unwrap();
        let s = sb.state();
        assert_eq!((s.output, s.scout), (Some(Output::Speakers), Some(true)));
        assert_eq!(
            s.effects[1],
            Some(EffectState {
                on: true,
                level: 50
            })
        );
        assert_eq!(
            s.effects[2],
            Some(EffectState {
                on: true,
                level: 80
            })
        );
        assert!(sb
            .set(AudioSetting::EffectLevel(Effect::Bass, 101))
            .is_err());
    }

    #[test]
    fn unconfirmed_write_is_an_error_and_state_shows_device() {
        let mock = MockG6 {
            stuck: Some(0x19),
            ..MockG6::new()
        };
        let mut sb = SoundBlaster::new(Box::new(mock), "G6").unwrap();
        assert!(sb.set(AudioSetting::EffectLevel(Effect::Bass, 90)).is_err());
        assert_eq!(sb.state().effects[2].unwrap().level, 30);
    }

    struct Silent;
    impl HidIo for Silent {
        fn write(&mut self, _: &[u8; PACKET]) -> Result<()> {
            Ok(())
        }
        fn read(&mut self, _: u32) -> Result<Option<[u8; PACKET]>> {
            Ok(None)
        }
    }

    #[test]
    fn silent_device_is_not_opened() {
        assert!(SoundBlaster::new(Box::new(Silent), "G6").is_err());
    }
}
