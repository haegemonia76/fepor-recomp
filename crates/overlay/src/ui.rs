//! The settings pages.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use fepor_core::memcard;
use fepor_core::settings::Aspect;
use fepor_input::{Action, GlyphSet};

use crate::shared::Shared;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Video,
    Audio,
    Language,
    Controls,
    Card,
    Help,
    Quit,
}

impl Page {
    pub const ALL: [Page; 7] = [
        Page::Video,
        Page::Audio,
        Page::Language,
        Page::Controls,
        Page::Card,
        Page::Help,
        Page::Quit,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::Video => "Video",
            Page::Audio => "Audio",
            Page::Controls => "Controls",
            Page::Card => "Memory card",
            Page::Language => "Language",
            Page::Help => "Help",
            Page::Quit => "Quit",
        }
    }

    pub fn parse(s: &str) -> Option<Page> {
        Page::ALL
            .into_iter()
            .find(|p| p.label().eq_ignore_ascii_case(s) || format!("{p:?}").eq_ignore_ascii_case(s))
    }
}

/// What the card page knows about the card image (re-read every couple of seconds while shown).
struct CardView {
    read_at: Instant,
    result: Result<(memcard::CardInfo, Vec<(String, usize)>), String>,
}

pub struct UiState {
    pub page: Page,
    card: Option<CardView>,
    card_message: String,
    focus_tabs: bool,
    /// The file chooser, while the player has it open.
    picker: Option<Receiver<Vec<PathBuf>>>,
    /// Saves waiting in the imports folder, re-read every couple of seconds.
    pending: Vec<String>,
    pending_at: Option<Instant>,
    pad: crate::pad::PadView,
    /// The gamepad input picked on the drawing of the pad.
    pad_selected: Option<&'static str>,
}

impl UiState {
    pub fn new(page: Page) -> UiState {
        UiState {
            page,
            card: None,
            card_message: String::new(),
            focus_tabs: true,
            picker: None,
            pending: Vec::new(),
            pending_at: None,
            pad: crate::pad::PadView::default(),
            pad_selected: None,
        }
    }
}

/// Quit page: one button, no confirmation.
fn quit(ui: &mut egui::Ui) {
    ui.label("Leave the game and close the window.");
    if ui.button("Quit the game").clicked() {
        // memory card writes already went to the file; nothing else needs flushing
        std::process::exit(0);
    }
}

const ACCENT: Color32 = crate::theme::GOLD;

fn read_card(path: &std::path::Path) -> Result<(memcard::CardInfo, Vec<(String, usize)>), String> {
    let image = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let info = memcard::inspect(&image)?;
    let saves = if info.formatted {
        memcard::list(&image)?
    } else {
        Vec::new()
    };
    Ok((info, saves))
}

/// One frame of UI: the settings panel when `open`, the statistics corner per `stats`.
pub fn show(
    root: &mut egui::Ui,
    st: &mut UiState,
    shared: &Shared,
    open: bool,
    stats: fepor_core::settings::StatsMode,
) {
    use fepor_core::settings::StatsMode;
    // the statistics go first so that the settings panel, painted after, covers them
    if stats != StatsMode::Off {
        stats_hud(root, &shared.stats(), stats == StatsMode::Full);
    }
    if open {
        settings_panel(root, st, shared);
    }
}

const GOOD: Color32 = Color32::from_rgb(110, 220, 120);
const WARN: Color32 = Color32::from_rgb(240, 200, 70);
const BAD: Color32 = Color32::from_rgb(240, 90, 80);

