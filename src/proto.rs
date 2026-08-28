//! The MCHOSE feature-report protocol. See PROTOCOL.md for where every
//! constant comes from.

use crate::hidraw::HidRaw;
use std::io;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// The mouse stops answering if it is talked at too fast: a preset used to fire
/// fifteen exchanges inside a second and wedge the config channel. Keep a floor
/// between them, process-wide, so no caller has to remember to.
const MIN_GAP: Duration = Duration::from_millis(25);

fn pace() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = *last {
        let since = t.elapsed();
        if since < MIN_GAP {
            sleep(MIN_GAP - since);
        }
    }
    *last = Some(Instant::now());
}

pub const REPORT_SHORT: u8 = 0x11; // 20 payload bytes
pub const REPORT_LONG: u8 = 0x12; // 64 payload bytes

pub fn payload_len(report: u8) -> usize {
    match report {
        REPORT_SHORT => 20,
        REPORT_LONG => 64,
        _ => 64,
    }
}

/// Send one command. `body[0]` is the command byte; the rest is the schema.
/// The whole payload goes out inverted, padding included.
pub fn send(dev: &HidRaw, report: u8, body: &[u8]) -> io::Result<()> {
    let len = payload_len(report);
    assert!(body.len() <= len, "command body longer than the report");
    let mut buf = vec![0xFFu8; len + 1];
    buf[0] = report;
    for (i, b) in body.iter().enumerate() {
        buf[1 + i] = b ^ 0xFF;
    }
    pace();
    let result = dev.set_feature(&buf);
    match &result {
        Ok(()) => logln!("-> {report:02x} {}", hex(body)),
        Err(e) => logln!("-> {report:02x} {} FAILED {e}", hex(body)),
    }
    result
}

/// Send a command and read the reply back. Retries while the echoed command
/// byte does not match, the way the vendor driver does.
pub fn request(dev: &HidRaw, report: u8, body: &[u8]) -> io::Result<Vec<u8>> {
    let len = payload_len(report);
    let command = body[0];
    let mut last = Vec::new();
    for attempt in 0..6 {
        send(dev, report, body)?;
        sleep(Duration::from_millis(20));
        let mut buf = vec![0u8; len + 1];
        buf[0] = report;
        pace();
        let n = dev.get_feature(&mut buf)?;
        buf.truncate(n.max(2));
        // An all-zero reply passes no useful field but can still carry a
        // correct echo: the mouse does that when its report channel has gone
        // quiet. No real reply in this protocol is entirely zero, so treat it
        // as a miss and ask again.
        if buf.len() >= 2 && (buf[1] ^ 0xFF) == command && buf[2..].iter().any(|b| *b != 0) {
            let payload: Vec<u8> = buf[2..].iter().map(|b| b ^ 0xFF).collect();
            logln!("<- {report:02x} {command:02x} {}", hex(&payload));
            return Ok(payload);
        }
        logln!(
            "<- {report:02x} {command:02x} no match on try {}, raw {}",
            attempt + 1,
            hex(&buf)
        );
        last = buf;
        sleep(Duration::from_millis(30 * (attempt + 1)));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "no reply to command 0x{command:02x} on report 0x{report:02x} (last: {})",
            hex(&last)
        ),
    ))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn put_u16le(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

// ---------------------------------------------------------------- identity

#[derive(Debug)]
pub struct Identity {
    pub vid: u16,
    pub pid: u16,
    pub firmware: u32,
    pub connect_mode: u8,
    pub connected: bool,
    pub battery: u8,
    pub charging: u8,
}

/// Read `0x11 0x06`.
///
/// The first request after the mouse has been idle comes back with the fields
/// zeroed even though the frame is otherwise well formed. A zero vendor ID is
/// the tell, so ask again rather than report a mouse that is nowhere.
pub fn identity(dev: &HidRaw) -> io::Result<Identity> {
    for _ in 0..4 {
        let d = request(dev, REPORT_SHORT, &[0x06])?;
        if d.len() < 11 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short identity reply"));
        }
        if u16le(&d, 0) == 0 {
            sleep(Duration::from_millis(120));
            continue;
        }
        let flags = d[8];
        return Ok(Identity {
            vid: u16le(&d, 0),
            pid: u16le(&d, 2),
            firmware: u32::from_le_bytes([d[4], d[5], d[6], d[7]]),
            connect_mode: flags & 0b0000_0111,
            connected: flags & 0b0000_1000 != 0,
            battery: d[9],
            charging: d[10],
        });
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the mouse keeps answering an empty identity. Is the dongle seated?",
    ))
}

