//! Named sets of settings, applied in as few round trips as the mouse allows.

use crate::hidraw::HidRaw;
use crate::proto::{self, Config, DPI_STAGES};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    /// Which of the six DPI stages this preset lives on, and what it holds.
    /// Presets ride the existing ladder rather than rewriting all six, so the
    /// DPI button on the mouse keeps working and nothing else is lost.
    pub stage: u8,
    pub dpi: u16,
    pub rate_hz: u32,
    /// 0 = 1 mm, 1 = 2 mm
    pub lod: u8,
    pub motion_sync: bool,
    pub ripple: bool,
    pub angle_snap: bool,
    pub debounce_ms: u8,
    pub sleep_min: u8,
    pub game_mode: u8,
}

/// What competitive Counter-Strike players run, on the mouse side.
///
/// 800 DPI and 1000 Hz is the settled pro standard. Everything that smooths,
/// predicts or delays is off: motion sync costs about a millisecond by pinning
/// sensor reads to the polling clock, ripple control is smoothing, and angle
/// snapping invents straight lines you did not draw. The lowest lift-off keeps
/// the crosshair still while you re-centre the mouse. Sleep after five idle
/// minutes keeps a forgotten mouse from staying awake overnight.
pub const CS: Preset = Preset {
    stage: 1,
    dpi: 800,
    rate_hz: 1000,
    lod: 0,
    motion_sync: false,
    ripple: false,
    angle_snap: false,
    debounce_ms: 3,
    sleep_min: 5,
    game_mode: 3,
};

/// A day at the desk on a 4K panel.
///
/// The opposite trade: smoothing on, because pointing at a 3 px handle beats
/// three milliseconds of click latency; a slower poll and a real sleep timer,
/// because this preset runs all day on a battery; and a debounce high enough
/// that a tired switch never double-fires in a file manager.
pub const DESK: Preset = Preset {
    stage: 2,
    dpi: 1600,
    rate_hz: 500,
    lod: 1,
    motion_sync: true,
    ripple: true,
    angle_snap: false,
    debounce_ms: 10,
    sleep_min: 5,
    game_mode: 1,
};

pub fn builtin() -> [(&'static str, Preset); 2] {
    [("cs", CS), ("desk", DESK)]
}

/// Built-ins plus anything saved, saved wins on a name clash.
pub fn all() -> BTreeMap<String, Preset> {
    let mut out: BTreeMap<String, Preset> =
        builtin().into_iter().map(|(n, p)| (n.to_string(), p)).collect();
    out.extend(load_saved());
    out
}

pub fn get(name: &str) -> Option<Preset> {
    all().get(&name.to_lowercase()).copied()
}

/// Apply the whole preset. The block-borne settings go in one write; rate,
/// flags and game mode have their own commands and cannot be folded in.
pub fn apply(dev: &HidRaw, p: &Preset) -> io::Result<()> {
    logln!("preset: applying {p:?}");
    let stage = (p.stage as usize).min(DPI_STAGES - 1);

    // Everything the config block carries goes in a single write, then the
    // three settings that have their own commands. One read, one block write,
    // one confirm: applying a preset used to take fifteen exchanges and choke
    // the mouse.
    let mut c = Config::read(dev)?;
    c.set_dpi(stage, p.dpi);
    c.set_dpi_stage(stage as u8);
    c.set_debounce_ms(p.debounce_ms);
    c.set_sleep_minutes(p.sleep_min);
    c.write(dev)?;
    let after = proto::confirm(dev, &c)?;

    if let Some(index) = proto::rate_index(p.rate_hz) {
        proto::set_report_rate(dev, index, index)?;
    }
    proto::set_flags_from(dev, &after, p.lod, p.ripple, p.angle_snap, p.motion_sync)?;
    proto::set_game_mode(dev, p.game_mode)?;
    Ok(())
}

/// How the mouse is set right now, so it can be saved or matched.
pub fn current(dev: &HidRaw) -> io::Result<Preset> {
    let c = Config::read(dev)?;
    let (ripple, angle_snap, motion_sync) = c.perf_flags();
    let stage = c.wireless_dpi_stage().min(DPI_STAGES as u8 - 1);
    Ok(Preset {
        stage,
        dpi: c.dpi(stage as usize),
        rate_hz: proto::rate_hz(c.wireless_rate_index()).unwrap_or(0),
        lod: crate::proto::stored_lod(),
        motion_sync,
        ripple,
        angle_snap,
        debounce_ms: c.debounce_ms(),
        sleep_min: c.sleep_minutes(),
        game_mode: proto::basics(dev).map(|b| b.3).unwrap_or(0),
    })
}

// ------------------------------------------------------------- saved presets

pub fn path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("mchose/presets.conf")
}

fn load_saved() -> BTreeMap<String, Preset> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path()) else {
        return out;
    };
    let mut name: Option<String> = None;
    let mut fields: BTreeMap<String, i64> = BTreeMap::new();
    let mut flush = |name: &mut Option<String>, fields: &mut BTreeMap<String, i64>| {
        if let Some(n) = name.take() {
            let get = |k: &str, d: i64| *fields.get(k).unwrap_or(&d);
            out.insert(
                n,
                Preset {
                    stage: get("stage", 0) as u8,
                    dpi: get("dpi", 800) as u16,
                    rate_hz: get("rate", 1000) as u32,
                    lod: get("lod", 0) as u8,
                    motion_sync: get("motion_sync", 0) != 0,
                    ripple: get("ripple", 0) != 0,
                    angle_snap: get("angle_snap", 0) != 0,
                    debounce_ms: get("debounce", 8) as u8,
                    sleep_min: get("sleep", 3) as u8,
                    game_mode: get("game_mode", 1) as u8,
                },
            );
        }
        fields.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&mut name, &mut fields);
            name = Some(section.trim().to_lowercase());
        } else if let Some((k, v)) = line.split_once('=') {
            if let Ok(v) = v.trim().parse::<i64>() {
                fields.insert(k.trim().to_string(), v);
            }
        }
    }
    flush(&mut name, &mut fields);
    out
}

pub fn save(name: &str, p: &Preset) -> io::Result<()> {
    let mut saved = load_saved();
    saved.insert(name.to_lowercase(), *p);
    let mut text = String::from("# mchose presets. Edit freely; a name here shadows a built-in.\n");
    for (n, p) in &saved {
        text.push_str(&format!(
            "\n[{n}]\nstage={}\ndpi={}\nrate={}\nlod={}\nmotion_sync={}\nripple={}\nangle_snap={}\ndebounce={}\nsleep={}\ngame_mode={}\n",
            p.stage, p.dpi, p.rate_hz, p.lod, p.motion_sync as u8, p.ripple as u8,
            p.angle_snap as u8, p.debounce_ms, p.sleep_min, p.game_mode
        ));
    }
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn built_in_presets_never_disable_idle_sleep() {
        for (name, preset) in builtin() {
            assert!(preset.sleep_min > 0, "{name} disables automatic sleep");
        }
    }
}
