//! The G3-family protocol (G3 V2, G3 V2 Pro, G3 A, A7e, ...), recovered from
//! the vendor's gmouse web driver (M HUB loads it for these models).
//!
//! Unlike the L7 family these mice declare no feature reports on the vendor
//! interface; instead every exchange is one 64-byte output report answered by
//! one 64-byte input report on the interrupt pipes:
//!
//! ```text
//! host   -> 55 <cmd> <args...>          (zero padded to 64 bytes)
//! device <- AA <cmd> <payload...>
//! ```
//!
//! The mouse also pushes unsolicited frames (AA FA .. on DPI-button changes,
//! AA 30 .. battery, AA ED .. link), so replies are matched on the command
//! echo and anything else is skipped while waiting.
//!
//! The command layouts below mirror the driver's frames byte for byte,
//! including the fixed 10-byte request headers, because the firmware was only
//! ever tested against those.

use crate::device::{self, Mouse};
use crate::hidraw::HidRaw;
use crate::logln;
use crate::preset::Preset;
use crate::proto::hex;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SYNC: u8 = 0x55;
const ACK: u8 = 0xAA;

const CMD_VERSION: u8 = 0x03;
const CMD_CONFIG_READ: u8 = 0x0E;
const CMD_CONFIG_WRITE: u8 = 0x0F;
const CMD_BATTERY: u8 = 0x30;
const CMD_ONLINE: u8 = 0xED;

pub const FRAME: usize = 64;

/// Poll rates the G3 V2 firmware accepts (index 0..=3). From the vendor's
/// model config: the G3 V2 is a 1 kHz-only part.
pub const RATES: [u32; 4] = [125, 250, 500, 1000];

/// DPI bounds from the vendor's G3 V2 model config.
pub const DPI_MIN: u16 = 200;
pub const DPI_MAX: u16 = 12000;

pub const DPI_STAGES: usize = 6;

fn timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "no reply from the mouse")
}

/// One round trip: send `[55 cmd args..]`, return the first input report that
/// echoes the command. The interface has no report IDs, so on write the
/// kernel takes a leading 0 and strips it; reads come back unprefixed.
pub fn request(dev: &HidRaw, cmd: u8, args: &[u8]) -> io::Result<Vec<u8>> {
    assert!(args.len() <= FRAME - 2);
    let mut out = [0u8; FRAME + 1];
    out[1] = SYNC;
    out[2] = cmd;
    out[3..3 + args.len()].copy_from_slice(args);
    for attempt in 1..=3 {
        // Drop stale pushes so an old frame cannot be mistaken for the reply.
        while dev.read_report(Duration::ZERO).is_ok() {}
        logln!("g3 -> {}", hex(&out[1..1 + 2 + args.len()]));
        dev.write_report(&out)?;
        let deadline = Instant::now() + Duration::from_millis(500);
        while let Some(remain) = deadline.checked_duration_since(Instant::now()) {
            let frame = match dev.read_report(remain) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e),
            };
            logln!("g3 <- {}", hex(&frame));
            if frame.len() >= 2 && frame[0] == ACK && frame[1] == cmd {
                return Ok(frame);
            }
            logln!("g3 .. skipped a push frame");
        }
        logln!("g3 .. attempt {attempt} timed out");
    }
    Err(timeout())
}

/// Fixed request headers, lifted from the driver verbatim.
const ARGS_CONFIG: [u8; 8] = [0xA5, 0x0B, 0x30, 0x01, 0x01, 0x01, 0x00, 0x00];
const ARGS_BATTERY: [u8; 8] = [0xA5, 0x0B, 0x2E, 0x01, 0x01, 0x01, 0x00, 0x00];

