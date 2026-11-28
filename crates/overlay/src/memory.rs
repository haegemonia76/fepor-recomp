//! The memory page (developer option, ADR 0048): read and write guest addresses, watch values, search RAM.
//! It runs on the render thread and never touches guest RAM: everything goes through the [`Link`] and what the
//! page shows is the copy the guest thread published after its last frame.

use egui::RichText;
use fepor_core::memwatch::{self, Filter, Link, Request, Width};

use crate::shared::Shared;

/// Painted frames between two steps of `FEPOR_MEMSCRIPT`, so that the game runs between them.
/// Candidates written to the state file (the page lists every published one, in a scroll area).
const STATE_HITS: usize = 8;
/// Height of the candidate list: fixed, so the page does not move as the search narrows.
const LIST_HEIGHT: f32 = 200.0;

/// Wide enough for `0x80123456` and a decimal 32-bit value.
const FIELD: f32 = 150.0;

const SCRIPT_STEP_FRAMES: u32 = 45;
/// Painted frames between two polls of `FEPOR_MEMSCRIPT_FILE` and between its steps.
const LIVE_POLL_FRAMES: u32 = 10;

struct Live {
    path: std::path::PathBuf,
    /// Lines of the file already queued.
    read: usize,
    polls: u32,
    last_state: String,
}

pub struct MemoryView {
    scan_width: Width,
    scan_value: String,
    /// The address the writer and the export work on.
    addr: String,
    width: Width,
    value: String,
    hold: bool,
    view_addr: String,
    watches: Vec<(u32, Width)>,
    name: String,
    /// Remaining steps of `FEPOR_MEMSCRIPT` (first is next) and the painted frames until it runs.
    script: Vec<String>,
    script_wait: u32,
    /// `FEPOR_MEMSCRIPT_FILE`: polled for appended steps; the page's state goes to `<file>.state`.
    live: Option<Live>,
    sent_view: Option<(u32, u32)>,
    hits_from: String,
    sent_from: Option<u32>,
}

impl MemoryView {
    pub fn new() -> MemoryView {
        // FEPOR_MEMSCRIPT="new u32 unknown;changed;inc;watch 0x80001234 u32" runs steps as a player would (frame dumps)
        let script = std::env::var("FEPOR_MEMSCRIPT")
            .map(|s| {
                s.split(';')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .rev()
                    .collect()
            })
            .unwrap_or_default();
        MemoryView {
            scan_width: Width::U32,
            scan_value: String::new(),
            addr: String::new(),
            width: Width::U32,
            value: String::new(),
            hold: false,
            view_addr: "0x80000000".into(),
            watches: Vec::new(),
            name: "Found value".into(),
            script,
            script_wait: SCRIPT_STEP_FRAMES,
            live: std::env::var_os("FEPOR_MEMSCRIPT_FILE").map(|p| Live {
                path: p.into(),
                read: 0,
                polls: 0,
                last_state: String::new(),
            }),
            sent_view: None,
            hits_from: "0x80000000".into(),
            sent_from: None,
        }
    }

    fn watch(&mut self, link: &Link, addr: u32, width: Width) {
        if !self.watches.contains(&(addr, width)) {
            self.watches.push((addr, width));
        }
        link.send(Request::Watch(self.watches.clone()));
    }

    fn select(&mut self, addr: u32, width: Width, value: Option<u32>) {
        self.addr = format!("{addr:#010x}");
        self.width = width;
        if let Some(v) = value {
            self.value = v.to_string();
        }
    }

    fn poke(&self, link: &Link, hold: bool) -> Result<(), String> {
        let addr = memwatch::parse_number(&self.addr).ok_or("address is not a number")?;
        let value = memwatch::parse_number(&self.value).ok_or("value is not a number")?;
        link.send(Request::Poke {
            addr,
            width: self.width,
            value,
            hold,
        });
        Ok(())
    }

