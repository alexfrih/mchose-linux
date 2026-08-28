//! mchose: configure MCHOSE mice on Linux.
//!
//! There is no vendor Linux app and libratbag does not know these mice. The
//! protocol was recovered from the M HUB web driver; PROTOCOL.md documents it.

use mchose::hidraw::{self, HidRaw};
use mchose::preset::{self, Preset};
use mchose::proto::{self, Config, DPI_STAGES, RATES};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

/// Vendors the M HUB driver treats with this protocol.
const VENDORS: [u16; 2] = [0x5253, 0x3837];

const USAGE: &str = "\
mchose - configure MCHOSE mice on Linux

  mchose info                    device, firmware, battery, link
  mchose show [--raw]            DPI stages, report rate, debounce, sleep
  mchose devices                 hidraw nodes that speak this protocol

  mchose dpi <value>             set the active DPI stage
  mchose dpi <value> --stage N   set stage N (0-5)
  mchose dpi --list a,b,c,d,e,f  set all six stages at once
  mchose stage <0-5>             switch the active stage
  mchose rate <hz>               125 500 1000 2000 4000 8000
  mchose debounce <ms>
  mchose sleep <minutes>         0 disables
  mchose profile <0-2>

  mchose lod <mm>                lift-off distance, 1 or 2
  mchose motion-sync <on|off>
  mchose ripple <on|off>
  mchose angle-snap <on|off>
  mchose game-mode <1|2|3>

  mchose backup [file]           save the config block
  mchose restore <file>          write a saved config block back
  mchose preset                  list the presets, mark the one in effect
  mchose preset cs               competitive Counter-Strike
  mchose preset desk             a day at the desk
  mchose preset save <name>      keep the current settings under a name

  mchose log [lines]             the last frames sent and received
  mchose raw <11|12> <hex...>    send one frame, print the reply

Options: --device /dev/hidrawN   skip autodetection

Lift-off is the one setting the mouse never reports back, so the last value
written is kept in ~/.local/state/mchose/. Everything else is read from the
mouse itself.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mchose: {e}");
            ExitCode::FAILURE
        }
    }
}

type R = Result<(), String>;

