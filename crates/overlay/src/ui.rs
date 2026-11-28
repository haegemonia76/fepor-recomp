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
    Speed,
    Capture,
    Language,
    Controls,
    Card,
    Mods,
    /// The memory watcher: shown only with the developer option (ADR 0048).
    Memory,
    Cheats,
    States,
    Help,
    Quit,
}

impl Page {
    pub const ALL: [Page; 13] = [
        Page::Video,
        Page::Audio,
        Page::Speed,
        Page::Capture,
        Page::Language,
        Page::Controls,
        Page::Card,
        Page::Mods,
        Page::Memory,
        Page::Cheats,
        Page::States,
        Page::Help,
        Page::Quit,
    ];

    /// The pages in the tab row; the memory page needs the developer option (or being opened by `FEPOR_OVERLAY`).
    fn visible(developer: bool, current: Page) -> Vec<Page> {
        Page::ALL
            .into_iter()
            .filter(|p| *p != Page::Memory || developer || current == Page::Memory)
            .collect()
    }

    fn label(self) -> &'static str {
        match self {
            Page::Video => "Video",
            Page::Audio => "Audio",
            Page::Speed => "Speed",
            Page::Capture => "Capture",
            Page::Controls => "Controls",
            Page::Card => "Memory card",
            Page::Mods => "Mods",
            Page::Memory => "Memory",
            Page::Cheats => "Cheats",
            Page::States => "Save states",
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
    backups: Vec<memcard::Backup>,
}

pub struct UiState {
    pub page: Page,
    card: Option<CardView>,
    card_message: String,
    memory: crate::memory::MemoryView,
    focus_tabs: bool,
    /// The file chooser, while the player has it open.
    picker: Option<Receiver<Vec<PathBuf>>>,
    /// Saves waiting in the imports folder, re-read every couple of seconds.
    pending: Vec<String>,
    pending_at: Option<Instant>,
    /// The INI chooser of the cheats page, and what came of the last import.
    cheat_picker: Option<Receiver<Option<PathBuf>>>,
    cheat_message: Option<Result<String, String>>,
    pad: crate::pad::PadView,
    /// The gamepad input picked on the drawing of the pad.
    pad_selected: Option<&'static str>,
    mods: crate::mods::ModsView,
}

impl UiState {
    /// Scripted memory-page steps run whether or not the page (or the overlay) is shown.
    pub fn memory_tick(&mut self, shared: &Shared) {
        self.memory.tick(&shared.memory);
    }

    pub fn memory_live(&self) -> bool {
        self.memory.live()
    }

    pub fn new(page: Page) -> UiState {
        UiState {
            page,
            card: None,
            card_message: String::new(),
            memory: crate::memory::MemoryView::new(),
            focus_tabs: true,
            picker: None,
            pending: Vec::new(),
            pending_at: None,
            cheat_picker: None,
            cheat_message: None,
            pad: crate::pad::PadView::default(),
            mods: crate::mods::ModsView::default(),
            pad_selected: None,
        }
    }
}

/// Quit page: one button, no confirmation.
fn quit(ui: &mut egui::Ui, shared: &Shared) {
    ui.label("Leave the game and close the window.");
    if ui.button("Quit the game").clicked() {
        // memory card writes already went to the file; a recording in progress is completed
        shared.capture.finish();
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
    // the guest thread publishes memory copies only while the page is shown
    shared
        .memory
        .set_active((open && st.page == Page::Memory) || st.memory.live());
    // the statistics go first so that the settings panel, painted after, covers them
    if stats != StatsMode::Off {
        stats_hud(root, &shared.stats(), stats == StatsMode::Full);
    }
    toast(root);
    capture_hud(root, &shared.capture);
    if open {
        settings_panel(root, st, shared);
    }
    if let Some(text) = shared.notice() {
        egui::Area::new(egui::Id::unique("pad notice"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 10.0))
            .interactable(false)
            .show(root.ctx(), |ui| {
                egui::Frame::new()
                    .fill(crate::theme::BAR.gamma_multiply(0.92))
                    .stroke(egui::Stroke::new(1.0, crate::theme::GOLD))
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.label(RichText::new(text).color(Color32::WHITE));
                    });
            });
    }
}

