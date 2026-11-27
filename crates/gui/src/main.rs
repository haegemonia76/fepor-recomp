//! egui front end: pick the game file and the output folder, run the recompilation.
//!
//! Nothing else lives here on purpose (settings are an in-game overlay, see ticket M5.4). The work is done
//! by the `fepor-recomp prepare` command, started as a child process so it can be cancelled; its
//! `progress <0..1> <text>` lines drive the progress bar and its exit code selects the error message.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};

/// The supported release.
const GAME_ID: &str = "GFEP01";

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    game: String,
    output: String,
}

fn config_path() -> PathBuf {
    fepor_core::paths::gui_config_path()
}

/// Message for the exit codes of `fepor-recomp prepare` (same texts as `packaging/recomplauncher.json`).
fn exit_message(code: i32) -> String {
    match code {
        10 => "The disc image could not be read. Pick the .iso, .rvz or .gcz of Fire Emblem: Path of Radiance (GFEP01).".into(),
        11 => format!("This disc is not the European release ({GAME_ID}) this recompilation supports."),
        20 => "Building the game needs clang, CMake and Ninja. Install them and try again.".into(),
        30 => "Building the game failed. The log below has the details.".into(),
        -1 => "The recompilation was interrupted.".into(),
        other => format!("The recompilation stopped with exit code {other}."),
    }
}

/// What checking the chosen file says.
enum GameCheck {
    Empty,
    Ok(String),
    Bad(String),
}

fn check_game(path: &str) -> GameCheck {
    if path.trim().is_empty() {
        return GameCheck::Empty;
    }
    let p = Path::new(path.trim());
    if !p.exists() {
        return GameCheck::Bad("This file or folder does not exist.".into());
    }
    let mut disc = match fepor_core::open_disc(p) {
        Ok(d) => d,
        Err(_) => return GameCheck::Bad(
            "This is not a GameCube disc image (.iso, .gcm, .rvz, .gcz) or extracted disc folder."
                .into(),
        ),
    };
    let id = disc.game_id().to_string();
    match disc
        .dol()
        .ok()
        .map(|dol| fepor_core::version::check(&id, &dol))
    {
        Some(Ok(_)) => GameCheck::Ok(format!("{id}: Fire Emblem: Path of Radiance (Europe)")),
        _ => GameCheck::Bad(format!(
            "This disc is {id}; only the European release ({GAME_ID}) is supported."
        )),
    }
}

/// The `fepor-recomp` executable next to this one, or the one on the PATH.
fn recomp_program() -> PathBuf {
    let name = if cfg!(windows) {
        "fepor-recomp.exe"
    } else {
        "fepor-recomp"
    };
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        let p = dir.join(name);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(name)
}

enum Msg {
    Progress(f32, String),
    Line(String),
    Exit(i32),
}

enum State {
    Idle,
    Running,
    Finished(Result<(), String>),
}

/// The mods of the output folder (ADR 0034), read again when the folder changes or on Refresh.
#[derive(Default)]
struct ModsPanel {
    data: Option<String>,
    list: fepor_core::mods::ModList,
    error: String,
}

fn mods_section(ui: &mut egui::Ui, st: &mut ModsPanel, output: &str) {
    if output.is_empty() {
        return;
    }
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Mods").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!("Folder: {}", fepor_core::mods::mods_dir(Path::new(output)).display()));
            if ui.button("Refresh").clicked() {
                st.data = None;
            }
        });
        if st.data.as_deref() != Some(output) {
            st.list = fepor_core::mods::ModList::load(Path::new(output));
            st.data = Some(output.to_string());
        }
        ui.label("Changes apply the next time the game starts. The first mod has the highest priority.");
        let list = &mut st.list;
        let (mut changed, mut shift) = (false, None);
        let last = list.entries.len().saturating_sub(1);
        if list.entries.is_empty() && list.broken.is_empty() {
            ui.label("No mods found: put a folder with a mod.toml there.");
        }
        for (i, e) in list.entries.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let m = &e.m.manifest;
                changed |= ui.checkbox(&mut e.enabled, format!("{} {}", m.name, m.version)).changed();
                if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                    shift = Some((i, true));
                }
                if ui.add_enabled(i < last, egui::Button::new("Down")).clicked() {
                    shift = Some((i, false));
                }
            });
            let m = &e.m;
            ui.label(
                egui::RichText::new(format!(
                    "{}{}{} file(s), {} patch(es), game {}",
                    m.manifest.description,
                    if m.manifest.description.is_empty() { "" } else { " - " },
                    m.files.len(),
                    m.manifest.patches.len(),
                    m.manifest.game_id
                ))
                .small(),
            );
        }
        if let Some((i, up)) = shift {
            changed |= list.shift(i, up);
        }
        for (folder, reason) in &list.broken {
            ui.colored_label(egui::Color32::from_rgb(200, 70, 60), format!("{folder}: cannot be read: {reason}"));
        }
        for c in list.conflicts(Some(GAME_ID)) {
            ui.colored_label(egui::Color32::from_rgb(200, 150, 40), c.to_string());
        }
        if changed {
            st.error = list.save().err().map(|e| format!("cannot save the mod list: {e}")).unwrap_or_default();
        }
        if !st.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(200, 70, 60), &st.error);
        }
    });
}