fn stats_hud(root: &mut egui::Ui, s: &crate::stats::StatsSnapshot, full: bool) {
    use egui::RichText as T;
    let line = |ui: &mut egui::Ui, color: Color32, text: String| {
        ui.label(T::new(text).monospace().small().color(color));
    };
    {
        // drawn into the root layer before the settings panel, which is painted over it
        let rect = egui::Rect::from_min_size(egui::pos2(6.0, 6.0), egui::vec2(520.0, 700.0));
        let ui = &mut root.new_child(egui::UiBuilder::new().max_rect(rect));
        egui::Frame::new()
            .fill(crate::theme::BAR.gamma_multiply(0.85))
            .stroke(egui::Stroke::new(1.0, crate::theme::GOLD_DARK))
            .corner_radius(4.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                if !full {
                    let fps = if s.shown_fps > 0.0 {
                        s.shown_fps
                    } else {
                        s.guest_fps
                    };
                    line(ui, Color32::WHITE, format!("{fps:.0} fps"));
                    return;
                }
                line(
                    ui,
                    Color32::WHITE,
                    format!(
                        "Game {:5.1} fps   shown {:5.1}/s   display {:.0} Hz",
                        s.guest_fps, s.shown_fps, s.display_hz
                    ),
                );
                if let Some((avg, p99, worst)) = s.frame_summary() {
                    let color = if worst > avg * 2.0 { WARN } else { GOOD };
                    line(
                        ui,
                        color,
                        format!("Frame avg {avg:5.1} ms  p99 {p99:5.1}  worst {worst:5.1}"),
                    );
                    let (resp, painter) =
                        ui.allocate_painter(egui::vec2(260.0, 34.0), egui::Sense::hover());
                    let rect = resp.rect;
                    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(120));
                    let top = (avg * 3.0).max(20.0);
                    let w = rect.width() / s.frame_ms.len().max(1) as f32;
                    for (i, ms) in s.frame_ms.iter().enumerate() {
                        let h = (ms / top).min(1.0) * rect.height();
                        let c = if *ms > avg * 2.0 {
                            BAD
                        } else if *ms > avg * 1.3 {
                            WARN
                        } else {
                            GOOD
                        };
                        let x = rect.left() + i as f32 * w;
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(x, rect.bottom() - h),
                                egui::pos2(x + w.max(1.0) - 0.5, rect.bottom()),
                            ),
                            0.0,
                            c,
                        );
                    }
                }
                let budget = |ms: f32| {
                    if ms > 800.0 {
                        BAD
                    } else if ms > 500.0 {
                        WARN
                    } else {
                        GOOD
                    }
                };
                line(
                    ui,
                    budget(s.draw_ms_per_s + s.interp_ms_per_s),
                    format!(
                        "Guest thread: draw {:4.0} ms/s, interp {:3.0} ms/s",
                        s.draw_ms_per_s, s.interp_ms_per_s
                    ),
                );
                line(
                    ui,
                    Color32::WHITE,
                    format!(
                        "Draws/frame {}   target {}x{}",
                        s.draws_per_frame, s.target.0, s.target.1
                    ),
                );
                match (s.cpu_process, s.rss_mib) {
                    (Some(cpu), Some(rss)) => line(
                        ui,
                        Color32::WHITE,
                        format!("CPU process {cpu:4.0} %   memory {rss:.0} MiB"),
                    ),
                    _ => line(
                        ui,
                        Color32::GRAY,
                        "CPU: not available on this system".into(),
                    ),
                }
                for t in s.threads.iter().take(5) {
                    let color = if t.percent > 90.0 {
                        WARN
                    } else {
                        Color32::LIGHT_GRAY
                    };
                    line(ui, color, format!("  {:<14}{:4.0} %", t.name, t.percent));
                }
                let gpu = match s.gpu_busy {
                    Some(b) => {
                        format!("GPU busy {b:3.0} %   latency {:.1} ms", s.gpu_latency_ms)
                    }
                    None => format!(
                        "GPU latency {:.1} ms (utilisation not available)",
                        s.gpu_latency_ms
                    ),
                };
                line(
                    ui,
                    if s.gpu_busy.is_some_and(|b| b > 95.0) {
                        WARN
                    } else {
                        Color32::WHITE
                    },
                    gpu,
                );
                line(
                    ui,
                    Color32::GRAY,
                    format!("  {} ({})", s.gpu_name, s.backend),
                );
                let audio = |v: f32| {
                    if v >= 195.0 {
                        GOOD
                    } else if v >= 150.0 {
                        WARN
                    } else {
                        BAD
                    }
                };
                line(
                    ui,
                    audio(s.ax_per_s.min(s.ai_per_s)),
                    format!(
                        "Audio AX {:3.0}/s   AI {:3.0}/s (200 = real time)",
                        s.ax_per_s, s.ai_per_s
                    ),
                );
            });
    }
}