/// Whether the recording indicator or a capture message is on screen.
pub fn capture_hud_shown(capture: &fepor_core::capture::Capture) -> bool {
    capture.is_recording() || capture.toast().is_some()
}

/// The recording indicator and the last capture message, in the top-right corner.
fn capture_hud(root: &mut egui::Ui, capture: &fepor_core::capture::Capture) {
    let mut lines = Vec::new();
    if let Some(s) = capture.recording_secs() {
        lines.push((BAD, format!("REC {}:{:02}", s / 60, s % 60)));
    }
    if let Some(t) = capture.toast() {
        lines.push((Color32::WHITE, t));
    }
    if lines.is_empty() {
        return;
    }
    let screen = root.max_rect();
    let rect = egui::Rect::from_min_size(
        egui::pos2(screen.left() + 6.0, screen.bottom() - 60.0),
        egui::vec2((screen.width() - 12.0).max(100.0), 54.0),
    );
    let ui = &mut root.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::bottom_up(egui::Align::Min)),
    );
    for (color, text) in lines.into_iter().rev() {
        egui::Frame::new()
            .fill(crate::theme::BAR.gamma_multiply(0.85))
            .stroke(egui::Stroke::new(1.0, crate::theme::GOLD_DARK))
            .corner_radius(4.0)
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.label(RichText::new(text).small().color(color));
            });
    }
}

/// The save state line ("Saved slot 2"), at the bottom left for a few seconds.
fn toast(root: &mut egui::Ui) {
    let Some(text) = fepor_core::states::message() else {
        return;
    };
    let r = root.max_rect();
    let rect = egui::Rect::from_min_size(
        egui::pos2(r.left() + 8.0, r.bottom() - 40.0),
        egui::vec2(520.0, 32.0),
    );
    let ui = &mut root.new_child(egui::UiBuilder::new().max_rect(rect));
    egui::Frame::new()
        .fill(crate::theme::BAR.gamma_multiply(0.9))
        .stroke(egui::Stroke::new(1.0, crate::theme::GOLD_DARK))
        .corner_radius(4.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(Color32::WHITE));
        });
}

const GOOD: Color32 = Color32::from_rgb(110, 220, 120);
const WARN: Color32 = Color32::from_rgb(240, 200, 70);
const BAD: Color32 = Color32::from_rgb(240, 90, 80);