    /// One scripted step, in the words of the buttons: `new u32 unknown`, `new u16 equal 5`, `changed`,
    /// `unchanged`, `inc`, `dec`, `equal N`, `watch ADDR [width]`, `view ADDR`, `poke ADDR width VALUE [hold]`,
    /// `select ADDR width [value]` (target of the export), `apply AMOUNT KEY` (the Apply button of an add/set cheat).
    fn run_step(&mut self, shared: &Shared, line: &str) -> Result<(), String> {
        let link = &*shared.memory;
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |s: &str| memwatch::parse_number(s).ok_or(format!("{s}: not a number"));
        let width = |s: &str| Width::parse(s).ok_or(format!("{s}: not a width"));
        match words.as_slice() {
            ["new", w, rest @ ..] => {
                let equal = match rest {
                    ["unknown"] => None,
                    ["equal", n] => Some(num(n)?),
                    _ => return Err(format!("{line}: expected unknown or equal N")),
                };
                link.send(Request::NewScan {
                    width: width(w)?,
                    equal,
                });
            }
            ["watch", a] => self.watch(link, num(a)?, Width::U32),
            ["watch", a, w] => self.watch(link, num(a)?, width(w)?),
            ["view", a] => self.view_addr = format!("{:#010x}", num(a)?),
            // `apply AMOUNT native:Cheat name`: the Apply button of an add/set cheat (ADR 0033), for headless runs
            ["apply", n, key @ ..] if !key.is_empty() => {
                let amount = n.replace(['_', ','], "").parse::<i64>().map_err(|_| format!("{n}: not a number"))?;
                shared.cheats.request(&key.join(" "), amount);
            }
            // `stat LABEL VALUE`: Apply of a row of the character table (the displayed character's base)
            ["stat", label @ .., v] if !label.is_empty() => {
                let value = v.parse::<i64>().map_err(|_| format!("{v}: not a number"))?;
                shared.cheats.request_stat(&label.join(" "), value);
            }
            ["clear"] => link.send(Request::ClearScan),
            ["release", a] => link.send(Request::Release(num(a)?)),
            [cmd @ ("poke" | "write" | "hold"), a, w, v, rest @ ..] => {
                self.addr = a.to_string();
                self.width = width(w)?;
                self.value = v.to_string();
                self.poke(link, *cmd == "hold" || rest == ["hold"])?;
            }
            ["select", a, w] => self.select(num(a)?, width(w)?, None),
            ["select", a, w, v] => self.select(num(a)?, width(w)?, Some(num(v)?)),
            _ => match Filter::parse(line) {
                Some(f) => link.send(Request::Narrow(f)),
                None => return Err(format!("{line}: unknown step")),
            },
        }
        Ok(())
    }
}

impl MemoryView {
    /// Whether the guest thread must keep publishing for the state file.
    pub fn live(&self) -> bool {
        self.live.is_some()
    }

    /// Once per painted image, drawn page or not: poll the step file, run one scripted step when due, and
    /// rewrite the state file when the page's state changed.
    pub fn tick(&mut self, shared: &Shared) {
        let link = &*shared.memory;
        if let Some(l) = &mut self.live {
            l.polls += 1;
            if l.polls % LIVE_POLL_FRAMES == 1 {
                if let Ok(text) = std::fs::read_to_string(&l.path) {
                    // only whole lines: a line still being written has no newline yet
                    let whole = text.rfind('\n').map_or("", |i| &text[..i]);
                    let lines: Vec<&str> = if text.is_empty() {
                        Vec::new()
                    } else {
                        whole.lines().collect()
                    };
                    for line in lines.iter().skip(l.read) {
                        let line = line.trim();
                        if !line.is_empty() && !line.starts_with('#') {
                            self.script.insert(0, line.to_owned());
                        }
                    }
                    l.read = lines.len().max(l.read);
                }
            }
        }
        if !self.script.is_empty() {
            if self.script_wait == 0 {
                let step = self.script.pop().unwrap_or_default();
                if let Err(e) = self.run_step(shared, &step) {
                    eprintln!("memory script: {e}");
                }
                self.script_wait = if self.live.is_some() {
                    LIVE_POLL_FRAMES
                } else {
                    SCRIPT_STEP_FRAMES
                };
            } else {
                self.script_wait -= 1;
            }
        }
        if let Some(l) = &mut self.live {
            let state = state_text(&link.published());
            if state != l.last_state {
                let mut name = l.path.clone().into_os_string();
                name.push(".state");
                if let Err(e) = std::fs::write(&name, &state) {
                    eprintln!("memory state: {e}");
                }
                l.last_state = state;
            }
        }
    }
}