struct App {
    /// `FEPOR_GUI_AUTOSTART=1`: start the recompilation as soon as the window opens (for scripted checks).
    autostart: bool,
    game: String,
    output: String,
    check: GameCheck,
    state: State,
    progress: f32,
    status: String,
    log: Vec<String>,
    rx: Option<Receiver<Msg>>,
    child: Option<Arc<Mutex<Child>>>,
    cancelled: bool,
    /// Where the last bug report went, or why it failed.
    report: Option<Result<String, String>>,
    mods: ModsPanel,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        let _ = &cc.egui_ctx;
        let saved: Saved = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        let check = check_game(&saved.game);
        App {
            autostart: std::env::var_os("FEPOR_GUI_AUTOSTART").is_some(),
            game: saved.game,
            output: saved.output,
            check,
            state: State::Idle,
            progress: 0.0,
            status: String::new(),
            log: Vec::new(),
            rx: None,
            child: None,
            cancelled: false,
            report: None,
            mods: ModsPanel::default(),
        }
    }

    fn save(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = toml::to_string(&Saved {
            game: self.game.clone(),
            output: self.output.clone(),
        }) {
            let _ = std::fs::write(path, text);
        }
    }

    fn can_start(&self) -> bool {
        matches!(self.check, GameCheck::Ok(_))
            && !self.output.trim().is_empty()
            && !matches!(self.state, State::Running)
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.save();
        self.log.clear();
        self.progress = 0.0;
        self.status = "Starting".into();
        self.cancelled = false;
        let (tx, rx) = channel();
        let mut cmd = Command::new(recomp_program());
        cmd.args([
            "prepare",
            "--game",
            self.game.trim(),
            "--data",
            self.output.trim(),
            "--build-type",
            "Release",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let child = Arc::new(Mutex::new(child));
                self.child = Some(child.clone());
                if let Some(out) = stdout {
                    pump(out, tx.clone(), ctx.clone(), true);
                }
                if let Some(err) = stderr {
                    pump(err, tx.clone(), ctx.clone(), false);
                }
                let ctx2 = ctx.clone();
                std::thread::spawn(move || loop {
                    let status = child
                        .lock()
                        .ok()
                        .and_then(|mut c| c.try_wait().ok().flatten());
                    if let Some(s) = status {
                        let _ = tx.send(Msg::Exit(s.code().unwrap_or(-1)));
                        ctx2.request_repaint();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                });
                self.rx = Some(rx);
                self.state = State::Running;
            }
            Err(e) => {
                self.state = State::Finished(Err(format!(
                    "Could not start {}: {e}",
                    recomp_program().display()
                )));
            }
        }
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(c) = &self.child {
            if let Ok(mut c) = c.lock() {
                let _ = c.kill();
            }
        }
    }

    fn drain(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut exit = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Progress(f, t) => {
                    self.progress = f;
                    self.status = t;
                }
                Msg::Line(l) => {
                    if self.log.len() < 5000 {
                        self.log.push(l);
                    }
                }
                Msg::Exit(c) => exit = Some(c),
            }
        }
        if let Some(code) = exit {
            self.rx = None;
            self.child = None;
            self.state = State::Finished(match code {
                _ if self.cancelled => Err("Cancelled.".into()),
                0 => Ok(()),
                c => Err(exit_message(c)),
            });
            if matches!(self.state, State::Finished(Ok(_))) {
                self.progress = 1.0;
            }
        }
    }
}