fn settings_panel(root: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    let step = shared.take_tab_step();
    if step != 0 {
        let n = Page::ALL.len() as i32;
        let i = Page::ALL.iter().position(|p| *p == st.page).unwrap_or(0) as i32;
        st.page = Page::ALL[(i + step).rem_euclid(n) as usize];
        st.focus_tabs = true;
    }
    // the panel: green gradient, gold frame; its contents sit inside a margin
    crate::theme::paint_panel(root.painter(), root.max_rect());
    let frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(34, 28));
    egui::CentralPanel::default().frame(frame).show(root, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new("Settings").color(crate::theme::GOLD));
            ui.add_space(14.0);
            for p in Page::ALL {
                let r = crate::theme::tab(ui, st.page == p, p.label());
                if r.clicked() && st.page != p {
                    st.page = p;
                }
            }
        });
        ui.separator();
        let footer = 22.0;
        let h = (ui.available_height() - footer).max(40.0);
        egui::ScrollArea::vertical().max_height(h).auto_shrink([false, false]).show(ui, |ui| match st.page {
            Page::Video => video(ui, shared),
            Page::Audio => audio(ui, shared),
            Page::Language => language(ui, shared),
            Page::Controls => controls(ui, st, shared),
            Page::Card => card(ui, st, shared),
            Page::Help => help(ui, shared),
            Page::Quit => quit(ui),
        });
        ui.separator();
        ui.label(
            RichText::new("F1 / Guide: close   PgUp-PgDn / LB-RB: page   arrows + Enter / D-pad + A, or the mouse")
                .small()
                .color(crate::theme::DIM),
        );
    });
    st.focus_tabs = false;
}

fn video(ui: &mut egui::Ui, shared: &Shared) {
    let before = shared.settings.get();
    let mut s = before.clone();
    ui.label(crate::theme::caption("Frame multiplier"));
    ui.add(
        egui::Slider::new(&mut s.video.frame_multiplier, 1..=8).custom_formatter(|v, _| {
            if v < 1.5 {
                "x1 (as the game draws)".to_string()
            } else {
                format!("x{v:.0} (interpolated images)")
            }
        }),
    );
    let (hz, fps) = (shared.display_hz(), shared.guest_fps());
    if hz > 0.0 && fps > 0.0 {
        let wanted = fps * before.video.frame_multiplier as f32;
        ui.label(
            RichText::new(format!(
                "The game draws {fps:.0} frames/s, the display refreshes {hz:.0} times/s: {:.0} images/s are seen{}.",
                wanted.min(hz),
                if wanted > hz * 1.02 { " (more cannot be shown)" } else { "" }
            ))
            .small()
            .weak(),
        );
    }
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Internal resolution"));
    ui.add(
        egui::Slider::new(&mut s.video.resolution, 1..=4)
            .custom_formatter(|v, _| format!("x{v:.0}  ({}x{})", 640 * v as u32, 528 * v as u32)),
    );
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Anti-aliasing (MSAA)"));
    ui.horizontal(|ui| {
        for n in [1, 2, 4, 8] {
            let label = if n == 1 {
                "Off".into()
            } else {
                format!("{n}x")
            };
            ui.radio_value(&mut s.video.msaa, n, label);
        }
    });
    ui.label(
        RichText::new(
            "Applies at once. A GPU without that sample count uses the next lower one it has.",
        )
        .small()
        .weak(),
    );
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Anisotropic filtering"));
    ui.horizontal(|ui| {
        for n in [1, 2, 4, 8, 16] {
            let label = if n == 1 {
                "Off".into()
            } else {
                format!("{n}x")
            };
            ui.radio_value(&mut s.video.anisotropy, n, label);
        }
    });
    ui.label(
        RichText::new("Applies at once. Sharper textures on slanted surfaces.")
            .small()
            .weak(),
    );
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Picture shape"));
    for a in Aspect::ALL {
        ui.radio_value(&mut s.video.aspect, a, a.label());
    }
    ui.add_space(8.0);
    filters(ui, &mut s.video.filters);
    ui.add_space(8.0);
    ui.checkbox(&mut s.video.fullscreen, "Fullscreen");
    ui.checkbox(
        &mut s.video.vsync,
        "Vsync (no tearing; off = lower latency)",
    );
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Statistics in the corner (F2)"));
    for m in fepor_core::settings::StatsMode::ALL {
        ui.radio_value(&mut s.video.stats, m, m.label());
    }
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Overlay size"));
    ui.add(
        egui::Slider::new(&mut s.video.overlay_scale, 0.0..=3.0).custom_formatter(|v, _| {
            if v < 0.05 {
                "Auto (follows the window)".to_string()
            } else {
                format!("x{v:.1}")
            }
        }),
    );
    if s != before {
        shared.settings.update(|x| *x = s);
    }
}