fn run(args: &[String]) -> R {
    let mut args: Vec<String> = args.to_vec();
    let explicit = take_option(&mut args, "--device");
    let raw_flag = take_flag(&mut args, "--raw");
    let stage_opt = take_option(&mut args, "--stage");
    let list_opt = take_option(&mut args, "--list");

    let command = args.first().map(String::as_str).unwrap_or("info");
    if matches!(command, "-h" | "--help" | "help") {
        print!("{USAGE}");
        return Ok(());
    }
    if command == "devices" {
        return devices();
    }
    if command == "log" {
        let text = fs::read_to_string(mchose::log::path()).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let n = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(60usize);
        for line in lines.iter().rev().take(n).rev() {
            println!("{line}");
        }
        return Ok(());
    }

    let dev = open_device(explicit.as_deref())?;

    match command {
        "info" => info(&dev),
        "show" => show(&dev, raw_flag),
        "dpi" => {
            if let Some(list) = list_opt {
                dpi_list(&dev, &list)
            } else {
                let value: u16 = num(args.get(1), "dpi <value>")?;
                let stage = match stage_opt {
                    Some(s) => Some(s.parse::<u8>().map_err(|_| "--stage takes 0-5")?),
                    None => None,
                };
                dpi(&dev, value, stage)
            }
        }
        "stage" => stage(&dev, num(args.get(1), "stage <0-5>")?),
        "rate" => rate(&dev, num(args.get(1), "rate <hz>")?),
        "debounce" => debounce(&dev, num(args.get(1), "debounce <ms>")?),
        "sleep" => sleep_minutes(&dev, num(args.get(1), "sleep <minutes>")?),
        "profile" => {
            let p: u8 = num(args.get(1), "profile <0-2>")?;
            proto::set_profile(&dev, p).map_err(io)?;
            println!("profile {p}");
            Ok(())
        }
        "lod" => {
            let mm: u8 = num(args.get(1), "lod <mm>")?;
            let index = match mm {
                1 => 0,
                2 => 1,
                _ => return Err("lod takes 1 or 2 (mm)".into()),
            };
            flags(&dev, |lod, _, _, _| *lod = index)?;
            println!("lift-off distance {mm} mm");
            Ok(())
        }
        "motion-sync" => {
            let on = onoff(args.get(1))? == 1;
            flags(&dev, |_, _, _, sync| *sync = on)?;
            println!("motion sync {}", onoff_str(on));
            Ok(())
        }
        "ripple" => {
            let on = onoff(args.get(1))? == 1;
            flags(&dev, |_, r, _, _| *r = on)?;
            println!("ripple control {}", onoff_str(on));
            Ok(())
        }
        "angle-snap" => {
            let on = onoff(args.get(1))? == 1;
            flags(&dev, |_, _, a, _| *a = on)?;
            println!("angle snapping {}", onoff_str(on));
            Ok(())
        }
        "game-mode" => {
            let mode: u8 = num(args.get(1), "game-mode <1|2|3>")?;
            if !(1..=3).contains(&mode) {
                return Err("game-mode takes 1, 2 or 3".into());
            }
            proto::set_game_mode(&dev, mode).map_err(io)?;
            std::thread::sleep(std::time::Duration::from_millis(60));
            let (_, _, _, got) = proto::basics(&dev).map_err(io)?;
            if got != mode {
                return Err(format!("the mouse did not take game mode {mode} (it reports {got})"));
            }
            println!("game mode {mode}");
            Ok(())
        }
        "backup" => backup(&dev, args.get(1).map(PathBuf::from)),
        "restore" => restore(&dev, args.get(1).ok_or("usage: mchose restore <file>")?),
        "preset" => match args.get(1).map(String::as_str) {
            None => list_presets(&dev),
            Some("save") => {
                let name = args.get(2).ok_or("usage: mchose preset save <name>")?;
                let now = preset::current(&dev).map_err(io)?;
                preset::save(name, &now).map_err(io)?;
                println!("saved preset {name:?} to {}", preset::path().display());
                Ok(())
            }
            Some(name) => {
                let p = preset::get(name)
                    .ok_or_else(|| format!("no preset {name:?}. Try: mchose preset"))?;
                preset::apply(&dev, &p).map_err(io)?;
                println!("{name}");
                describe(&p, "  ");
                Ok(())
            }
        },
        "raw" => raw(&dev, &args[1..]),
        other => Err(format!("unknown command {other:?}, try --help")),
    }
}

// ------------------------------------------------------------------ device

fn candidates() -> Result<Vec<hidraw::Node>, String> {
    let nodes = hidraw::nodes().map_err(io)?;
    Ok(nodes
        .into_iter()
        .filter(|n| VENDORS.contains(&n.vid) && hidraw::has_config_collection(&n.descriptor))
        .collect())
}

fn devices() -> R {
    let found = candidates()?;
    if found.is_empty() {
        println!("no MCHOSE configuration interface found");
        return Ok(());
    }
    for n in found {
        println!("{}  {:04x}:{:04x}  {}", n.dev.display(), n.vid, n.pid, n.name);
    }
    Ok(())
}

fn open_device(explicit: Option<&str>) -> Result<HidRaw, String> {
    if let Some(path) = explicit {
        return HidRaw::open(std::path::Path::new(path)).map_err(|e| open_hint(path, e));
    }
    let found = candidates()?;
    let node = found
        .first()
        .ok_or("no MCHOSE configuration interface found. Is the mouse or its dongle plugged in?")?;
    HidRaw::open(&node.dev).map_err(|e| open_hint(&node.dev.to_string_lossy(), e))
}

