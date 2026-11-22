//! Presents XFB copies in a window: the copied EFB region is stretched into the surface,
//! letterboxed to the frame's aspect ratio.

use std::sync::Arc;

use fepor_core::settings::Aspect;
use fepor_gx::state::CopyEfb;
use wgpu::util::DeviceExt;

use crate::{EFB_HEIGHT, EFB_WIDTH};

const SHADER: &str = r#"
struct U { rect: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var t: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct VO { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    var corners = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    let xy = corners[i];
    var o: VO;
    o.p = vec4<f32>(xy, 0.0, 1.0);
    o.uv = mix(u.rect.xy, u.rect.zw, vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5));
    return o;
}
@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(t, s, i.uv).rgb, 1.0);
}
"#;

pub struct Presenter {
    surface: wgpu::Surface<'static>,
    window: Arc<winit::window::Window>,
    config: wgpu::SurfaceConfiguration,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    aspect: Aspect,
    adapter: wgpu::Adapter,
}

impl Presenter {
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        surface: wgpu::Surface<'static>,
        window: Arc<winit::window::Window>,
        low_latency: bool,
    ) -> Presenter {
        let caps = surface.get_capabilities(adapter);
        // the EFB holds gamma-space values: write them as they are
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: if low_latency {
                // images are paced by the renderer: the present call must not wait for the display
                [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
                    .into_iter()
                    .find(|m| caps.present_modes.contains(m))
                    .unwrap_or(wgpu::PresentMode::Fifo)
            } else {
                wgpu::PresentMode::Fifo
            },
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(device, &config);

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("present"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("present"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("present"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Presenter {
            surface,
            window,
            config,
            layout,
            pipeline,
            sampler,
            aspect: Aspect::Fit,
            adapter: adapter.clone(),
        }
    }

    /// Draw the copied region into the window and present it.
    pub fn show(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        efb: &wgpu::TextureView,
        c: &CopyEfb,
        overlay: Option<&mut fepor_overlay::Overlay>,
    ) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if (size.width, size.height) != (self.config.width, self.config.height) {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(device, &self.config);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(device, &self.config);
                return;
            }
            Err(_) => return,
        };
        let (w, h) = (c.width.clamp(1, EFB_WIDTH), c.height.clamp(1, EFB_HEIGHT));
        let (x, y) = (c.x.min(EFB_WIDTH - 1), c.y.min(EFB_HEIGHT - 1));
        let rect = [
            x as f32 / EFB_WIDTH as f32,
            y as f32 / EFB_HEIGHT as f32,
            (x + w).min(EFB_WIDTH) as f32 / EFB_WIDTH as f32,
            (y + h).min(EFB_HEIGHT) as f32 / EFB_HEIGHT as f32,
        ];
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&rect),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(efb),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        // letterbox to the chosen aspect ratio (default: the frame's own)
        let (sw, sh) = (size.width as f32, size.height as f32);
        let aspect = match self.aspect {
            Aspect::Fit => w as f32 / h as f32,
            Aspect::Wide16x9 => 16.0 / 9.0,
            Aspect::Wide21x9 => 21.0 / 9.0,
            Aspect::Stretch => sw / sh,
        };
        let (vw, vh) = if sw / sh > aspect {
            (sh * aspect, sh)
        } else {
            (sw, sw / aspect)
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_viewport((sw - vw) / 2.0, (sh - vh) / 2.0, vw, vh, 0.0, 1.0);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        if let Some(o) = overlay {
            o.paint(
                device,
                queue,
                &mut enc,
                &view,
                self.config.format,
                [size.width, size.height],
                self.window.scale_factor() as f32,
            );
        }
        queue.submit([enc.finish()]);
        self.window.pre_present_notify();
        frame.present();
    }

    pub fn set_aspect(&mut self, aspect: Aspect) {
        self.aspect = aspect;
    }

    pub fn set_fullscreen(&self, on: bool) {
        let want = on.then_some(winit::window::Fullscreen::Borderless(None));
        if self.window.fullscreen() != want {
            self.window.set_fullscreen(want);
        }
    }

    /// Images are paced by the renderer when it interpolates: then presenting must not wait for the display.
    pub fn set_low_latency(&mut self, device: &wgpu::Device, low_latency: bool) {
        let caps = self.surface.get_capabilities(&self.adapter);
        self.config.present_mode = if low_latency {
            [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
                .into_iter()
                .find(|m| caps.present_modes.contains(m))
                .unwrap_or(wgpu::PresentMode::Fifo)
        } else {
            wgpu::PresentMode::Fifo
        };
        self.surface.configure(device, &self.config);
    }
}