/// "x4", "x0.5", "uncapped".
fn speed_text(factor: f32) -> String {
    if fepor_core::speed::is_uncapped() {
        format!("uncapped (x{factor:.1})")
    } else {
        format!("x{factor}")
    }
}

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
                    let speed = fepor_core::speed::factor();
                    if speed == 1.0 {
                        line(ui, Color32::WHITE, format!("{fps:.0} fps"));
                    } else {
                        line(
                            ui,
                            Color32::WHITE,
                            format!("{fps:.0} fps  {}", speed_text(speed)),
                        );
                    }
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
                let speed = fepor_core::speed::factor();
                if speed != 1.0 {
                    line(ui, GOOD, format!("Speed {}", speed_text(speed)));
                }
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
                if let Some(ms) = s.scaling_gpu_ms {
                    line(
                        ui,
                        Color32::WHITE,
                        format!("Scaling pass {ms:.2} ms on the GPU (FSR, filters)"),
                    );
                }
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
    let developer = shared.settings.get().developer;
    let step = shared.take_tab_step();
    if step != 0 {
        let pages = Page::visible(developer, st.page);
        let n = pages.len() as i32;
        let i = pages.iter().position(|p| *p == st.page).unwrap_or(0) as i32;
        st.page = pages[(i + step).rem_euclid(n) as usize];
        st.focus_tabs = true;
    }
    // the panel: green gradient, gold frame; its contents sit inside a margin
    crate::theme::paint_panel(root.painter(), root.max_rect());
    let frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(34, 28));
    egui::CentralPanel::default().frame(frame).show(root, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new("Settings").color(crate::theme::GOLD));
            ui.add_space(14.0);
            for p in Page::visible(developer, st.page) {
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
            Page::Speed => speed(ui, shared),
            Page::Capture => capture(ui, shared),
            Page::Language => language(ui, shared),
            Page::Controls => controls(ui, st, shared),
            Page::Card => card(ui, st, shared),
            Page::Mods => crate::mods::page(ui, &mut st.mods, shared),
            Page::Memory => crate::memory::memory(ui, &mut st.memory, shared),
            Page::Cheats => cheats(ui, st, shared),
            Page::States => states(ui, shared),
            Page::Help => help(ui, shared),
            Page::Quit => quit(ui, shared),
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
    ui.add_enabled(
        !s.video.fsr.is_on(),
        egui::Slider::new(&mut s.video.resolution, 1..=4)
            .custom_formatter(|v, _| format!("x{v:.0}  ({}x{})", 640 * v as u32, 528 * v as u32)),
    );
    if s.video.fsr.is_on() {
        ui.label(
            RichText::new("Set by the FSR preset below, from the window's size.")
                .small()
                .weak(),
        );
    }
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Picture shape"));
    for a in Aspect::ALL {
        ui.radio_value(&mut s.video.aspect, a, a.label());
    }
    ui.add_space(8.0);
    fsr(ui, &mut s.video.fsr);
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

/// AMD FSR 1 upscaling: while on, it picks the internal resolution from the window's size.
fn fsr(ui: &mut egui::Ui, f: &mut fepor_core::settings::Fsr) {
    use fepor_core::settings::FsrQuality;
    ui.label(crate::theme::caption("Upscaling (AMD FSR 1)"));
    for q in FsrQuality::ALL {
        ui.radio_value(&mut f.quality, q, q.label());
    }
    if f.is_on() {
        ui.label(
            RichText::new("Renders smaller, upscales to the window.")
                .small()
                .weak(),
        );
        ui.label(RichText::new("Sharpness").small());
        ui.add(
            egui::Slider::new(&mut f.sharpness, 0.0..=1.0)
                .custom_formatter(|x, _| format!("{x:.2}")),
        );
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

/// Fast-forward and slow motion (ADR 0036): the speeds of the two held keys and the sound while they are used.
fn speed(ui: &mut egui::Ui, shared: &Shared) {
    use fepor_core::settings::SpeedAudio;
    use fepor_core::speed::Speed;
    let before = shared.settings.get();
    let (mut fast, mut slow, mut sound) = (
        before.game.fast_forward,
        before.game.slow_motion,
        before.audio.speed,
    );
    ui.label(crate::theme::caption(
        "Fast-forward (hold Tab, or press the right stick)",
    ));
    egui::ComboBox::from_id_salt("fast_forward")
        .width(240.0)
        .selected_text(fast.label())
        .show_ui(ui, |ui| {
            for s in Speed::FAST {
                ui.selectable_value(&mut fast, s, s.label());
            }
        });
    ui.add_space(8.0);
    ui.label(crate::theme::caption(
        "Slow motion (hold `, or press the left stick)",
    ));
    egui::ComboBox::from_id_salt("slow_motion")
        .width(240.0)
        .selected_text(slow.label())
        .show_ui(ui, |ui| {
            for s in Speed::SLOW {
                ui.selectable_value(&mut slow, s, s.label());
            }
        });
    ui.add_space(8.0);
    ui.label(crate::theme::caption("Sound at other speeds"));
    egui::ComboBox::from_id_salt("speed_audio")
        .width(240.0)
        .selected_text(sound.label())
        .show_ui(ui, |ui| {
            for s in SpeedAudio::ALL {
                ui.selectable_value(&mut sound, s, s.label());
            }
        });
    if (fast, slow, sound)
        != (
            before.game.fast_forward,
            before.game.slow_motion,
            before.audio.speed,
        )
    {
        shared.settings.update(|x| {
            x.game.fast_forward = fast;
            x.game.slow_motion = slow;
            x.audio.speed = sound;
        });
    }
    ui.add_space(6.0);
    ui.add(
        egui::Label::new(
            "The whole game runs at the chosen speed. Uncapped runs as fast as this computer keeps up with and is always silent.",
        )
        .wrap(),
    );
}

/// Screenshots and video recording; the same actions as F12 and F10.
fn capture(ui: &mut egui::Ui, shared: &Shared) {
    let c = &shared.capture;
    ui.horizontal(|ui| {
        if ui.button("Take a screenshot (F12)").clicked() {
            c.request_screenshot();
        }
        let label = if c.is_recording() {
            "Stop recording (F10)"
        } else {
            "Record a video (F10)"
        };
        if ui.button(label).clicked() {
            c.toggle_recording();
        }
    });
    ui.add_space(8.0);
    let mut overlay = shared.settings.get().capture.overlay;
    if ui
        .checkbox(
            &mut overlay,
            "Include the overlay (menu, statistics, messages)",
        )
        .changed()
    {
        shared.settings.update(|x| x.capture.overlay = overlay);
    }
    ui.add_space(8.0);
    ui.label("Pictures are taken at the internal resolution (Video page), without the window's borders and bars.");
    ui.label(format!(
        "Screenshots: {}",
        shared.data_dir.join("screenshots").display()
    ));
    ui.label(format!(
        "Videos: {}",
        shared.data_dir.join("videos").display()
    ));
    ui.label(
        RichText::new(
            "Videos need ffmpeg: on the PATH, next to the game, or named by FEPOR_FFMPEG.",
        )
        .small()
        .weak(),
    );
    if let Some(t) = c.toast() {
        ui.add_space(6.0);
        ui.label(t);
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

/// The gamepads plugged in: which one drives the game, and which one the sections below edit.
fn pads_section(ui: &mut egui::Ui, st: &mut UiState, c: &mut fepor_input::Controls) {
    ui.label(crate::theme::caption("Gamepads"));
    let pads = c.pads().to_vec();
    if pads.is_empty() {
        ui.label(
            RichText::new(
                "No gamepad plugged in. Plug one in at any time: it gets the profile of its model \
                 and plays the game at once. The keyboard works too.",
            )
            .small()
            .color(crate::theme::DIM),
        );
        ui.add_space(8.0);
        return;
    }
    let edited = c.edited_pad().map(|p| p.id);
    let active = c.active_pad().map(|p| p.id);
    egui::Grid::new("pads")
        .striped(true)
        .num_columns(3)
        .show(ui, |ui| {
            for pad in &pads {
                if ui
                    .radio(edited == Some(pad.id), pad.display_name())
                    .clicked()
                {
                    c.select_pad(pad.id);
                    st.pad_selected = None;
                }
                ui.label(if active == Some(pad.id) {
                    "plays the game"
                } else {
                    ""
                });
                if let Some((text, hover)) = c.profile_label(pad.id) {
                    ui.label(RichText::new(text).small().color(crate::theme::DIM))
                        .on_hover_text(hover);
                }
                ui.end_row();
            }
        });
    ui.label(
        RichText::new(
            "Every controller model has its own profile (buttons and button names below), picked \
             automatically when it is plugged in. The pad plugged in or pressed last plays the game.",
        )
        .small()
        .color(crate::theme::DIM),
    );
    ui.add_space(8.0);
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
    let glyphs = c.glyphs();
    let gamepad = c.gamepad_bindings().clone();
    pads_section(ui, st, &mut c);
    ui.horizontal_wrapped(|ui| {
        ui.label(crate::theme::caption("Controller"));
        for (g, name) in [
            (GlyphSet::GameCube, "GameCube"),
            (GlyphSet::Xbox, "Xbox"),
            (GlyphSet::Nintendo, "Nintendo"),
            (GlyphSet::PlayStation, "PlayStation"),
        ] {
            if ui.radio(glyphs == g, name).clicked() {
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
                glyphs.source_label(source)
            )));
            let current = gamepad.get(source).copied();
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
                RichText::new(if glyphs == GlyphSet::GameCube {
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
                let names: Vec<&str> = cfg.sources_of(true, None, a);
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

fn cheats(ui: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    if let Some(rx) = &st.cheat_picker {
        match rx.try_recv() {
            Ok(file) => {
                st.cheat_picker = None;
                st.cheat_message = file.map(|f| {
                    shared
                        .cheats
                        .import(&f)
                        .map(|n| format!("{n} code(s) imported from {}.", f.display()))
                });
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => st.cheat_picker = None,
        }
    }
    let Some(game) = shared.cheats.game() else {
        ui.label("Cheats are available once the game has started.");
        return;
    };
    let list = shared.cheats.list();
    ui.label(RichText::new("Native cheats").strong());
    if let Some(e) = shared.cheats.native_error() {
        ui.colored_label(Color32::LIGHT_RED, format!("cannot read your cheats file: {e}"));
    }
    let native: Vec<_> = list.iter().filter(|c| c.kind == fepor_core::cheats::Kind::Native).collect();
    if native.is_empty() {
        ui.label(format!("No native cheats for {game}."));
    }
    for c in native {
        cheat_row(ui, shared, c);
    }
    ui.label(
        RichText::new(
            "Shipped with the game's addresses; add your own in cheats/<game id>.toml of the data folder (same \
             name replaces a shipped one).",
        )
        .small()
        .weak(),
    );
    ui.add_space(8.0);
    ui.separator();
    ui.label(RichText::new("Gecko and Action Replay codes").strong());
    let choosing = st.cheat_picker.is_some();
    if ui
        .add_enabled(
            !choosing,
            egui::Button::new(if choosing {
                "Choosing..."
            } else {
                "Import a Dolphin game INI..."
            }),
        )
        .clicked()
    {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(
                rfd::FileDialog::new()
                    .set_title("Choose a Dolphin game INI with Gecko or Action Replay codes")
                    .add_filter("Dolphin game INI", &["ini"])
                    .pick_file(),
            );
        });
        st.cheat_picker = Some(rx);
    }
    match &st.cheat_message {
        Some(Ok(m)) => {
            ui.label(m);
        }
        Some(Err(e)) => {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        None => {}
    }
    ui.add_space(4.0);
    let dolphin: Vec<_> = list.iter().filter(|c| c.kind != fepor_core::cheats::Kind::Native).collect();
    if dolphin.is_empty() {
        ui.label(format!("No codes for {game} yet. Import the game's INI (for example GFEP01.ini from Dolphin)."));
    }
    for c in dolphin {
        cheat_row(ui, shared, c);
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new(
            "Cheats hold their value every frame while on; turning one off puts the game's value back. Only plain \
             writes to data run: conditionals, loops, pointers and code insertion are listed as not supported.",
        )
        .small()
        .weak(),
    );
}

fn cheat_row(ui: &mut egui::Ui, shared: &Shared, c: &fepor_core::cheats::CheatView) {
    let mut on = c.enabled;
    ui.horizontal_wrapped(|ui| {
        let label = if c.author.is_empty() {
            c.name.clone()
        } else {
            format!("{}  [{}]", c.name, c.author)
        };
        if ui.add_enabled(c.supported.is_ok(), egui::Checkbox::new(&mut on, label)).changed() {
            shared.cheats.set_enabled(&c.key, on);
        }
        ui.label(RichText::new(c.kind.label()).small().weak());
    });
    if let Err(why) = &c.supported {
        ui.label(RichText::new(format!("    not supported: {why}")).small().weak());
    }
    if let Some(s) = &c.status {
        ui.colored_label(Color32::LIGHT_RED, format!("    not applied: {s}"));
    }
    for n in &c.notes {
        ui.label(RichText::new(format!("    {n}")).small().weak());
    }
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
            backups: memcard::list_backups(&memcard::backups_dir(&shared.data_dir), path),
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
    ui.add_space(6.0);
    backups(ui, st, shared);
    if !st.card_message.is_empty() {
        ui.label(&st.card_message);
    }
}

/// The copies of the card kept before each run and while playing, with the keep count and Restore buttons. A
/// restore cannot touch the card the running game has open, so it is queued for the next start.
fn backups(ui: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    let dir = memcard::backups_dir(&shared.data_dir);
    let mut keep = shared.settings.get().game.card_backups;
    ui.horizontal(|ui| {
        ui.label(crate::theme::caption("Backups kept (0 = off)"));
        if ui
            .add(egui::DragValue::new(&mut keep).range(0..=50))
            .changed()
        {
            shared.settings.update(|x| x.game.card_backups = keep);
        }
    });
    let Some(view) = &st.card else {
        return;
    };
    if view.backups.is_empty() {
        ui.label("  (no backups yet)");
    }
    let mut queued = None;
    for b in &view.backups {
        ui.horizontal(|ui| {
            ui.label(format!("  {}", memcard::format_stamp(b.stamp)));
            if ui.button("Restore").clicked() {
                queued = Some(b.clone());
            }
        });
    }
    if let Some(b) = queued {
        let name = b
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        st.card_message = match memcard::queue_restore(&dir, &name) {
            Ok(()) => format!(
                "The card of {} replaces the current one the next time the game starts (which is backed up first).",
                memcard::format_stamp(b.stamp)
            ),
            Err(e) => format!("cannot queue the restore: {e}"),
        };
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

/// How long ago a slot was written, for the list.
fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// Save states (ADR 0035): eight slots, the quick save and load keys, the rewind buffer.
fn states(ui: &mut egui::Ui, shared: &Shared) {
    use fepor_core::states;
    if states::slot_path(0).is_none() {
        ui.label("Save states are not available (the game has not started).");
        return;
    }
    ui.label("F5 saves and F7 loads the chosen slot, F6 chooses the next one. Hold F8 to rewind.");
    ui.label(
        RichText::new(
            "A state holds the whole machine, the memory card included: loading one puts the card back as it was when the state was saved. The card as it is now is backed up first (see the Memory card page). Cheats and mods you have switched on are applied again after a load.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);
    let current = states::slot();
    let infos = states::list();
    for (i, info) in infos.iter().enumerate() {
        let n = i as u8;
        ui.horizontal(|ui| {
            if ui
                .selectable_label(current == n, format!("Slot {}", n + 1))
                .clicked()
            {
                states::set_slot(n);
            }
            if ui.button("Save").clicked() {
                states::set_slot(n);
                states::post(states::Command::Save(n));
            }
            if ui
                .add_enabled(info.is_some(), egui::Button::new("Load"))
                .clicked()
            {
                states::set_slot(n);
                states::post(states::Command::Load(n));
            }
            match info {
                Some(i) => ui.label(format!(
                    "{}   {:.1} MB",
                    ago(i.modified),
                    i.bytes as f64 / 1e6
                )),
                None => ui.label(RichText::new("empty").weak()),
            };
        });
    }
    ui.add_space(10.0);
    ui.separator();
    let before = shared.settings.get().states;
    let mut s = before.clone();
    ui.checkbox(&mut s.rewind, "Keep a rewind buffer (a state every second)");
    ui.add_enabled_ui(s.rewind, |ui| {
        ui.add(
            egui::Slider::new(&mut s.rewind_seconds, 10..=600)
                .suffix(" s")
                .text("History"),
        );
    });
    if s.rewind {
        ui.label(format!(
            "Held now: {} s of play (at most {} MiB).",
            states::rewind_seconds(),
            states::REWIND_MEMORY_MIB
        ));
    }
    if s != before {
        shared.settings.update(|x| x.states = s);
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
    ui.label(
        "Hold Tab (right stick pressed): fast-forward. Hold ` (left stick pressed): slow motion.",
    );
    ui.label("F12: screenshot. F10: start / stop recording a video (Capture page).");
    ui.add_space(6.0);
    ui.label(format!(
        "Playtime: {}",
        fepor_core::playtime::format(shared.playtime_secs())
    ));
    ui.label(
        RichText::new("Counted while the game runs with this menu closed.")
            .small()
            .weak(),
    );
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
    let before = shared.settings.get().presence;
    let mut on = before.enabled;
    ui.checkbox(&mut on, "Discord rich presence");
    if on != before.enabled {
        shared.settings.update(|x| x.presence.enabled = on);
    }
    ui.label(
        RichText::new(if before.client_id.is_empty() {
            "Off by default; nothing is sent. It also needs presence.client_id in settings.toml."
        } else {
            "Shows that you are playing and for how long, to a running Discord client. Nothing else is sent."
        })
        .small()
        .weak(),
    );
    ui.add_space(6.0);
    let mut developer = shared.settings.get().developer;
    if ui
        .checkbox(
            &mut developer,
            "Developer tools (adds the Memory page: read, write and search guest RAM)",
        )
        .changed()
    {
        shared.settings.update(|x| x.developer = developer);
    }
    ui.add_space(6.0);
    if ui.button("Make bug report").clicked() {
        shared.request_report();
    }
    ui.label(
        RichText::new("A zip with the log, settings, bindings, system and game state and the picture on screen. No saves, no game data; names, paths and addresses are anonymised. Attach it to the report.")
            .small()
            .weak(),
    );
    let message = shared.report_message();
    if !message.is_empty() {
        ui.label(message);
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new("FEPOR_FPS_MULT, FEPOR_RES and FEPOR_ASPECT override the saved video settings for one run.")
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