fn open_hint(path: &str, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "cannot open {path}: permission denied.\n\
             Install the udev rule once, then replug:\n  \
             sudo cp 70-mchose.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger"
        )
    } else {
        format!("cannot open {path}: {e}")
    }
}

// ----------------------------------------------------------------- reading

fn info(dev: &HidRaw) -> R {
    let id = proto::identity(dev).map_err(io)?;
    let version = proto::version_string(dev).unwrap_or_default();
    println!("device      {:04x}:{:04x}", id.vid, id.pid);
    if version.is_empty() {
        println!("firmware    {:#010x}", id.firmware);
    } else {
        println!("firmware    {version}");
    }
    println!(
        "link        {} ({})",
        link_name(id.connect_mode),
        if id.connected { "online" } else { "offline" }
    );
    println!(
        "battery     {} %{}",
        id.battery,
        if id.charging != 0 { ", charging" } else { "" }
    );
    Ok(())
}

fn link_name(mode: u8) -> &'static str {
    match mode {
        0 => "wired",
        1 => "2.4 GHz",
        2 => "bluetooth",
        _ => "unknown",
    }
}

fn show(dev: &HidRaw, raw_flag: bool) -> R {
    let c = Config::read(dev).map_err(io)?;
    let active = c.wireless_dpi_stage() as usize;
    println!("profile     {}", c.profile());
    println!("stages      {} enabled", c.enabled_stages());
    for s in 0..DPI_STAGES {
        let mark = if s == active { "*" } else { " " };
        let y = c.dpi_y(s);
        let axis = if y != 0 && y != c.dpi(s) { format!("  (Y {y})") } else { String::new() };
        println!("  {mark} {s}       {} dpi{axis}", c.dpi(s));
    }
    println!("rate        {} (wired), {} (wireless)", rate_name(c.wired_rate_index()), rate_name(c.wireless_rate_index()));
    println!("debounce    {} ms", c.debounce_ms());
    println!(
        "sleep       {}",
        if c.sleep_minutes() == 0 { "off".to_string() } else { format!("{} min", c.sleep_minutes()) }
    );
    let (ripple, angle_snap, motion_sync) = c.perf_flags();
    println!("ripple      {}", onoff_str(ripple));
    println!("angle snap  {}", onoff_str(angle_snap));
    println!("motion sync {}", onoff_str(motion_sync));
    println!("sensor      {:#04x}", c.sensor());
    if let Ok((_, _, _, game)) = proto::basics(dev) {
        println!("game mode   {game}");
    }
    println!("rotate      {}", c.rotate());
    println!(
        "lod         {} mm (write-only, from this tool's state)",
        if proto::stored_lod() == 0 { 1 } else { 2 }
    );
    if raw_flag {
        println!("\nraw config  {}", proto::hex(&c.body));
    }
    Ok(())
}

fn rate_name(index: u8) -> String {
    RATES
        .get(index as usize)
        .map(|hz| format!("{hz} Hz"))
        .unwrap_or_else(|| format!("index {index}"))
}

// ----------------------------------------------------------------- writing

/// Read, change, write back, then read again and confirm it landed.
fn edit<F: Fn(&mut Config)>(dev: &HidRaw, change: F) -> Result<Config, String> {
    backup_once(dev)?;
    let mut c = Config::read(dev).map_err(io)?;
    change(&mut c);
    c.write(dev).map_err(io)?;
    proto::confirm(dev, &c).map_err(io)
}

fn dpi(dev: &HidRaw, value: u16, stage: Option<u8>) -> R {
    check_dpi(value)?;
    let current = Config::read(dev).map_err(io)?;
    let target = stage.unwrap_or(current.wireless_dpi_stage()) as usize;
    if target >= DPI_STAGES {
        return Err(format!("stage must be 0-{}", DPI_STAGES - 1));
    }
    edit(dev, |c| c.set_dpi(target, value))?;
    println!("stage {target} set to {value} dpi");
    Ok(())
}