/// Read `0x11 0x04`, the firmware version string.
pub fn version_string(dev: &HidRaw) -> io::Result<String> {
    let d = request(dev, REPORT_SHORT, &[0x04])?;
    if d.is_empty() {
        return Ok(String::new());
    }
    let n = (d[0] as usize).min(d.len() - 1);
    Ok(String::from_utf8_lossy(&d[1..1 + n]).trim_end_matches('\0').to_string())
}

// ------------------------------------------------------------------ config

/// The 63-byte body shared by read `0x12 0x67` and write `0x12 0x57`.
/// Kept as raw bytes so a read-modify-write never drops a field this tool
/// does not model.
#[derive(Clone)]
pub struct Config {
    pub body: [u8; 63],
}

pub const DPI_STAGES: usize = 6;

/// Bytes of the config block the mouse writes itself, so a read-back never
/// matches what was sent and comparing them raises a false alarm.
///   3  flips between 0x00 and 0x02 whenever the active DPI stage changes
///  17  bit 0 of `sensor` is always set, and refuses to be cleared
const DEVICE_OWNED: [usize; 1] = [3];
const SENSOR_OWNED_BITS: u8 = 0x01;

impl Config {
    /// Read the live config with `0x12 0x67`.
    ///
    /// The long-report channel can wedge and answer all zeros while the short
    /// reports keep working. Re-selecting the profile with `0x11 0x58` wakes
    /// it up, so do that rather than hand back a block of zeros that would
    /// look like a wiped mouse.
    pub fn read(dev: &HidRaw) -> io::Result<Self> {
        for attempt in 0..6 {
            let d = request(dev, REPORT_LONG, &[0x67])?;
            if d.len() < 63 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short config reply"));
            }
            if d[..63].iter().any(|b| *b != 0) {
                let mut body = [0u8; 63];
                body.copy_from_slice(&d[..63]);
                return Ok(Self { body });
            }
            if attempt > 0 {
                logln!("config channel answered zeroes, re-selecting the profile");
                set_profile(dev, 0)?;
            }
            sleep(Duration::from_millis(150 * (attempt + 1) as u64));
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the mouse keeps answering an empty config block. Unplug the dongle and plug it back in.",
        ))
    }

    /// The four performance flags `0x11 0x42` sets also land in this byte, so
    /// they can be cross-checked even though the command itself is write-only.
    /// Bit 0 belongs to the mouse and does not clear.
    pub fn perf_flags(&self) -> (bool, bool, bool) {
        let s = self.body[17];
        (s & 0x04 != 0, s & 0x08 != 0, s & 0x10 != 0) // ripple, angle snap, motion sync
    }

    /// Write it back with `0x12 0x57`.
    pub fn write(&self, dev: &HidRaw) -> io::Result<()> {
        let mut frame = Vec::with_capacity(64);
        frame.push(0x57);
        frame.extend_from_slice(&self.body);
        send(dev, REPORT_LONG, &frame)
    }

    /// Did the mouse take what we wrote? Everything must match except the
    /// bytes it keeps for itself.
    pub fn matches(&self, other: &Config) -> bool {
        for (i, (a, b)) in self.body.iter().zip(other.body.iter()).enumerate() {
            if DEVICE_OWNED.contains(&i) {
                continue;
            }
            if i == 17 {
                if a & !SENSOR_OWNED_BITS != b & !SENSOR_OWNED_BITS {
                    return false;
                }
                continue;
            }
            if a != b {
                return false;
            }
        }
        true
    }

    /// Where two blocks differ, for the log.
    pub fn diff(&self, other: &Config) -> String {
        self.body
            .iter()
            .zip(other.body.iter())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("[{i}] {a:02x}->{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn profile(&self) -> u8 {
        self.body[0]
    }

    // Byte 1 packs the wired pair, byte 2 the wireless pair. The vendor's own
    // read parser and write schema disagree on which is which; both directions
    // here use one layout, so a round-trip is stable either way.
    pub fn wired_rate_index(&self) -> u8 {
        self.body[1] >> 4
    }
    #[allow(dead_code)]
    pub fn wired_dpi_stage(&self) -> u8 {
        self.body[1] & 0x0F
    }
    pub fn wireless_rate_index(&self) -> u8 {
        self.body[2] >> 4
    }
    pub fn wireless_dpi_stage(&self) -> u8 {
        self.body[2] & 0x0F
    }
    /// Kept for `restore` and for anyone editing the block by hand; the rate
    /// itself is set with `0x11 0x41`, which owns it.
    #[allow(dead_code)]
    pub fn set_rate_index(&mut self, v: u8) {
        self.body[1] = (v << 4) | (self.body[1] & 0x0F);
        self.body[2] = (v << 4) | (self.body[2] & 0x0F);
    }
    pub fn set_dpi_stage(&mut self, v: u8) {
        self.body[1] = (self.body[1] & 0xF0) | (v & 0x0F);
        self.body[2] = (self.body[2] & 0xF0) | (v & 0x0F);
    }

    pub fn dpi(&self, stage: usize) -> u16 {
        u16le(&self.body, 4 + stage * 2)
    }
    pub fn set_dpi(&mut self, stage: usize, value: u16) {
        put_u16le(&mut self.body, 4 + stage * 2, value);
        // keep the Y axis square unless the caller asked otherwise
        put_u16le(&mut self.body, 51 + stage * 2, value);
    }
    pub fn dpi_y(&self, stage: usize) -> u16 {
        u16le(&self.body, 51 + stage * 2)
    }

    pub fn enabled_stages(&self) -> u8 {
        self.body[16]
    }
    pub fn set_enabled_stages(&mut self, n: u8) {
        self.body[16] = n;
    }
    pub fn sensor(&self) -> u8 {
        self.body[17]
    }
    /// Bit 0 is the mouse's own and refuses to be cleared, so keep it.
    pub fn set_sensor(&mut self, v: u8) {
        self.body[17] = v | (self.body[17] & 0x01);
    }
    pub fn debounce_ms(&self) -> u8 {
        self.body[18]
    }
    pub fn set_debounce_ms(&mut self, ms: u8) {
        self.body[18] = ms;
    }
    pub fn sleep_minutes(&self) -> u8 {
        self.body[19]
    }
    pub fn set_sleep_minutes(&mut self, m: u8) {
        self.body[19] = m;
    }
    pub fn rotate(&self) -> u8 {
        self.body[49]
    }
}