/// The render filters: they change the picture only, never the overlay.
fn filters(ui: &mut egui::Ui, f: &mut fepor_core::settings::Filters) {
    use fepor_core::settings::{Filters, Mask};
    ui.label(crate::theme::caption("Picture filters"));
    let slider =
        |ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>| {
            ui.label(RichText::new(label).small());
            ui.add(egui::Slider::new(v, range).custom_formatter(|x, _| format!("{x:.2}")));
        };
    slider(ui, "Scanlines", &mut f.scanlines, 0.0..=1.0);
    slider(ui, "Gamma", &mut f.gamma, 0.5..=2.0);
    slider(ui, "Saturation", &mut f.saturation, 0.0..=2.0);
    slider(ui, "Contrast", &mut f.contrast, 0.5..=1.5);
    slider(ui, "Brightness", &mut f.brightness, -0.5..=0.5);
    ui.label(RichText::new("Screen mask").small());
    for m in Mask::ALL {
        ui.radio_value(&mut f.mask, m, m.label());
    }
    if f.mask != Mask::Off {
        slider(ui, "Mask strength", &mut f.mask_strength, 0.0..=1.0);
    }
    if ui.button("Reset filters").clicked() {
        *f = Filters::default();
    }
}

fn audio(ui: &mut egui::Ui, shared: &Shared) {
    let before = shared.settings.get();
    let mut pct = (before.audio.volume * 100.0).round() as u32;
    ui.label(crate::theme::caption("Master volume"));
    ui.add(egui::Slider::new(&mut pct, 0..=100).suffix("%"));
    let volume = pct as f32 / 100.0;
    if (volume - before.audio.volume).abs() > 0.004 {
        shared.settings.update(|x| x.audio.volume = volume);
    }
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Output device"));
    let devices = shared
        .audio_devices
        .lock()
        .map(|d| d.clone())
        .unwrap_or_default();
    let mut device = before.audio.device.clone();
    let shown = device
        .clone()
        .unwrap_or_else(|| "System default".to_string());
    egui::ComboBox::from_id_salt("audio_device")
        .width(360.0)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut device, None, "System default");
            for name in devices {
                ui.selectable_value(&mut device, Some(name.clone()), name);
            }
        });
    if device != before.audio.device {
        shared.settings.update(|x| x.audio.device = device);
    }
}