fn dpi_list(dev: &HidRaw, list: &str) -> R {
    let values: Vec<u16> = list
        .split(',')
        .map(|v| v.trim().parse::<u16>().map_err(|_| format!("{v:?} is not a DPI value")))
        .collect::<Result<_, _>>()?;
    if values.len() != DPI_STAGES {
        return Err(format!("--list takes exactly {DPI_STAGES} comma-separated values"));
    }
    for v in &values {
        check_dpi(*v)?;
    }
    edit(dev, |c| {
        for (s, v) in values.iter().enumerate() {
            c.set_dpi(s, *v);
        }
        c.set_enabled_stages(DPI_STAGES as u8);
    })?;
    println!("stages set to {}", values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "));
    Ok(())
}

fn check_dpi(value: u16) -> R {
    if value < 50 || value > 26000 {
        return Err(format!("{value} dpi is outside the sensor range (50 to 26000)"));
    }
    if value % 50 != 0 {
        return Err(format!("{value} dpi is not a multiple of 50"));
    }
    Ok(())
}

fn stage(dev: &HidRaw, n: u8) -> R {
    if n as usize >= DPI_STAGES {
        return Err(format!("stage must be 0-{}", DPI_STAGES - 1));
    }
    let c = edit(dev, |c| c.set_dpi_stage(n))?;
    println!("active stage {n} ({} dpi)", c.dpi(n as usize));
    Ok(())
}

fn rate(dev: &HidRaw, hz: u32) -> R {
    let index = RATES
        .iter()
        .position(|r| *r == hz)
        .ok_or_else(|| format!("rate must be one of {}", RATES.map(|r| r.to_string()).join(", ")))?
        as u8;
    backup_once(dev)?;
    // `0x11 0x41` owns the rate; the config block only mirrors it, so read it
    // back rather than writing both and letting them fight.
    proto::set_report_rate(dev, index, index).map_err(io)?;
    std::thread::sleep(std::time::Duration::from_millis(60));
    let c = Config::read(dev).map_err(io)?;
    if c.wired_rate_index() != index && c.wireless_rate_index() != index {
        return Err(format!(
            "the mouse did not take {hz} Hz (it still reports {} wired, {} wireless)",
            rate_name(c.wired_rate_index()),
            rate_name(c.wireless_rate_index())
        ));
    }
    println!("report rate {hz} Hz");
    Ok(())
}

fn debounce(dev: &HidRaw, ms: u8) -> R {
    if ms > 30 {
        return Err("debounce is 0 to 30 ms".into());
    }
    edit(dev, |c| c.set_debounce_ms(ms))?;
    println!("debounce {ms} ms");
    Ok(())
}

fn sleep_minutes(dev: &HidRaw, minutes: u8) -> R {
    edit(dev, |c| c.set_sleep_minutes(minutes))?;
    if minutes == 0 {
        println!("sleep disabled");
    } else {
        println!("sleep after {minutes} min");
    }
    Ok(())
}

// ------------------------------------------------------------------ backup

fn state_dir() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("mchose")
}

fn backup(dev: &HidRaw, to: Option<PathBuf>) -> R {
    let c = Config::read(dev).map_err(io)?;
    let path = to.unwrap_or_else(|| state_dir().join("config.bin"));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io)?;
    }
    fs::write(&path, c.body).map_err(io)?;
    println!("saved {}", path.display());
    Ok(())
}

/// Keep one copy of the config as it was before this tool ever wrote to it.
fn backup_once(dev: &HidRaw) -> R {
    let path = state_dir().join("config.original.bin");
    if path.exists() {
        return Ok(());
    }
    let c = Config::read(dev).map_err(io)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io)?;
    }
    fs::write(&path, c.body).map_err(io)?;
    eprintln!("note: saved the config as it was to {}", path.display());
    Ok(())
}