// ------------------------------------------------------------- performance

/// `0x11 0x42`. None of these read back, so the caller keeps the last written
/// set in its own state file.
#[derive(Clone, Copy, Debug)]
pub struct Performance {
    pub lod: u8,
    pub ripple: u8,
    pub angle_snap: u8,
    pub motion_sync: u8,
    pub rotate_open: u8,
    pub rotate_val: u8,
}

impl Default for Performance {
    fn default() -> Self {
        Self { lod: 0, ripple: 0, angle_snap: 0, motion_sync: 0, rotate_open: 0, rotate_val: 0 }
    }
}

pub fn set_performance(dev: &HidRaw, p: Performance) -> io::Result<()> {
    send(
        dev,
        REPORT_SHORT,
        &[0x42, p.lod, p.ripple, p.angle_snap, p.motion_sync, 0, 0, 0, p.rotate_open, p.rotate_val],
    )
}

/// `0x11 0x41`, both links in one frame.
pub fn set_report_rate(dev: &HidRaw, wired_index: u8, wireless_index: u8) -> io::Result<()> {
    send(dev, REPORT_SHORT, &[0x41, wired_index, wireless_index])
}

/// `0x11 0x02`. Game mode has its own command; the `gameMode` field inside
/// `0x11 0x42` is something else and does not move what `0x11 0x03` reports.
pub fn set_game_mode(dev: &HidRaw, mode: u8) -> io::Result<()> {
    send(dev, REPORT_SHORT, &[0x02, mode])
}

