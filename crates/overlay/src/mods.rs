//! The mods page (ADR 0034): which mods are on and in what order. Changes are written to `mods.toml` at once
//! and take effect the next time the game starts.

use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use fepor_core::mods::ModList;

use crate::shared::Shared;

/// The list as read from disk, re-read every couple of seconds while the page is shown.
#[derive(Default)]
pub struct ModsView {
    list: Option<(Instant, ModList)>,
    error: String,
}

pub fn page(ui: &mut egui::Ui, st: &mut ModsView, shared: &Shared) {
    if st
        .list
        .as_ref()
        .is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(2))
    {
        st.list = Some((Instant::now(), ModList::load(&shared.data_dir)));
    }
    let Some((_, list)) = &mut st.list else {
        return;
    };
    ui.add(
        egui::Label::new(format!(
            "Mods in {}. Changes apply the next time the game starts. The first mod has the highest priority.",
            fepor_core::mods::mods_dir(&shared.data_dir).display()
        ))
        .wrap(),
    );
    ui.add_space(8.0);
    if list.entries.is_empty() && list.broken.is_empty() {
        ui.label("No mods found: put a folder with a mod.toml there.");
    }
    let mut changed = false;
    let last = list.entries.len().saturating_sub(1);
    let mut shift = None;
    for (i, e) in list.entries.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut e.enabled, "").changed();
            ui.label(
                RichText::new(&e.m.manifest.name)
                    .strong()
                    .color(crate::theme::GOLD),
            );
            let m = &e.m.manifest;
            ui.label(
                RichText::new(format!("{} {}", m.version, m.author))
                    .small()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(i < last, egui::Button::new("Down"))
                    .clicked()
                {
                    shift = Some((i, false));
                }
                if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                    shift = Some((i, true));
                }
            });
        });
        let m = &e.m;
        let mut what = Vec::new();
        if !m.files.is_empty() {
            what.push(format!("{} replaced file(s)", m.files.len()));
        }
        if !m.manifest.patches.is_empty() {
            what.push(format!("{} memory patch(es)", m.manifest.patches.len()));
        }
        let text = [
            m.manifest.description.as_str(),
            &what.join(", "),
            &format!("game {}", m.manifest.game_id),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" - ");
        ui.add(egui::Label::new(RichText::new(text).small()).wrap());
        ui.add_space(4.0);
    }
    if let Some((i, up)) = shift {
        changed |= list.shift(i, up);
    }
    for (folder, reason) in &list.broken {
        ui.colored_label(
            Color32::LIGHT_RED,
            format!("{folder}: cannot be read: {reason}"),
        );
    }
    let conflicts = list.conflicts(None);
    if !conflicts.is_empty() {
        ui.add_space(6.0);
        ui.label(crate::theme::caption("Conflicts"));
        for c in conflicts.iter().take(12) {
            ui.add(
                egui::Label::new(
                    RichText::new(c.to_string())
                        .small()
                        .color(Color32::from_rgb(240, 200, 70)),
                )
                .wrap(),
            );
        }
        if conflicts.len() > 12 {
            ui.label(format!("... and {} more", conflicts.len() - 12));
        }
    }
    if changed {
        st.error = match list.save() {
            Ok(()) => String::new(),
            Err(e) => format!("cannot save the mod list: {e}"),
        };
    }
    if !st.error.is_empty() {
        ui.colored_label(Color32::LIGHT_RED, &st.error);
    }
}