fn restore(dev: &HidRaw, from: &str) -> R {
    let body = fs::read(from).map_err(io)?;
    if body.len() != 63 {
        return Err(format!("{from} is {} bytes, expected 63", body.len()));
    }
    let mut c = Config::read(dev).map_err(io)?;
    c.body.copy_from_slice(&body);
    c.write(dev).map_err(io)?;
    proto::confirm(dev, &c).map_err(io)?;
    println!("restored from {from}");
    Ok(())
}

// ------------------------------------------------------------------- flags

/// The four sensor flags travel together in one command, so read the live set,
/// change the one asked for, and send them all.
fn flags<F: Fn(&mut u8, &mut bool, &mut bool, &mut bool)>(dev: &HidRaw, change: F) -> R {
    let c = Config::read(dev).map_err(io)?;
    let (mut ripple, mut angle_snap, mut motion_sync) = c.perf_flags();
    let mut lod = proto::stored_lod();
    change(&mut lod, &mut ripple, &mut angle_snap, &mut motion_sync);
    proto::set_flags(dev, lod, ripple, angle_snap, motion_sync).map_err(io)
}

// ----------------------------------------------------------------- presets

fn list_presets(dev: &HidRaw) -> R {
    let now = preset::current(dev).ok();
    for (name, p) in preset::all() {
        let live = now.as_ref() == Some(&p);
        println!("{} {name}", if live { "*" } else { " " });
        describe(&p, "    ");
        println!();
    }
    if now.is_none() {
        return Ok(());
    }
    if !preset::all().values().any(|p| now.as_ref() == Some(p)) {
        println!("  the mouse is on none of these. `mchose preset save <name>` keeps it.");
    }
    Ok(())
}

fn describe(p: &Preset, pad: &str) {
    println!("{pad}{} dpi on stage {}, {} Hz", p.dpi, p.stage, p.rate_hz);
    println!(
        "{pad}lift-off {} mm, motion sync {}, ripple {}, angle snap {}",
        if p.lod == 0 { 1 } else { 2 },
        onoff_str(p.motion_sync),
        onoff_str(p.ripple),
        onoff_str(p.angle_snap)
    );
    println!(
        "{pad}debounce {} ms, sleep {}, game mode {}",
        p.debounce_ms,
        if p.sleep_min == 0 { "off".into() } else { format!("{} min", p.sleep_min) },
        p.game_mode
    );
}

// --------------------------------------------------------------------- raw

fn raw(dev: &HidRaw, args: &[String]) -> R {
    let report = args
        .first()
        .and_then(|v| u8::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .ok_or("raw <11|12> <hex bytes...>")?;
    let body: Vec<u8> = args[1..]
        .iter()
        .map(|v| u8::from_str_radix(v.trim_start_matches("0x"), 16).map_err(|_| format!("{v:?} is not a hex byte")))
        .collect::<Result<_, _>>()?;
    if body.is_empty() {
        return Err("give at least the command byte".into());
    }
    match proto::request(dev, report, &body) {
        Ok(reply) => println!("{}", proto::hex(&reply)),
        Err(e) => {
            proto::send(dev, report, &body).map_err(io)?;
            println!("sent, no matching reply ({e})");
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- utils

fn onoff_str(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

fn num<T: std::str::FromStr>(arg: Option<&String>, what: &str) -> Result<T, String> {
    arg.ok_or_else(|| format!("usage: mchose {what}"))?
        .parse()
        .map_err(|_| format!("usage: mchose {what}"))
}

fn onoff(arg: Option<&String>) -> Result<u8, String> {
    match arg.map(String::as_str) {
        Some("on" | "1" | "true") => Ok(1),
        Some("off" | "0" | "false") => Ok(0),
        _ => Err("takes on or off".into()),
    }
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == name) {
        args.remove(i);
        true
    } else {
        false
    }
}

fn take_option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name || a.starts_with(&format!("{name}=")))?;
    let arg = args.remove(i);
    if let Some(v) = arg.strip_prefix(&format!("{name}=")) {
        return Some(v.to_string());
    }
    if i < args.len() {
        Some(args.remove(i))
    } else {
        None
    }
}
