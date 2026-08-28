//! One window for the mouse. Everything applies as you touch it.

use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke, Vec2};
use mchose::hidraw::{self, HidRaw};
use mchose::logln;
use mchose::preset::{self, Preset};
use mchose::proto::{self, Config, DPI_STAGES};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

const RATES: [u32; 6] = [125, 500, 1000, 2000, 4000, 8000];

// Taken from the app icon, not invented: the plate tones it sits on, the violet
// it glows with, and the hot core of that glow. One accent, no shadows.
const BG: Color32 = Color32::from_rgb(0x0B, 0x09, 0x10);
const CARD: Color32 = Color32::from_rgb(0x15, 0x12, 0x21);
const CHIP: Color32 = Color32::from_rgb(0x1F, 0x1B, 0x2E);
const LINE: Color32 = Color32::from_rgb(0x2E, 0x27, 0x45);
const TEXT: Color32 = Color32::from_rgb(0xED, 0xE9, 0xF5);
const MUTED: Color32 = Color32::from_rgb(0x87, 0x80, 0x99);
const ACCENT: Color32 = Color32::from_rgb(0x78, 0x3E, 0xCD);
/// The hot core of the icon's glow, for the one number that carries the screen.
const HOT: Color32 = Color32::from_rgb(0xD7, 0x9B, 0xFC);
const BAD: Color32 = Color32::from_rgb(0xE0, 0x62, 0x5A);

// ------------------------------------------------------------------ worker

#[derive(Clone, Default)]
struct Snapshot {
    present: bool,
    model: String,
    firmware: String,
    link: String,
    online: bool,
    battery: u8,
    charging: bool,
    dpi: [u16; DPI_STAGES],
    stage: u8,
    rate: u8,
    debounce: u8,
    sleep: u8,
    ripple: bool,
    angle_snap: bool,
    motion_sync: bool,
    lod: u8,
    game_mode: u8,
    /// Name of the preset the mouse currently matches, if any.
    preset: Option<String>,
}

enum Cmd {
    Preset(String),
    Refresh,
    Dpi(usize, u16),
    Stage(u8),
    Rate(u8),
    Debounce(u8),
    Sleep(u8),
    Flags { lod: u8, ripple: bool, angle_snap: bool, motion_sync: bool },
    GameMode(u8),
}

enum Msg {
    State(Box<Snapshot>),
    Error(String),
}

fn worker(rx: Receiver<Cmd>, tx: Sender<Msg>, ctx: egui::Context) {
    let mut lod = load_lod();
    while let Ok(cmd) = rx.recv() {
        let result = (|| -> Result<Snapshot, String> {
            let dev = open().map_err(|e| e)?;
            match cmd {
                Cmd::Refresh => {}
                Cmd::Preset(ref name) => {
                    let p = preset::get(name).ok_or_else(|| format!("no preset {name:?}"))?;
                    preset::apply(&dev, &p).map_err(str_err)?;
                    lod = p.lod;
                }
                Cmd::Dpi(stage, value) => edit(&dev, |c| c.set_dpi(stage, value))?,
                Cmd::Stage(n) => edit(&dev, |c| c.set_dpi_stage(n))?,
                Cmd::Rate(index) => {
                    proto::set_report_rate(&dev, index, index).map_err(str_err)?;
                }
                Cmd::Debounce(ms) => edit(&dev, |c| c.set_debounce_ms(ms))?,
                Cmd::Sleep(min) => edit(&dev, |c| c.set_sleep_minutes(min))?,
                Cmd::Flags { lod: l, ripple, angle_snap, motion_sync } => {
                    proto::set_flags(&dev, l, ripple, angle_snap, motion_sync).map_err(str_err)?;
                    lod = l;
                    save_lod(l);
                }
                Cmd::GameMode(m) => proto::set_game_mode(&dev, m).map_err(str_err)?,
            }
            read(&dev, lod)
        })();
        let msg = match result {
            Ok(s) => Msg::State(Box::new(s)),
            Err(e) => Msg::Error(e),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    }
}

fn str_err(e: std::io::Error) -> String {
    e.to_string()
}

fn open() -> Result<HidRaw, String> {
    let nodes = hidraw::nodes().map_err(str_err)?;
    let node = nodes
        .into_iter()
        .find(|n| matches!(n.vid, 0x5253 | 0x3837) && hidraw::has_config_collection(&n.descriptor))
        .ok_or("No MCHOSE mouse found. Plug in the dongle.")?;
    HidRaw::open(&node.dev).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!("{} is not readable. Run install.sh once.", node.dev.display())
        } else {
            format!("{}: {e}", node.dev.display())
        }
    })
}