/// The language of the game's text. The game reads it once, when it starts.
fn language(ui: &mut egui::Ui, shared: &Shared) {
    use fepor_core::settings::Language;
    let before = shared.settings.get().game.language;
    let mut now = before;
    ui.label(crate::theme::caption("Text language"));
    egui::ComboBox::from_id_salt("text_language")
        .width(240.0)
        .selected_text(now.label())
        .show_ui(ui, |ui| {
            for l in Language::ALL {
                ui.selectable_value(&mut now, l, l.label());
            }
        });
    if now != before {
        shared.settings.update(|x| x.game.language = now);
    }
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            "Menus, dialogue and windows. Takes effect the next time the game starts.",
        )
        .wrap(),
    );
    ui.add_space(12.0);
    ui.label(crate::theme::caption("Audio language"));
    ui.add(egui::Label::new("This disc has one set of voices and movie soundtracks: no other audio language to choose.").wrap());
}

fn controls(ui: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    let Ok(mut c) = shared.controls.lock() else {
        return;
    };
    if let Some((keyboard, action)) = c.remap_target() {
        ui.colored_label(
            ACCENT,
            format!(
                "Press the {} input for {}   ({})",
                if keyboard { "keyboard" } else { "gamepad" },
                action.label(),
                if keyboard {
                    "Esc keeps the current binding"
                } else {
                    "Esc cancels"
                }
            ),
        );
        if ui.button("Cancel").clicked() {
            c.cancel_remap();
        }
        ui.separator();
    }
    let cfg = c.config().clone();
    ui.horizontal_wrapped(|ui| {
        ui.label(crate::theme::caption("Controller"));
        for (g, name) in [
            (GlyphSet::GameCube, "GameCube"),
            (GlyphSet::Xbox, "Xbox"),
            (GlyphSet::Nintendo, "Nintendo"),
            (GlyphSet::PlayStation, "PlayStation"),
        ] {
            if ui.radio(cfg.glyphs == g, name).clicked() {
                c.set_glyphs(g);
                st.pad_selected = None;
            }
        }
    });
    ui.add_space(4.0);
    crate::pad::show(ui, &mut st.pad, &mut c, &mut st.pad_selected);
    ui.add_space(4.0);
    // the GameCube buttons to give to the input picked on the drawing
    match st.pad_selected {
        Some(source) => {
            ui.label(crate::theme::caption(format!(
                "{} does:",
                cfg.glyphs.source_label(source)
            )));
            let current = cfg.bindings.gamepad.get(source).copied();
            ui.horizontal_wrapped(|ui| {
                for a in Action::ALL {
                    if ui.selectable_label(current == Some(a), a.label()).clicked() {
                        c.assign(false, source, a);
                        st.pad_selected = None;
                    }
                }
                if ui.button("Nothing").clicked() {
                    c.unassign(false, source);
                    st.pad_selected = None;
                }
            });
        }
        None => {
            ui.label(
                RichText::new(if cfg.glyphs == GlyphSet::GameCube {
                    "Click a GameCube button, then press the input of your gamepad that should do it."
                } else {
                    "Click a button on the controller, then choose the GameCube button it stands for."
                })
                .small()
                .color(crate::theme::DIM),
            );
        }
    }
    ui.add_space(8.0);
    let mut dz = cfg.deadzone;
    ui.horizontal(|ui| {
        ui.label(crate::theme::caption("Stick dead zone"));
        ui.add(egui::Slider::new(&mut dz, 0.0..=0.9));
    });
    if (dz - cfg.deadzone).abs() > 1e-4 {
        c.set_deadzone(dz);
    }
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Keyboard"));
    let mut rebind = None;
    egui::Grid::new("bindings")
        .striped(true)
        .num_columns(3)
        .show(ui, |ui| {
            for a in Action::ALL {
                ui.label(a.label());
                let names: Vec<&str> = cfg.sources_of(true, a);
                ui.label(if names.is_empty() {
                    "-".to_string()
                } else {
                    names.join(", ")
                });
                if ui.small_button("Rebind").clicked() {
                    rebind = Some(a);
                }
                ui.end_row();
            }
        });
    if let Some(a) = rebind {
        c.rebind(true, a);
    }
    ui.add_space(6.0);
    if ui.button("Restore the default bindings").clicked() {
        c.reset_bindings();
        st.pad_selected = None;
    }
}