/// The page's results as text for scripts: the message, the candidates and the watch list.
fn state_text(p: &memwatch::Published) -> String {
    let mut s = format!(
        "game {}\nmessage {}\n",
        p.game.as_deref().unwrap_or("-"),
        p.message
    );
    match &p.scan {
        Some(scan) => {
            s += &format!("candidates {}\n", scan.count);
            for h in scan.hits.iter().take(STATE_HITS) {
                s += &format!("candidate {:08X} was {} now {}\n", h.addr, h.at_scan, h.now);
            }
        }
        None => s += "candidates none\n",
    }
    for (a, w, v) in &p.watches {
        match v {
            Some(v) => s += &format!("watch {a:08X} {} {v}\n", w.label()),
            None => s += &format!("watch {a:08X} {} outside\n", w.label()),
        }
    }
    for h in &p.held {
        s += &format!("held {:08X} {}\n", h.addr, h.value);
    }
    s
}

fn mono(s: impl Into<String>) -> RichText {
    RichText::new(s).monospace()
}

pub fn memory(ui: &mut egui::Ui, st: &mut MemoryView, shared: &Shared) {
    let link = &*shared.memory;
    let view_req = memwatch::parse_number(&st.view_addr).map(|a| (a, 128));
    if let Some(v) = view_req.filter(|v| st.sent_view != Some(*v)) {
        link.send(Request::View {
            addr: v.0,
            len: v.1,
        });
        st.sent_view = Some(v);
    }
    let p = link.published();
    ui.label(
        RichText::new(
            "Values are copies the game thread took at a frame boundary; nothing here reads the game's memory directly.",
        )
        .small()
        .weak(),
    );
    // the changing parts sit on a short line of fixed-width cells, so nothing above or below re-wraps
    ui.label(
        mono(format!(
            "game {:<8} frame {:>9}",
            p.game.as_deref().unwrap_or("-"),
            p.frame
        ))
        .small()
        .weak(),
    );
    // always one line, so the page below does not move when a message comes or goes
    ui.colored_label(
        crate::theme::GOLD,
        if p.message.is_empty() {
            " "
        } else {
            &p.message
        },
    );
    ui.add_space(6.0);
    scan_section(ui, st, link, &p);
    ui.separator();
    target_section(ui, st, link, &p);
    ui.separator();
    view_section(ui, st, &p);
}

fn scan_section(ui: &mut egui::Ui, st: &mut MemoryView, link: &Link, p: &memwatch::Published) {
    ui.label(crate::theme::caption("Search"));
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("memory_scan_width")
            .selected_text(st.scan_width.label())
            .show_ui(ui, |ui| {
                for w in Width::ALL {
                    ui.selectable_value(&mut st.scan_width, w, w.label());
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut st.scan_value)
                .desired_width(FIELD)
                .hint_text("value"),
        );
        let value = memwatch::parse_number(&st.scan_value);
        if ui
            .add_enabled(value.is_some(), egui::Button::new("New: equals"))
            .clicked()
        {
            link.send(Request::NewScan {
                width: st.scan_width,
                equal: value,
            });
        }
        if ui.button("New: unknown value").clicked() {
            link.send(Request::NewScan {
                width: st.scan_width,
                equal: None,
            });
        }
        if ui.button("Clear").clicked() {
            link.send(Request::ClearScan);
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Keep the ones that:");
        let value = memwatch::parse_number(&st.scan_value);
        if ui
            .add_enabled(value.is_some(), egui::Button::new("equal the value"))
            .clicked()
        {
            link.send(Request::Narrow(Filter::Equal(value.unwrap_or(0))));
        }
        for (label, f) in [
            ("changed", Filter::Changed),
            ("did not change", Filter::Unchanged),
            ("increased", Filter::Increased),
            ("decreased", Filter::Decreased),
        ] {
            if ui.button(label).clicked() {
                link.send(Request::Narrow(f));
            }
        }
    });
    let Some(scan) = &p.scan else {
        ui.label(RichText::new("No search running: pick a size and start one. Each step compares with the step before.").small().weak());
        return;
    };
    let width = scan.width.unwrap_or(Width::U32);
    ui.horizontal(|ui| {
        ui.label("Show from address");
        let r = ui.add(egui::TextEdit::singleline(&mut st.hits_from).desired_width(FIELD));
        if r.changed() || st.sent_from.is_none() {
            let from = memwatch::parse_number(&st.hits_from).unwrap_or(0);
            if st.sent_from != Some(from) {
                link.send(Request::HitsFrom(from));
                st.sent_from = Some(from);
            }
        }
        ui.label(mono(format!(
            "{:>3} shown of {:>9}",
            scan.hits.len(),
            scan.count
        )));
    });
    egui::ScrollArea::vertical()
        .id_salt("memory_hits_scroll")
        .min_scrolled_height(LIST_HEIGHT)
        .max_height(LIST_HEIGHT)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("memory_hits")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    for h in &scan.hits {
                        ui.label(mono(format!(
                            "{:08X}  was {:>10}  now {:>10}",
                            h.addr, h.at_scan, h.now
                        )));
                        if ui.small_button("Use").clicked() {
                            st.select(h.addr, width, Some(h.now));
                        }
                        if ui.small_button("Watch").clicked() {
                            st.watch(link, h.addr, width);
                        }
                        ui.end_row();
                    }
                });
        });
    ui.label(
        RichText::new(
            "was = value at the previous search step, now = value in memory at the latest frame. Lowest addresses first; raise \"Show from address\" or narrow the search to see others.",
        )
        .small()
        .weak(),
    );
}