/// Firmware version, e.g. "2.3.5". The wired firmware carries the digits at
/// 23..26 of the version reply; the dongle firmware moves them around
/// (observed at 20..22), so fall back to the first 3-digit ASCII run.
pub fn version(dev: &HidRaw) -> io::Result<String> {
    let r = request(dev, CMD_VERSION, &[])?;
    let digits = |w: &[u8]| w.iter().all(|b| b.is_ascii_digit());
    if r.len() > 25 && digits(&r[23..26]) {
        let v = &r[23..26];
        return Ok(format!("{}.{}.{}", v[0] as char, v[1] as char, v[2] as char));
    }
    let end = r.len().min(28);
    for i in 8..end.saturating_sub(2) {
        if digits(&r[i..i + 3]) && (i + 3 >= end || !r[i + 3].is_ascii_digit()) {
            return Ok(format!(
                "{}.{}.{}",
                r[i] as char, r[i + 1] as char, r[i + 2] as char
            ));
        }
    }
    Ok("unknown".into())
}

/// Battery percent and the charging flag, from the cmd 0x30 reply.
pub fn battery(dev: &HidRaw) -> io::Result<(u8, bool)> {
    let r = request(dev, CMD_BATTERY, &ARGS_BATTERY)?;
    if r.len() < 10 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short battery reply"));
    }
    Ok((r[8], r[9] != 0))
}

/// Wireless link status through a dongle, cmd 0xED: 1 when the mouse is
/// online. The wired interface reports 0.
pub fn online(dev: &HidRaw) -> io::Result<u8> {
    let r = request(dev, CMD_ONLINE, &[])?;
    if r.len() < 9 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short online reply"));
    }
    Ok(r[8])
}

/// The raw cmd 0x0E reply, for backups.
pub fn read_raw(dev: &HidRaw) -> io::Result<Vec<u8>> {
    request(dev, CMD_CONFIG_READ, &ARGS_CONFIG)
}

/// Everything the cmd 0x0E reply carries, parsed at the driver's own offsets.
/// Unknown and padding bytes are not kept: the driver rebuilds writes from a
/// zeroed template, and so do we.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    /// Index into RATES.
    pub rate: u8,
    pub dpi_count: u8,
    pub dpi_index: u8,
    pub dpi: [u16; DPI_STAGES],
    pub scroll_reverse: bool,
    /// Lift-off distance as stored: 1 or 2 mm.
    pub lod: u8,
    /// Sensor flags; bit 0 is line correction (angle snapping).
    pub sensor: u8,
    pub debounce_ms: u8,
    pub sleep_min: u8,
    pub highspeed: bool,
    /// Sensor rotation in degrees, signed.
    pub angle: i8,
    /// True when the reply carried no config (all-0x00/0xFF DPI bytes, which
    /// is how a dongle answers when it cannot reach the mouse) and these are
    /// the driver's factory defaults, not live values. Writes based on a
    /// stub would silently go nowhere, so callers must refuse them.
    pub stub: bool,
}

impl Config {
    /// The driver's fallback when the reply carries no config at all (first
    /// DPI word reads all 0x00 or all 0xFF; a dongle that cannot reach the
    /// mouse answers like that).
    fn defaults() -> Self {
        Self {
            rate: 3,
            dpi_count: 6,
            dpi_index: 2,
            dpi: [800, 1600, 2400, 3200, 5000, 12000],
            scroll_reverse: false,
            lod: 1,
            sensor: 0,
            debounce_ms: 8,
            sleep_min: 3,
            highspeed: false,
            angle: 0,
            stub: true,
        }
    }