fn edit<F: Fn(&mut Config)>(dev: &HidRaw, change: F) -> Result<(), String> {
    let mut c = Config::read(dev).map_err(str_err)?;
    change(&mut c);
    c.write(dev).map_err(str_err)?;
    let mut last = None;
    for _ in 0..5 {
        thread::sleep(std::time::Duration::from_millis(90));
        let after = Config::read(dev).map_err(str_err)?;
        if after.matches(&c) {
            return Ok(());
        }
        last = Some(after);
    }
    let diff = last.as_ref().map(|a| c.diff(a)).unwrap_or_default();
    logln!("gui: write not confirmed, diff {diff}");
    Err(format!("the mouse did not take that change ({diff})"))
}

fn read(dev: &HidRaw, lod: u8) -> Result<Snapshot, String> {
    let id = proto::identity(dev).map_err(str_err)?;
    let c = Config::read(dev).map_err(str_err)?;
    let (ripple, angle_snap, motion_sync) = c.perf_flags();
    let mut dpi = [0u16; DPI_STAGES];
    for (s, slot) in dpi.iter_mut().enumerate() {
        *slot = c.dpi(s);
    }
    let game_mode = proto::basics(dev).map(|b| b.3).unwrap_or(0);
    let live = Preset {
        stage: c.wireless_dpi_stage().min(DPI_STAGES as u8 - 1),
        dpi: c.dpi((c.wireless_dpi_stage() as usize).min(DPI_STAGES - 1)),
        rate_hz: proto::rate_hz(c.wireless_rate_index()).unwrap_or(0),
        lod,
        motion_sync,
        ripple,
        angle_snap,
        debounce_ms: c.debounce_ms(),
        sleep_min: c.sleep_minutes(),
        game_mode,
    };
    let preset = preset::all().into_iter().find(|(_, p)| *p == live).map(|(n, _)| n);
    Ok(Snapshot {
        present: true,
        model: model_name(id.pid),
        firmware: proto::version_string(dev).unwrap_or_default(),
        link: match id.connect_mode {
            0 => "wired",
            1 => "2.4 GHz",
            2 => "bluetooth",
            _ => "unknown",
        }
        .into(),
        online: id.connected,
        battery: id.battery,
        charging: id.charging != 0,
        dpi,
        stage: c.wireless_dpi_stage(),
        rate: c.wireless_rate_index(),
        debounce: c.debounce_ms(),
        sleep: c.sleep_minutes(),
        ripple,
        angle_snap,
        motion_sync,
        lod,
        game_mode,
        preset,
    })
}

/// The driver's own table, for the ones that reach this protocol.
fn model_name(pid: u16) -> String {
    match pid {
        0x00b0 => "MCHOSE L7 Pro".into(),
        0x00b1 => "MCHOSE L7 Ultra".into(),
        0x00c0 => "MCHOSE L7".into(),
        0x0020 => "MCHOSE M7".into(),
        0x0030 => "MCHOSE M7 Pro".into(),
        0x0031 => "MCHOSE M7 Ultra".into(),
        0x0070 => "MCHOSE A7".into(),
        other => format!("MCHOSE {other:04x}"),
    }
}

fn lod_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("mchose/lod")
}