fn target_section(ui: &mut egui::Ui, st: &mut MemoryView, link: &Link, p: &memwatch::Published) {
    ui.label(crate::theme::caption("Watch and write"));
    let mut remove = None;
    egui::Grid::new("memory_watches")
        .num_columns(5)
        .striped(true)
        .show(ui, |ui| {
            for (i, (addr, width, value)) in p.watches.iter().enumerate() {
                ui.label(mono(format!("{addr:08X}")));
                ui.label(width.label());
                match value {
                    Some(v) => ui.label(mono(format!(
                        "{v:#0w$x} {v:>10}",
                        w = 2 + 2 * width.bytes()
                    ))),
                    None => ui.label("outside RAM"),
                };
                if ui.small_button("Use").clicked() {
                    st.select(*addr, *width, *value);
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        st.watches.remove(i);
        link.send(Request::Watch(st.watches.clone()));
    }
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.addr)
                .desired_width(FIELD)
                .hint_text("address"),
        );
        egui::ComboBox::from_id_salt("memory_width")
            .selected_text(st.width.label())
            .show_ui(ui, |ui| {
                for w in Width::ALL {
                    ui.selectable_value(&mut st.width, w, w.label());
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut st.value)
                .desired_width(FIELD)
                .hint_text("value"),
        );
        ui.checkbox(&mut st.hold, "hold every frame");
    });
    let addr = memwatch::parse_number(&st.addr);
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(addr.is_some(), egui::Button::new("Watch"))
            .clicked()
        {
            st.watch(link, addr.unwrap_or(0), st.width);
        }
        if ui
            .add_enabled(addr.is_some(), egui::Button::new("Write"))
            .clicked()
        {
            let _ = st.poke(link, st.hold);
        }
        if ui
            .add_enabled(addr.is_some(), egui::Button::new("Release"))
            .clicked()
        {
            link.send(Request::Release(addr.unwrap_or(0)));
        }
    });
    for h in &p.held {
        ui.label(mono(format!("holding {:08X} = {:#x}", h.addr, h.value)));
    }
    let (Some(addr), Some(value)) = (addr, memwatch::parse_number(&st.value)) else {
        return;
    };
    ui.add_space(4.0);
    ui.label(crate::theme::caption("Export"));
    ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(240.0));
    let toml = memwatch::cheat_toml(&st.name, p.game.as_deref(), addr, st.width, value, st.hold);
    let gecko = memwatch::gecko_line(addr, st.width, value);
    ui.horizontal(|ui| {
        ui.label(mono(format!("Gecko: {gecko}")));
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(gecko.clone());
        }
    });
    ui.horizontal(|ui| {
        ui.label("Native cheat (TOML):");
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(toml.clone());
        }
    });
    ui.add(egui::Label::new(mono(toml.trim_end())).wrap());
}

fn view_section(ui: &mut egui::Ui, st: &mut MemoryView, p: &memwatch::Published) {
    ui.label(crate::theme::caption("Memory"));
    ui.add(egui::TextEdit::singleline(&mut st.view_addr).desired_width(FIELD));
    for (i, row) in p.view.chunks(16).take(8).enumerate() {
        let hex: Vec<String> = row.iter().map(|b| format!("{b:02X}")).collect();
        let text: String = row
            .iter()
            .map(|&b| {
                if (0x20..0x7F).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        ui.label(mono(format!(
            "{:08X}  {:<47}  {text}",
            p.view_addr.wrapping_add(i as u32 * 16),
            hex.join(" ")
        )));
    }
}