/// Read `0x11 0x03`: bond, vid, pid, link, game mode.
pub fn basics(dev: &HidRaw) -> io::Result<(u16, u16, u8, u8)> {
    let d = request(dev, REPORT_SHORT, &[0x03])?;
    if d.len() < 7 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short basics reply"));
    }
    Ok((u16le(&d, 1), u16le(&d, 3), d[5], d[6]))
}

/// `0x11 0x58`.
pub fn set_profile(dev: &HidRaw, profile: u8) -> io::Result<()> {
    send(dev, REPORT_SHORT, &[0x58, profile])
}

/// Ripple control, angle snapping and motion sync, applied through the
/// vendor's own command and then reconciled with the config block.
///
/// `0x11 0x42` turns them on but will not turn them off: sending zeroes leaves
/// the bits standing. The block write does clear them (all but bit 0, which
/// belongs to the mouse), so use both and let the block have the last word.
pub fn set_flags(
    dev: &HidRaw,
    lod: u8,
    ripple: bool,
    angle_snap: bool,
    motion_sync: bool,
) -> io::Result<()> {
    set_flags_from(dev, &Config::read(dev)?, lod, ripple, angle_snap, motion_sync)
}

/// Same, when the caller has just read the block and need not read it twice.
pub fn set_flags_from(
    dev: &HidRaw,
    before: &Config,
    lod: u8,
    ripple: bool,
    angle_snap: bool,
    motion_sync: bool,
) -> io::Result<()> {
    store_lod(lod);
    send(
        dev,
        REPORT_SHORT,
        &[0x42, lod, ripple as u8, angle_snap as u8, motion_sync as u8, 0, 0, 0, 0, before.rotate()],
    )?;
    sleep(Duration::from_millis(120));

    let wanted = (before.sensor() & !0x1C)
        | (ripple as u8) << 2
        | (angle_snap as u8) << 3
        | (motion_sync as u8) << 4;
    let mut after = Config::read(dev)?;
    if after.sensor() == wanted {
        return Ok(());
    }
    after.set_sensor(wanted);
    after.write(dev)?;
    for _ in 0..5 {
        sleep(Duration::from_millis(120));
        if Config::read(dev)?.sensor() == wanted {
            return Ok(());
        }
    }
    logln!("flags: sensor would not settle on {wanted:02x}");
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the mouse would not take those flags",
    ))
}

/// Report rates an 8 kHz model accepts, by index.
pub const RATES: [u32; 6] = [125, 500, 1000, 2000, 4000, 8000];

pub fn rate_index(hz: u32) -> Option<u8> {
    RATES.iter().position(|r| *r == hz).map(|i| i as u8)
}

pub fn rate_hz(index: u8) -> Option<u32> {
    RATES.get(index as usize).copied()
}

/// Lift-off has no read command, so the last value written is kept on disk.
pub fn lod_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("mchose/lod")
}

pub fn stored_lod() -> u8 {
    std::fs::read_to_string(lod_path())
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

pub fn store_lod(v: u8) {
    let path = lod_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

/// Read the config block back until it matches what was written, allowing for
/// the bytes the mouse keeps for itself.
pub fn confirm(dev: &HidRaw, wrote: &Config) -> io::Result<Config> {
    let mut last = None;
    for attempt in 0..5 {
        sleep(Duration::from_millis(80 * (attempt + 1)));
        let after = Config::read(dev)?;
        if after.matches(wrote) {
            return Ok(after);
        }
        last = Some(after);
    }
    let diff = last.as_ref().map(|c| wrote.diff(c)).unwrap_or_default();
    logln!("write not confirmed, diff {diff}");
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the mouse did not take the change ({diff})"),
    ))
}