fn load_lod() -> u8 {
    std::fs::read_to_string(lod_path())
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

fn save_lod(v: u8) {
    let path = lod_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

// --------------------------------------------------------------------- app

struct App {
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
    state: Snapshot,
    error: Option<String>,
    busy: bool,
    /// One command in flight at a time; a newer one replaces whatever is
    /// waiting. Queuing every slider tick would have the mouse chasing values
    /// the user has already moved past.
    pending: Option<Cmd>,
    /// While a slider is held we show the local value, not the last read.
    dragging_dpi: Option<u16>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let (msg_tx, msg_rx) = channel();
        let c = ctx.clone();
        thread::spawn(move || worker(cmd_rx, msg_tx, c));
        let _ = cmd_tx.send(Cmd::Refresh);
        Self {
            tx: cmd_tx,
            rx: msg_rx,
            state: Snapshot::default(),
            error: None,
            busy: true,
            pending: None,
            dragging_dpi: None,
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if self.busy {
            self.pending = Some(cmd);
            return;
        }
        self.busy = true;
        let _ = self.tx.send(cmd);
    }

    fn drain(&mut self) {
        let mut got = false;
        while let Ok(msg) = self.rx.try_recv() {
            got = true;
            self.busy = false;
            match msg {
                Msg::State(s) => {
                    self.state = *s;
                    self.error = None;
                }
                Msg::Error(e) => {
                    logln!("gui: {e}");
                    // Keep whatever we last read on screen. Blanking the window
                    // on a failed write leaves no way to undo it.
                    self.error = Some(e);
                }
            }
        }
        if got {
            if let Some(cmd) = self.pending.take() {
                self.busy = true;
                let _ = self.tx.send(cmd);
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        Frame::new()
            .fill(BG)
            .inner_margin(Margin::symmetric(26, 22))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                self.header(ui);
                ui.add_space(16.0);
                if let Some(e) = self.error.clone() {
                    self.notice(ui, &e);
                    ui.add_space(12.0);
                }
                if !self.state.present {
                    ui.label(RichText::new("Reading the mouse…").color(MUTED));
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 8.0);
                    self.presets(ui);
                    ui.add_space(10.0);
                    self.dpi_section(ui);
                    ui.add_space(10.0);
                    self.rate_section(ui);
                    ui.add_space(10.0);
                    self.rest(ui);
                });
            });
    }
}