    /// Parse a cmd 0x0E reply. Fields the firmware leaves at 0xFF (the wired
    /// G3 V2 does this for lod and highspeed) are clamped to a valid value,
    /// the same values the driver's own defaults use.
    pub fn parse(r: &[u8]) -> io::Result<Self> {
        if r.len() < FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short config reply"));
        }
        let first = u16::from_le_bytes([r[13], r[14]]);
        if (first == 0 && r[15] == 0) || (first == 0xFFFF && r[15] == 0xFF) {
            return Ok(Self::defaults());
        }
        let mut dpi = [0u16; DPI_STAGES];
        for (i, v) in dpi.iter_mut().enumerate() {
            *v = u16::from_le_bytes([r[13 + i * 2], r[14 + i * 2]]);
        }
        let rate = r[10].saturating_sub(1).min(RATES.len() as u8 - 1);
        let dpi_count = r[11].clamp(1, DPI_STAGES as u8);
        let dpi_index = r[12].saturating_sub(1).min(dpi_count - 1);
        let lod = match r[49] {
            1 | 2 => r[49],
            _ => 1,
        };
        let highspeed = r[53] == 1;
        let scroll_reverse = r[48] == 1;
        Ok(Self {
            rate,
            dpi_count,
            dpi_index,
            dpi,
            scroll_reverse,
            lod,
            sensor: r[50],
            debounce_ms: r[51],
            sleep_min: r[52],
            highspeed,
            angle: r[55] as i8,
            stub: false,
        })
    }

    pub fn read(dev: &HidRaw) -> io::Result<Self> {
        let r = request(dev, CMD_CONFIG_READ, &ARGS_CONFIG)?;
        Self::parse(&r)
    }

    /// The cmd 0x0F frame. Field offsets are exactly the read reply's (the
    /// driver's own array has one extra leading report-ID byte, which is why
    /// its indices look one higher). Verified on a wired G3 V2: write with
    /// these offsets, read back, every field lands.
    pub fn frame(&self) -> [u8; FRAME] {
        let mut f = [0u8; FRAME];
        f[0..10].copy_from_slice(&[SYNC, CMD_CONFIG_WRITE, 0xAE, 0x0A, 0x30, 0x01, 0x01, 0x01, 0x00, 0x00]);
        f[10] = self.rate + 1;
        f[11] = self.dpi_count;
        f[12] = self.dpi_index + 1;
        for (i, v) in self.dpi.iter().enumerate() {
            f[13 + i * 2..15 + i * 2].copy_from_slice(&v.to_le_bytes());
        }
        f[48] = self.scroll_reverse as u8;
        f[49] = self.lod;
        f[50] = self.sensor;
        f[51] = self.debounce_ms;
        f[52] = self.sleep_min;
        f[53] = self.highspeed as u8;
        f[55] = self.angle as u8;
        f
    }

    pub fn write(&self, dev: &HidRaw) -> io::Result<()> {
        let f = self.frame();
        request(dev, CMD_CONFIG_WRITE, &f[2..FRAME])?;
        Ok(())
    }

    /// True when this config matches every field of the preset the G3 family
    /// has. Game mode, motion sync and ripple are L7-only.
    pub fn matches_preset(&self, p: &Preset) -> bool {
        if self.stub {
            return false;
        }
        let i = self.dpi_index as usize;
        self.dpi[i] == p.dpi
            && self.dpi_index == p.stage
            && RATES[self.rate as usize] == p.rate_hz
            && self.lod == if p.lod == 0 { 1 } else { 2 }
            && self.debounce_ms == p.debounce_ms
            && self.sleep_min == p.sleep_min
            && (self.sensor & 1 != 0) == p.angle_snap
    }

    /// Apply the fields of a preset this family has, clamped to its ranges.
    pub fn apply_preset(&mut self, p: &Preset) {
        let stage = (p.stage as usize).min(DPI_STAGES - 1);
        self.dpi[stage] = p.dpi.clamp(DPI_MIN, DPI_MAX);
        self.dpi_index = stage as u8;
        if let Some(index) = RATES.iter().position(|r| *r == p.rate_hz) {
            self.rate = index as u8;
        }
        self.lod = if p.lod == 0 { 1 } else { 2 };
        self.debounce_ms = p.debounce_ms;
        self.sleep_min = p.sleep_min;
        self.sensor = if p.angle_snap { self.sensor | 1 } else { self.sensor & !1 };
    }
}

/// Save the raw config block the first time anything writes, so there is
/// always a way back. Returns the path when it was created.
pub fn backup_once(dev: &HidRaw) -> io::Result<Option<PathBuf>> {
    let path = crate::log::path().with_file_name("config-g3.original.bin");
    if path.exists() {
        return Ok(None);
    }
    let raw = read_raw(dev)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, raw)?;
    Ok(Some(path))
}