/// Forward the lines of a child pipe: stdout `progress` lines become progress updates.
fn pump(
    pipe: impl std::io::Read + Send + 'static,
    tx: Sender<Msg>,
    ctx: egui::Context,
    is_stdout: bool,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            let msg = match (is_stdout, line.strip_prefix("progress ")) {
                (true, Some(rest)) => {
                    let (f, text) = rest.split_once(' ').unwrap_or((rest, ""));
                    f.parse::<f32>()
                        .map(|f| Msg::Progress(f, text.to_string()))
                        .unwrap_or(Msg::Line(line.clone()))
                }
                _ => Msg::Line(line),
            };
            if tx.send(msg).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        self.drain();
        if self.autostart {
            self.autostart = false;
            if self.can_start() {
                self.start(ctx);
            }
        }
        egui::CentralPanel::default().show(root, |ui| {
            ui.heading("Fire Emblem: Path of Radiance");
            ui.label("Recompile the game from your own copy of the disc. The game data is copied to the output folder (about 1.3 GB), so the disc is only needed once.");
            ui.add_space(12.0);
            let running = matches!(self.state, State::Running);

            ui.label("Game (disc image or extracted folder)");
            ui.horizontal(|ui| {
                let changed = ui
                    .add_enabled(
                        !running,
                        egui::TextEdit::singleline(&mut self.game)
                            .desired_width(ui.available_width() - 190.0),
                    )
                    .changed();
                if ui
                    .add_enabled(!running, egui::Button::new("Choose file..."))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("GameCube disc image", &["iso", "gcm", "rvz", "gcz"])
                        .pick_file()
                    {
                        self.game = p.display().to_string();
                        self.check = check_game(&self.game);
                    }
                }
                if ui
                    .add_enabled(!running, egui::Button::new("Folder..."))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.game = p.display().to_string();
                        self.check = check_game(&self.game);
                    }
                }
                if changed {
                    self.check = check_game(&self.game);
                }
            });
            match &self.check {
                GameCheck::Empty => {}
                GameCheck::Ok(m) => {
                    ui.colored_label(egui::Color32::from_rgb(60, 160, 80), m);
                }
                GameCheck::Bad(m) => {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 60), m);
                }
            }

            ui.add_space(8.0);
            ui.label("Output folder");
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.output)
                        .desired_width(ui.available_width() - 100.0),
                );
                if ui
                    .add_enabled(!running, egui::Button::new("Choose..."))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.output = p.display().to_string();
                    }
                }
            });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if running {
                    if ui.button("Cancel").clicked() {
                        self.cancel();
                    }
                } else if ui
                    .add_enabled(self.can_start(), egui::Button::new("Recompile"))
                    .clicked()
                {
                    self.start(ctx);
                }
                if ui
                    .add_enabled(
                        !running && !self.output.trim().is_empty(),
                        egui::Button::new("Bug report"),
                    )
                    .on_hover_text("Zip the log, settings and system information (no saves, no game data) to attach to a report")
                    .clicked()
                {
                    self.report = Some(
                        fepor_core::bugreport::create_default(Path::new(self.output.trim()), Default::default())
                            .map(|p| p.display().to_string()),
                    );
                }
            });
            match &self.report {
                Some(Ok(p)) => {
                    ui.label(format!("Saved {p}"));
                }
                Some(Err(e)) => {
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 60), e);
                }
                None => {}
            }

            if running || self.progress > 0.0 {
                ui.add_space(8.0);
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .text(self.status.clone())
                        .show_percentage(),
                );
            }
            match &self.state {
                State::Finished(Ok(())) => {
                    ui.add_space(8.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(60, 160, 80),
                        format!("Done. The game and its data are in {}; the disc image is no longer needed.", self.output.trim()),
                    );
                }
                State::Finished(Err(e)) => {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(200, 70, 60), e);
                }
                _ => {}
            }
            mods_section(ui, &mut self.mods, self.output.trim());
            if !self.log.is_empty() {
                ui.add_space(8.0);
                egui::CollapsingHeader::new("Log")
                    .default_open(matches!(self.state, State::Finished(Err(_))))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for l in &self.log {
                                    ui.monospace(l);
                                }
                            });
                    });
            }
        });
        if matches!(self.state, State::Running) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 420.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "fepor-recomp",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_have_the_launcher_messages() {
        assert!(exit_message(10).contains("could not be read"));
        assert!(exit_message(11).contains("GFEP01"));
        assert!(exit_message(20).contains("clang"));
        assert!(exit_message(30).contains("failed"));
        assert!(exit_message(2).contains("exit code 2"));
        assert!(exit_message(-1).contains("interrupted"));
    }

    #[test]
    fn a_missing_path_is_reported_not_opened() {
        assert!(matches!(check_game(""), GameCheck::Empty));
        assert!(matches!(
            check_game("/definitely/not/here.iso"),
            GameCheck::Bad(_)
        ));
    }
}