impl App {
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if self.state.present { self.state.model.as_str() } else { "mchose" })
                    .color(TEXT)
                    .size(18.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.busy {
                    ui.add(egui::Spinner::new().size(12.0).color(MUTED));
                    ui.add_space(6.0);
                }
                if self.state.present {
                    let battery = format!(
                        "{} %{}",
                        self.state.battery,
                        if self.state.charging { " charging" } else { "" }
                    );
                    ui.label(RichText::new(battery).color(MUTED).size(12.0));
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(&self.state.link)
                            .color(if self.state.online { MUTED } else { BAD })
                            .size(12.0),
                    );
                }
            });
        });
        if self.state.present && !self.state.firmware.is_empty() {
            ui.label(RichText::new(format!("firmware {}", self.state.firmware)).color(MUTED).size(11.0));
        }
    }

    fn notice(&self, ui: &mut egui::Ui, text: &str) {
        card(ui, |ui| {
            ui.label(RichText::new(text).color(BAD).size(13.0));
        });
    }

    fn presets(&mut self, ui: &mut egui::Ui) {
        let names: Vec<String> = preset::all().into_keys().collect();
        if names.is_empty() {
            return;
        }
        section(ui, "Preset");
        card(ui, |ui| {
            let width = share(ui, names.len());
            let mut pick = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for name in &names {
                    let active = self.state.preset.as_ref() == Some(name);
                    if chip(ui, name, active, width).clicked() && !active {
                        pick = Some(name.clone());
                    }
                }
            });
            if self.state.preset.is_none() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("the mouse is on none of these").color(MUTED).size(11.0),
                );
            }
            if let Some(name) = pick {
                self.send(Cmd::Preset(name));
            }
        });
    }

    fn dpi_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "DPI");
        card(ui, |ui| {
            let width = share(ui, DPI_STAGES);
            let mut pick = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for s in 0..DPI_STAGES {
                    let active = self.state.stage as usize == s;
                    if chip(ui, &self.state.dpi[s].to_string(), active, width).clicked() && !active {
                        pick = Some(s as u8);
                    }
                }
            });
            if let Some(s) = pick {
                self.send(Cmd::Stage(s));
            }
            ui.add_space(14.0);

            let stage = (self.state.stage as usize).min(DPI_STAGES - 1);
            let mut value = self.dragging_dpi.unwrap_or(self.state.dpi[stage]);
            let response = ui.add(
                egui::Slider::new(&mut value, 50..=26000)
                    // Linear, every useful setting sits in the first tenth of
                    // the track; the sensor's range only reads well on a log.
                    .logarithmic(true)
                    .show_value(false)
                    .trailing_fill(true),
            );
            if response.dragged() {
                self.dragging_dpi = Some(value - value % 50);
            }
            if response.drag_stopped() || (response.changed() && !response.dragged()) {
                self.dragging_dpi = None;
                self.send(Cmd::Dpi(stage, (value - value % 50).max(50)));
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{value}")).color(HOT).size(38.0));
                ui.add_space(2.0);
                ui.label(RichText::new("dpi").color(MUTED).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("stage {stage}")).color(MUTED).size(12.0));
                });
            });
        });
    }

    fn rate_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "Report rate");
        card(ui, |ui| {
            let width = share(ui, RATES.len());
            let mut pick = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for (i, hz) in RATES.iter().enumerate() {
                    let active = self.state.rate as usize == i;
                    if chip(ui, &format!("{hz}"), active, width).clicked() && !active {
                        pick = Some(i as u8);
                    }
                }
            });
            if let Some(i) = pick {
                self.send(Cmd::Rate(i));
            }
        });
    }

    fn rest(&mut self, ui: &mut egui::Ui) {
        section(ui, "Sensor");
        card(ui, |ui| {
            let mut lod = self.state.lod;
            row(ui, "Lift-off", |ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for (i, label) in ["1 mm", "2 mm"].iter().enumerate().rev() {
                    if chip(ui, label, lod as usize == i, 56.0).clicked() {
                        lod = i as u8;
                    }
                }
            });
            let mut motion = self.state.motion_sync;
            let mut ripple = self.state.ripple;
            let mut snap = self.state.angle_snap;
            row(ui, "Motion sync", |ui| dot(ui, &mut motion));
            row(ui, "Ripple control", |ui| dot(ui, &mut ripple));
            row(ui, "Angle snapping", |ui| dot(ui, &mut snap));
            if lod != self.state.lod
                || motion != self.state.motion_sync
                || ripple != self.state.ripple
                || snap != self.state.angle_snap
            {
                self.send(Cmd::Flags { lod, ripple, angle_snap: snap, motion_sync: motion });
            }

            let mut game = self.state.game_mode;
            row(ui, "Game mode", |ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for m in (1..=3u8).rev() {
                    if chip(ui, &m.to_string(), game == m, 40.0).clicked() {
                        game = m;
                    }
                }
            });
            if game != self.state.game_mode {
                self.send(Cmd::GameMode(game));
            }
        });

        ui.add_space(10.0);
        section(ui, "Buttons and power");
        card(ui, |ui| {
            let mut debounce = self.state.debounce;
            let debounce_text = format!("{debounce} ms");
            if slider_row(ui, "Debounce", &mut debounce, 0..=30, &debounce_text) {
                self.send(Cmd::Debounce(debounce));
            }

            let mut sleep = self.state.sleep;
            let sleep_text = if sleep == 0 { "off".to_string() } else { format!("{sleep} min") };
            if slider_row(ui, "Sleep", &mut sleep, 0..=60, &sleep_text) {
                self.send(Cmd::Sleep(sleep));
            }
        });
    }
}

// ----------------------------------------------------------------- widgets

const GAP: f32 = 8.0;

/// Width for `n` chips laid side by side across what is left of the row.
fn share(ui: &egui::Ui, n: usize) -> f32 {
    ((ui.available_width() - GAP * (n as f32 - 1.0)) / n as f32).max(34.0)
}