fn card(ui: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    let imports = shared.data_dir.join("imports");
    // the player's answer from the file chooser, if it came in
    if let Some(rx) = &st.picker {
        match rx.try_recv() {
            Ok(files) => {
                st.picker = None;
                st.card_message = queue_imports(&files, &imports);
                st.pending_at = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => st.picker = None,
        }
    }
    if st
        .pending_at
        .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
    {
        st.pending = pending_imports(&imports);
        st.pending_at = Some(Instant::now());
    }
    let path = shared.card_path.lock().ok().and_then(|p| p.clone());
    match path {
        Some(path) => card_info(ui, st, shared, &path),
        None => {
            ui.label("No memory card in slot A.");
            if !st.card_message.is_empty() {
                ui.label(&st.card_message);
            }
        }
    }
    ui.add_space(8.0);
    ui.separator();
    import_section(ui, st, &imports);
}

fn card_info(ui: &mut egui::Ui, st: &mut UiState, shared: &Shared, path: &Path) {
    ui.label(format!("Slot A: {}", path.display()));
    if st
        .card
        .as_ref()
        .is_none_or(|c| c.read_at.elapsed() > Duration::from_secs(2))
    {
        st.card = Some(CardView {
            read_at: Instant::now(),
            result: read_card(&path),
        });
    }
    let Some(view) = &st.card else {
        return;
    };
    match &view.result {
        Err(e) => {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        Ok((info, saves)) => {
            if !info.formatted {
                ui.label("The card is not formatted: the game offers to format it.");
            } else {
                ui.label(format!(
                    "{} Mbit, {} blocks free",
                    info.size_mbit, info.free_blocks
                ));
                ui.add_space(4.0);
                for (name, _) in saves {
                    ui.label(format!("  {name}"));
                }
                if saves.is_empty() {
                    ui.label("  (no saves)");
                }
                ui.add_space(6.0);
                let exports = shared.data_dir.join("exports");
                if ui
                    .button(format!("Export the saves as .gci to {}", exports.display()))
                    .clicked()
                {
                    st.card_message = export(&path, &exports);
                }
            }
        }
    }
    if !st.card_message.is_empty() {
        ui.label(&st.card_message);
    }
}

/// A `.gci` file is a 0x40-byte directory entry followed by whole 8 KiB blocks.
fn check_gci(bytes: &[u8]) -> Result<(), String> {
    const HEADER: usize = 0x40;
    const BLOCK: usize = 0x2000;
    if bytes.len() < HEADER + BLOCK || (bytes.len() - HEADER) % BLOCK != 0 {
        return Err("not a GameCube save (.gci)".into());
    }
    Ok(())
}

/// Copy the chosen `.gci` files into `dir`, where the game picks them up at its next start. Returns what to
/// tell the player.
pub(crate) fn queue_imports(files: &[PathBuf], dir: &Path) -> String {
    if files.is_empty() {
        return String::new();
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        return format!("Cannot create {}: {e}", dir.display());
    }
    let (mut queued, mut problems) = (0, Vec::new());
    for f in files {
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let done = std::fs::read(f)
            .map_err(|e| e.to_string())
            .and_then(|bytes| check_gci(&bytes).map(|()| bytes))
            .and_then(|bytes| std::fs::write(dir.join(&name), bytes).map_err(|e| e.to_string()));
        match done {
            Ok(()) => queued += 1,
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    let mut msg = String::new();
    if queued > 0 {
        msg = format!("{queued} save(s) queued: they are imported into the card the next time the game starts.");
    }
    for p in problems {
        msg.push_str(&format!("\n{p}"));
    }
    msg.trim().to_string()
}

/// Save files waiting in `dir` for the next start.
fn pending_imports(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".gci") || n.ends_with(".gci.failed"))
        .collect();
    names.sort();
    names
}

/// Open the native file chooser on its own thread (it blocks until the player is done) and keep the receiver.
fn start_picker(st: &mut UiState) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let files = rfd::FileDialog::new()
            .set_title("Choose the saves to import")
            .add_filter("GameCube save", &["gci"])
            .pick_files()
            .unwrap_or_default();
        let _ = tx.send(files);
    });
    st.picker = Some(rx);
}

