//! The in-game settings overlay (ADR 0020): an egui UI drawn over the game's window by the renderer.
//!
//! Threads: the window thread and the gamepad thread push input into [`Shared`]; the render thread owns the
//! [`Overlay`], runs the UI once per presented image and paints it with our own wgpu painter.

mod paint;
mod shared;
mod stats;
mod ui;

use std::sync::Arc;
use std::time::Instant;

pub use shared::Shared;
pub use stats::{StatsSnapshot, ThreadLoad};

pub struct Overlay {
    ctx: egui::Context,
    shared: Arc<Shared>,
    painter: Option<(paint::Painter, wgpu::TextureFormat)>,
    start: Instant,
    ui: ui::UiState,
}

impl Overlay {
    pub fn new(shared: Arc<Shared>) -> Overlay {
        // FEPOR_OVERLAY=<page> opens the overlay at start (debugging, frame dumps)
        let wanted = std::env::var("FEPOR_OVERLAY").ok();
        let page = wanted
            .as_deref()
            .and_then(ui::Page::parse)
            .unwrap_or(ui::Page::Video);
        if wanted.is_some() {
            shared.set_open(true);
        }
        Overlay {
            ctx: egui::Context::default(),
            shared,
            painter: None,
            start: Instant::now(),
            ui: ui::UiState::new(page),
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn is_open(&self) -> bool {
        self.shared.is_open()
    }

    /// Run the UI and draw it over `view` when the overlay is open. `size_px` is the target's size and `ppp`
    /// the window's scale factor.
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size_px: [u32; 2],
        ppp: f32,
    ) {
        let open = self.shared.is_open();
        let hud = self.shared.settings.get().video.show_stats;
        if !open && !hud {
            return;
        }
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size_px[0] as f32 / ppp, size_px[1] as f32 / ppp),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: self.shared.take_events(),
            focused: true,
            ..Default::default()
        };
        raw.viewports.insert(
            raw.viewport_id,
            egui::ViewportInfo {
                native_pixels_per_point: Some(ppp),
                ..Default::default()
            },
        );
        let (shared, state) = (&self.shared, &mut self.ui);
        let out = self
            .ctx
            .run_ui(raw, |root| ui::show(root, state, shared, open, hud));
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        if self.painter.as_ref().is_none_or(|(_, f)| *f != format) {
            self.painter = Some((paint::Painter::new(device, format), format));
        }
        if let Some((painter, _)) = &mut self.painter {
            painter.paint(
                device,
                queue,
                enc,
                view,
                size_px,
                out.pixels_per_point,
                &out.textures_delta,
                &prims,
            );
        }
    }
}