fn section(ui: &mut egui::Ui, title: &str) {
    // Spaced-out caps: egui has no letter-spacing, so space the letters.
    let spaced: String = title.to_uppercase().chars().flat_map(|c| [c, ' ']).collect();
    ui.label(RichText::new(spaced.trim_end()).color(MUTED).size(9.5));
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::new()
        .fill(CARD)
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn chip(ui: &mut egui::Ui, label: &str, active: bool, width: f32) -> egui::Response {
    let id = ui.next_auto_id();
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let text = RichText::new(label)
        .color(if active { Color32::WHITE } else { TEXT })
        .size(12.5);
    ui.add_sized(
        Vec2::new(width, 32.0),
        egui::Button::new(text)
            .fill(if active {
                ACCENT
            } else if hovered {
                LINE
            } else {
                CHIP
            })
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(10)),
    )
}

/// A dot, not a box: filled when on, an outline when off.
fn dot(ui: &mut egui::Ui, value: &mut bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(22.0, 22.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
    }
    let painter = ui.painter();
    let centre = rect.center();
    if *value {
        painter.circle_filled(centre, 7.0, ACCENT);
    } else {
        painter.circle_stroke(centre, 7.0, Stroke::new(1.5, LINE));
    }
    response.clicked()
}

/// A labelled slider that reads "name ———— value". The row lays out right to
/// left, so the readout is added first to land on the right.
fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u8,
    range: std::ops::RangeInclusive<u8>,
    readout: &str,
) -> bool {
    row(ui, label, |ui| {
        ui.add_sized(
            Vec2::new(46.0, 20.0),
            egui::Label::new(RichText::new(readout).color(MUTED).size(12.0)),
        );
        ui.add_space(2.0);
        let r = ui.add(
            egui::Slider::new(value, range)
                .show_value(false)
                .trailing_fill(true),
        );
        r.drag_stopped() || (r.changed() && !r.dragged())
    })
}

fn row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.set_min_height(30.0);
        ui.label(RichText::new(label).color(TEXT).size(13.0));
        // Right-aligned, which means egui adds items right to left: a group of
        // several widgets has to be fed in reverse to read in order.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            out = Some(add(ui));
        });
    });
    out.unwrap()
}

// -------------------------------------------------------------------- main

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([470.0, 900.0])
            .with_min_inner_size([430.0, 420.0])
            .with_title("Mouse"),
        ..Default::default()
    };
    eframe::run_native(
        "mchose",
        options,
        Box::new(|cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = BG;
            visuals.window_fill = BG;
            visuals.extreme_bg_color = CHIP;
            visuals.override_text_color = Some(TEXT);
            visuals.selection.bg_fill = ACCENT;
            visuals.widgets.inactive.bg_fill = CHIP;
            visuals.widgets.hovered.bg_fill = LINE;
            visuals.widgets.inactive.weak_bg_fill = CHIP;
            visuals.widgets.hovered.weak_bg_fill = LINE;
            visuals.widgets.active.weak_bg_fill = ACCENT;
            visuals.widgets.noninteractive.bg_stroke = Stroke::NONE;
            // A slider's rail and its handle read the same colour field, so a
            // violet handle paints the whole rail violet. Keep both dim and let
            // the handle be a ring instead: the filled part of the rail is the
            // only violet, which is also what the dots do elsewhere.
            visuals.handle_shape = egui::style::HandleShape::Circle;
            visuals.widgets.inactive.bg_fill = CHIP;
            visuals.widgets.hovered.bg_fill = LINE;
            visuals.widgets.active.bg_fill = LINE;
            visuals.widgets.inactive.fg_stroke = Stroke::new(2.0, HOT);
            visuals.widgets.hovered.fg_stroke = Stroke::new(2.5, HOT);
            visuals.widgets.active.fg_stroke = Stroke::new(2.5, HOT);
            visuals.widgets.active.bg_fill = ACCENT;
            cc.egui_ctx.set_visuals(visuals);
            cc.egui_ctx.all_styles_mut(|s| {
                s.spacing.slider_width = 190.0;
                s.spacing.interact_size.y = 26.0;
                // the window is sized to fit, so the bar should not draw itself
                s.spacing.scroll.bar_width = 4.0;
                s.spacing.scroll.floating = true;
            });
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
    )
}