fn import_section(ui: &mut egui::Ui, st: &mut UiState, imports: &Path) {
    ui.label(crate::theme::caption("Import saves"));
    let choosing = st.picker.is_some();
    if ui
        .add_enabled(
            !choosing,
            egui::Button::new(if choosing {
                "Choosing..."
            } else {
                "Choose .gci files..."
            }),
        )
        .clicked()
    {
        start_picker(st);
    }
    if st.pending.is_empty() {
        ui.label(
            RichText::new("Saves chosen here are put on the card the next time the game starts.")
                .small()
                .weak(),
        );
        return;
    }
    ui.label(format!("In the imports folder ({}):", st.pending.len()));
    let mut remove = None;
    for name in &st.pending {
        ui.horizontal(|ui| {
            if let Some(base) = name.strip_suffix(".failed") {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    format!("  {base}: not imported (already on the card, or no room)"),
                );
            } else {
                ui.label(format!("  {name}  (imported at the next start)"));
            }
            if ui.small_button("Remove").clicked() {
                remove = Some(name.clone());
            }
        });
    }
    if let Some(name) = remove {
        let _ = std::fs::remove_file(imports.join(&name));
        st.pending_at = None;
    }
}

fn export(card: &std::path::Path, dir: &std::path::Path) -> String {
    let run = || -> Result<usize, String> {
        let image = std::fs::read(card).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let saves = memcard::list(&image)?;
        for (name, entry) in &saves {
            let gci = memcard::export_gci(&image, *entry)?;
            std::fs::write(dir.join(format!("{name}.gci")), gci).map_err(|e| e.to_string())?;
        }
        Ok(saves.len())
    };
    match run() {
        Ok(n) => format!("Exported {n} save(s)."),
        Err(e) => format!("Export failed: {e}"),
    }
}

fn help(ui: &mut egui::Ui, shared: &Shared) {
    ui.label("F1 (keyboard) or Guide / Back (gamepad): open and close this menu.");
    ui.label("While it is open the game sees no input.");
    ui.add_space(6.0);
    ui.label(
        "Navigation: arrows and Enter, D-pad / left stick and A, or the mouse. B or Esc closes.",
    );
    ui.label("PgUp / PgDn or LB / RB: previous / next page.");
    ui.add_space(6.0);
    ui.label(format!("Data folder: {}", shared.data_dir.display()));
    ui.label(format!(
        "Settings: {}",
        fepor_core::settings::path_in(&shared.data_dir).display()
    ));
    ui.label(format!(
        "Bindings: {}",
        shared.data_dir.join("input.toml").display()
    ));
    ui.add_space(6.0);
    ui.label(
        RichText::new("FEPOR_FPS_MULT, FEPOR_RES, FEPOR_MSAA, FEPOR_ANISO and FEPOR_ASPECT override the saved video settings for one run.")
            .small()
            .weak(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gci(blocks: usize) -> Vec<u8> {
        vec![0u8; 0x40 + 0x2000 * blocks]
    }

    #[test]
    fn only_whole_block_saves_are_queued_and_the_rest_is_reported() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let good = src.path().join("01-GFEP-FIREEMBLEM8.gci");
        let bad = src.path().join("notes.gci");
        std::fs::write(&good, gci(19)).unwrap();
        std::fs::write(&bad, b"hello").unwrap();
        let imports = dst.path().join("imports");
        let msg = queue_imports(&[good, bad], &imports);
        assert!(msg.starts_with("1 save(s) queued"), "{msg}");
        assert!(msg.contains("notes.gci: not a GameCube save"), "{msg}");
        assert_eq!(
            pending_imports(&imports),
            vec!["01-GFEP-FIREEMBLEM8.gci".to_string()]
        );
    }

    #[test]
    fn choosing_nothing_says_nothing() {
        let dst = tempfile::tempdir().unwrap();
        assert_eq!(queue_imports(&[], dst.path()), "");
    }
}
