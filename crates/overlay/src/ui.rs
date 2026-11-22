//! The settings pages.

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
    Controls,
    Card,
    Help,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::Video,
        Page::Audio,
        Page::Controls,
        Page::Card,
        Page::Help,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::Video => "Video",
            Page::Audio => "Audio",
            Page::Controls => "Controls",
            Page::Card => "Memory card",
            Page::Help => "Help",
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
}

impl UiState {
    pub fn new(page: Page) -> UiState {
        UiState {
            page,
            card: None,
            card_message: String::new(),
            focus_tabs: true,
        }
    }
}

const ACCENT: Color32 = Color32::from_rgb(120, 190, 255);

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

pub fn show(root: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    let step = shared.take_tab_step();
    if step != 0 {
        let n = Page::ALL.len() as i32;
        let i = Page::ALL.iter().position(|p| *p == st.page).unwrap_or(0) as i32;
        st.page = Page::ALL[(i + step).rem_euclid(n) as usize];
        st.focus_tabs = true;
    }
    let frame = egui::Frame::new()
        .fill(Color32::from_black_alpha(215))
        .inner_margin(14.0);
    egui::CentralPanel::default().frame(frame).show(root, |ui| {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Settings").color(ACCENT));
            ui.add_space(12.0);
            for p in Page::ALL {
                let r = ui.selectable_label(st.page == p, p.label());
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
            Page::Controls => controls(ui, shared),
            Page::Card => card(ui, st, shared),
            Page::Help => help(ui, shared),
        });
        ui.separator();
        ui.label(
            RichText::new("F1 / Guide: close   PgUp-PgDn / LB-RB: page   arrows + Enter / D-pad + A, or the mouse")
                .small()
                .weak(),
        );
    });
    st.focus_tabs = false;
}

fn video(ui: &mut egui::Ui, shared: &Shared) {
    let before = shared.settings.get();
    let mut s = before.clone();
    ui.label("Frame multiplier");
    ui.add(
        egui::Slider::new(&mut s.video.frame_multiplier, 1..=8).custom_formatter(|v, _| {
            if v < 1.5 {
                "x1 (as the game draws)".to_string()
            } else {
                format!("x{v:.0} (interpolated images)")
            }
        }),
    );
    ui.add_space(8.0);
    ui.label("Internal resolution");
    ui.add(
        egui::Slider::new(&mut s.video.resolution, 1..=4)
            .custom_formatter(|v, _| format!("x{v:.0}  ({}x{})", 640 * v as u32, 528 * v as u32)),
    );
    ui.add_space(8.0);
    ui.label("Picture shape");
    for a in Aspect::ALL {
        ui.radio_value(&mut s.video.aspect, a, a.label());
    }
    ui.add_space(8.0);
    ui.checkbox(&mut s.video.fullscreen, "Fullscreen");
    if s != before {
        shared.settings.update(|x| *x = s);
    }
}

fn audio(ui: &mut egui::Ui, shared: &Shared) {
    let before = shared.settings.get();
    let mut pct = (before.audio.volume * 100.0).round() as u32;
    ui.label("Master volume");
    ui.add(egui::Slider::new(&mut pct, 0..=100).suffix("%"));
    let volume = pct as f32 / 100.0;
    if (volume - before.audio.volume).abs() > 0.004 {
        shared.settings.update(|x| x.audio.volume = volume);
    }
}

fn controls(ui: &mut egui::Ui, shared: &Shared) {
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
    ui.horizontal(|ui| {
        ui.label("Button names");
        for g in [
            GlyphSet::GameCube,
            GlyphSet::Xbox,
            GlyphSet::Nintendo,
            GlyphSet::PlayStation,
        ] {
            if ui.radio(cfg.glyphs == g, format!("{g:?}")).clicked() {
                c.set_glyphs(g);
            }
        }
    });
    let mut dz = cfg.deadzone;
    ui.horizontal(|ui| {
        ui.label("Stick dead zone");
        ui.add(egui::Slider::new(&mut dz, 0.0..=0.9));
    });
    if (dz - cfg.deadzone).abs() > 1e-4 {
        c.set_deadzone(dz);
    }
    ui.add_space(6.0);
    let mut rebind = None;
    egui::Grid::new("bindings")
        .striped(true)
        .num_columns(5)
        .show(ui, |ui| {
            ui.label(RichText::new("GameCube").strong());
            ui.label(RichText::new("Keyboard").strong());
            ui.label("");
            ui.label(RichText::new("Gamepad").strong());
            ui.label("");
            ui.end_row();
            for a in Action::ALL {
                ui.label(a.label());
                for keyboard in [true, false] {
                    let names: Vec<String> = cfg
                        .sources_of(keyboard, a)
                        .into_iter()
                        .map(|s| {
                            if keyboard {
                                s.to_string()
                            } else {
                                cfg.glyphs.source_label(s)
                            }
                        })
                        .collect();
                    ui.label(if names.is_empty() {
                        "-".to_string()
                    } else {
                        names.join(", ")
                    });
                    if ui.small_button("Rebind").clicked() {
                        rebind = Some((keyboard, a));
                    }
                }
                ui.end_row();
            }
        });
    if let Some((keyboard, a)) = rebind {
        c.rebind(keyboard, a);
    }
    ui.add_space(6.0);
    if ui.button("Restore the default bindings").clicked() {
        c.reset_bindings();
    }
}

fn card(ui: &mut egui::Ui, st: &mut UiState, shared: &Shared) {
    let path = shared.card_path.lock().ok().and_then(|p| p.clone());
    let Some(path) = path else {
        ui.label("No memory card in slot A.");
        return;
    };
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
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!(
            ".gci saves put in {} are imported into the card the next time the game starts.",
            shared.data_dir.join("imports").display()
        ))
        .small()
        .weak(),
    );
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
        RichText::new("FEPOR_FPS_MULT, FEPOR_RES and FEPOR_ASPECT override the saved video settings for one run.")
            .small()
            .weak(),
    );
}