/// Read, change, write and confirm a config, refusing nodes that cannot
/// carry the write: a stub config (no live config is served), or a dongle
/// while the wired sibling is present — observed on the G3 V2, where the
/// dongle keeps a private config cache and does not proxy (verified by
/// unbinding the wired USB device: writes landed in the cache only).
pub fn edit<F: Fn(&mut Config)>(m: &Mouse, change: F) -> io::Result<Config> {
    let mut c = Config::read(&m.dev)?;
    if c.stub {
        return Err(io::Error::other(
            "this node is not serving the mouse's live config (a dongle while the mouse is on the cable, or the wireless link is down). Use the wired node",
        ));
    }
    if m.pid != 0x418c {
        if let Ok(nodes) = device::candidates() {
            if nodes.iter().any(|n| n.pid == 0x418c) {
                return Err(io::Error::other(
                    "the mouse is also on the cable; the dongle does not proxy writes in this state. Write via the wired node",
                ));
            }
        }
    }
    change(&mut c);
    c.write(&m.dev)?;
    std::thread::sleep(Duration::from_millis(60));
    let got = Config::read(&m.dev)?;
    if got != c {
        return Err(io::Error::other("the mouse did not take the setting (read-back differs)"));
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reply captured from a wired G3 V2 (firmware 2.3.5).
    const WIRED_CONFIG: [u8; 64] = [
        0xaa, 0x0e, 0xa5, 0xf5, 0x2e, 0x01, 0x01, 0x01, 0x00, 0x00, 0x02, 0x06, 0x03, 0x90, 0x01,
        0x20, 0x03, 0x40, 0x06, 0x80, 0x0c, 0x00, 0x19, 0xe0, 0x2e, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xff, 0x01, 0x0a, 0x03, 0xff, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn parse_wired_reply() {
        let c = Config::parse(&WIRED_CONFIG).unwrap();
        assert_eq!(c.rate, 1);
        assert_eq!(c.dpi_count, 6);
        assert_eq!(c.dpi_index, 2);
        assert_eq!(c.dpi, [400, 800, 1600, 3200, 6400, 12000]);
        assert!(!c.scroll_reverse);
        assert_eq!(c.lod, 1); // 0xff on the wire, clamped
        assert_eq!(c.sensor, 1);
        assert_eq!(c.debounce_ms, 10);
        assert_eq!(c.sleep_min, 3);
        assert!(!c.highspeed); // 0xff on the wire, clamped
        assert_eq!(c.angle, 0);
    }

    #[test]
    fn parse_empty_reply_gives_defaults() {
        let mut r = [0xffu8; 64];
        r[0] = ACK;
        r[1] = CMD_CONFIG_READ;
        let c = Config::parse(&r).unwrap();
        assert_eq!(c, Config::defaults());
        assert!(c.stub);
    }

    #[test]
    fn frame_layout_matches_driver() {
        let c = Config {
            rate: 3,
            dpi_count: 6,
            dpi_index: 2,
            dpi: [400, 800, 1600, 3200, 6400, 12000],
            scroll_reverse: false,
            lod: 2,
            sensor: 1,
            debounce_ms: 8,
            sleep_min: 3,
            highspeed: true,
            angle: -5,
            stub: false,
        };
        let f = c.frame();
        assert_eq!(&f[0..10], &[0x55, 0x0f, 0xae, 0x0a, 0x30, 1, 1, 1, 0, 0]);
        assert_eq!(f[10], 4);
        assert_eq!(f[11], 6);
        assert_eq!(f[12], 3);
        assert_eq!(&f[13..15], &400u16.to_le_bytes());
        assert_eq!(&f[23..25], &12000u16.to_le_bytes());
        assert_eq!(f[49], 2);
        assert_eq!(f[50], 1);
        assert_eq!(f[51], 8);
        assert_eq!(f[52], 3);
        assert_eq!(f[53], 1);
        assert_eq!(f[55], (-5i8) as u8);
    }

    #[test]
    fn write_read_roundtrip_offsets() {
        // Write offsets are the read offsets: a reply built from frame()
        // must parse back to the same config.
        let c = Config::parse(&WIRED_CONFIG).unwrap();
        let f = c.frame();
        let mut r = f;
        r[0] = ACK;
        r[1] = CMD_CONFIG_READ;
        let back = Config::parse(&r).unwrap();
        assert_eq!(back, c);
    }
}
