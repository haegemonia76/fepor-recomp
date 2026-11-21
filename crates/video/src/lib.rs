//! wgpu backend for the GX front end: draws into an EFB-sized render target, handles EFB copies
//! (to the display and to textures) and can dump presented frames as PNG.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use fepor_gx::state::{CopyEfb, DrawCall, Event, Primitive, RenderState, Sink, TextureDesc, Tluts};
use fepor_gx::{tev, texture};
use wgpu::util::DeviceExt;

mod present;

pub const EFB_WIDTH: u32 = 640;
pub const EFB_HEIGHT: u32 = 528;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    pos: [f32; 4],
    col: [[f32; 4]; 2],
    uv: [[f32; 4]; 8],
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PipelineKey {
    shader: u64,
    topology: u8,
    blend: u32,
    z: u32,
    cull: u8,
    mask: u8,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TexKey {
    desc: TextureDesc,
    sample: u64,
}

struct EfbTexture {
    addr: u32,
    width: u32,
    height: u32,
    view: Rc<wgpu::TextureView>,
}

/// Which presented frames to write as PNG (`FEPOR_DUMP_FRAMES=dir`, `FEPOR_DUMP_EVERY=n`).
struct Dump {
    dir: PathBuf,
    every: u64,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    efb: wgpu::Texture,
    efb_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    shaders: HashMap<u64, wgpu::ShaderModule>,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    samplers: HashMap<(u8, u8, bool, bool), wgpu::Sampler>,
    textures: HashMap<TexKey, Rc<wgpu::TextureView>>,
    efb_textures: Vec<EfbTexture>,
    white: Rc<wgpu::TextureView>,
    encoder: Option<wgpu::CommandEncoder>,
    pending_clear: Option<([f64; 4], f32)>,
    frame: u64,
    dump: Option<Dump>,
    cull: bool,
    /// `FEPOR_DUMP_TEXTURES=dir`: write every newly decoded texture as PNG.
    dump_textures: Option<PathBuf>,
    trace: bool,
    trace_draws: bool,
    /// Draws since the last presented frame.
    pub draws: u64,
    presenter: Option<present::Presenter>,
}

fn write_png(path: &std::path::Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) {
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(data))
        .expect("write png");
}

fn hash_str(s: &str) -> u64 {
    // FNV-1a
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

impl Renderer {
    /// Create a renderer. With a window it presents every XFB copy there; without one it only renders
    /// (frames can still be dumped with `FEPOR_DUMP_FRAMES`).
    pub fn new(window: Option<Arc<winit::window::Window>>) -> Result<Renderer, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = match &window {
            Some(w) => Some(
                instance
                    .create_surface(w.clone())
                    .map_err(|e| format!("surface: {e}"))?,
            ),
            None => None,
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface.as_ref(),
            ..Default::default()
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("fepor"),
            ..Default::default()
        }))
        .map_err(|e| format!("device: {e}"))?;
        let presenter = surface.map(|s| {
            present::Presenter::new(
                &adapter,
                &device,
                s,
                window.clone().expect("surface implies window"),
            )
        });

        let efb = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("efb"),
            size: wgpu::Extent3d {
                width: EFB_WIDTH,
                height: EFB_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("efb depth"),
            size: wgpu::Extent3d {
                width: EFB_WIDTH,
                height: EFB_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let efb_view = efb.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tev uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mut entries = Vec::new();
        for i in 0..8 {
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: i,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 8 + i,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tev textures"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gx"),
            bind_group_layouts: &[&uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });

        let white_tex = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("white"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &[255, 255, 255, 255],
        );
        let white = Rc::new(white_tex.create_view(&Default::default()));

        let dump = std::env::var("FEPOR_DUMP_FRAMES").ok().map(|d| Dump {
            dir: PathBuf::from(d),
            every: std::env::var("FEPOR_DUMP_EVERY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
        });
        if let Some(d) = &dump {
            let _ = std::fs::create_dir_all(&d.dir);
        }
        Ok(Renderer {
            device,
            queue,
            efb,
            efb_view,
            depth_view,
            uniform_layout,
            texture_layout,
            pipeline_layout,
            shaders: HashMap::new(),
            pipelines: HashMap::new(),
            samplers: HashMap::new(),
            textures: HashMap::new(),
            efb_textures: Vec::new(),
            white,
            encoder: None,
            pending_clear: Some(([0.0; 4], 1.0)),
            frame: 0,
            dump,
            presenter,
            cull: std::env::var("FEPOR_CULL").is_ok(),
            dump_textures: std::env::var("FEPOR_DUMP_TEXTURES")
                .ok()
                .map(PathBuf::from)
                .inspect(|d| {
                    let _ = std::fs::create_dir_all(d);
                }),
            trace_draws: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "gxdraw")),
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "gx" || x == "all")),
            draws: 0,
        })
    }

    fn flush(&mut self) {
        if let Some(e) = self.encoder.take() {
            self.queue.submit([e.finish()]);
        }
    }

    fn sampler(&mut self, d: &TextureDesc) -> wgpu::Sampler {
        let key = (d.wrap_s, d.wrap_t, d.mag_linear, d.min_linear);
        let device = &self.device;
        self.samplers
            .entry(key)
            .or_insert_with(|| {
                let wrap = |w: u8| match w {
                    1 => wgpu::AddressMode::Repeat,
                    2 => wgpu::AddressMode::MirrorRepeat,
                    _ => wgpu::AddressMode::ClampToEdge,
                };
                let f = |l: bool| {
                    if l {
                        wgpu::FilterMode::Linear
                    } else {
                        wgpu::FilterMode::Nearest
                    }
                };
                device.create_sampler(&wgpu::SamplerDescriptor {
                    address_mode_u: wrap(d.wrap_s),
                    address_mode_v: wrap(d.wrap_t),
                    mag_filter: f(d.mag_linear),
                    min_filter: f(d.min_linear),
                    ..Default::default()
                })
            })
            .clone()
    }

    fn texture(&mut self, d: &TextureDesc, ram: &[u8], tluts: &Tluts) -> Rc<wgpu::TextureView> {
        if d.width == 0 || d.height == 0 || d.width > 1024 || d.height > 1024 {
            return self.white.clone();
        }
        if let Some(t) = self
            .efb_textures
            .iter()
            .find(|t| t.addr == d.addr && t.width == d.width && t.height == d.height)
        {
            return t.view.clone();
        }
        let size = texture::image_size(d.format, d.width, d.height);
        let a = d.addr as usize & 0x01FF_FFFF;
        let Some(data) = ram.get(a..a + size) else {
            return self.white.clone();
        };
        // cheap content fingerprint: size plus sampled bytes
        let mut sample = size as u64;
        for i in (0..size).step_by((size / 64).max(1)) {
            sample = sample.wrapping_mul(0x100_0000_01b3) ^ data[i] as u64;
        }
        let key = TexKey { desc: *d, sample };
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        let tlut = tluts
            .data
            .get(&d.tlut_offset)
            .or_else(|| {
                tluts
                    .data
                    .iter()
                    .filter(|(k, _)| **k <= d.tlut_offset)
                    .max_by_key(|(k, _)| **k)
                    .map(|(_, v)| v)
            })
            .or_else(|| tluts.data.values().next())
            .map(|v| (v.as_slice(), d.tlut_format));
        let rgba = texture::decode(d.format, d.width, d.height, data, tlut);
        if let Some(dir) = &self.dump_textures {
            let path = dir.join(format!(
                "tex_{:08x}_{}x{}_f{}_{:016x}.png",
                d.addr, d.width, d.height, d.format, sample
            ));
            write_png(&path, d.width, d.height, png::ColorType::Rgba, &rgba);
        }
        let tex = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("gx texture"),
                size: wgpu::Extent3d {
                    width: d.width,
                    height: d.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &rgba,
        );
        let view = Rc::new(tex.create_view(&Default::default()));
        if self.textures.len() > 2048 {
            self.textures.clear();
        }
        self.textures.insert(key, view.clone());
        view
    }

    fn pipeline(
        &mut self,
        state: &RenderState,
        shader_hash: u64,
        src: &str,
        topology: wgpu::PrimitiveTopology,
    ) -> wgpu::RenderPipeline {
        let key = PipelineKey {
            shader: shader_hash,
            topology: topology as u8,
            blend: state.blend_mode & 0xFFFF,
            z: state.z_mode,
            cull: if self.cull { state.cull } else { 0 },
            mask: state.color_update as u8 | (state.alpha_update as u8) << 1,
        };
        if let Some(p) = self.pipelines.get(&key) {
            return p.clone();
        }
        let device = &self.device;
        let module = self.shaders.entry(shader_hash).or_insert_with(|| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("tev"),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        });

        let factor_src = |f: u32| match f & 7 {
            0 => wgpu::BlendFactor::Zero,
            1 => wgpu::BlendFactor::One,
            2 => wgpu::BlendFactor::Dst,
            3 => wgpu::BlendFactor::OneMinusDst,
            4 => wgpu::BlendFactor::SrcAlpha,
            5 => wgpu::BlendFactor::OneMinusSrcAlpha,
            6 => wgpu::BlendFactor::DstAlpha,
            _ => wgpu::BlendFactor::OneMinusDstAlpha,
        };
        let factor_dst = |f: u32| match f & 7 {
            0 => wgpu::BlendFactor::Zero,
            1 => wgpu::BlendFactor::One,
            2 => wgpu::BlendFactor::Src,
            3 => wgpu::BlendFactor::OneMinusSrc,
            4 => wgpu::BlendFactor::SrcAlpha,
            5 => wgpu::BlendFactor::OneMinusSrcAlpha,
            6 => wgpu::BlendFactor::DstAlpha,
            _ => wgpu::BlendFactor::OneMinusDstAlpha,
        };
        let bm = state.blend_mode;
        let blend = (bm & 1 != 0).then(|| {
            let comp = wgpu::BlendComponent {
                src_factor: factor_src(bm >> 8),
                dst_factor: factor_dst(bm >> 5),
                operation: if bm >> 11 & 1 != 0 {
                    wgpu::BlendOperation::ReverseSubtract
                } else {
                    wgpu::BlendOperation::Add
                },
            };
            wgpu::BlendState {
                color: comp,
                alpha: comp,
            }
        });
        let mut mask = wgpu::ColorWrites::empty();
        if state.color_update {
            mask |= wgpu::ColorWrites::RED | wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE;
        }
        if state.alpha_update {
            mask |= wgpu::ColorWrites::ALPHA;
        }
        let zm = state.z_mode;
        let compare = if zm & 1 != 0 {
            match zm >> 1 & 7 {
                0 => wgpu::CompareFunction::Never,
                1 => wgpu::CompareFunction::Less,
                2 => wgpu::CompareFunction::Equal,
                3 => wgpu::CompareFunction::LessEqual,
                4 => wgpu::CompareFunction::Greater,
                5 => wgpu::CompareFunction::NotEqual,
                6 => wgpu::CompareFunction::GreaterEqual,
                _ => wgpu::CompareFunction::Always,
            }
        } else {
            wgpu::CompareFunction::Always
        };
        let cull_mode = match key.cull {
            1 => Some(wgpu::Face::Back),
            2 => Some(wgpu::Face::Front),
            _ => None,
        };
        let attrs: Vec<wgpu::VertexAttribute> = (0..11)
            .map(|i| wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16 * i as u64,
                shader_location: i,
            })
            .collect();
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("gx draw"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GpuVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attrs,
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend,
                        write_mask: mask,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode,
                    front_face: wgpu::FrontFace::Ccw,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: zm >> 4 & 1 != 0,
                    depth_compare: compare,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            });
        self.pipelines.insert(key, pipeline.clone());
        pipeline
    }

    fn draw(&mut self, d: DrawCall, ram: &[u8], tluts: &Tluts) {
        let n = d.vertices.len();
        let (indices, topology) = match d.prim {
            Primitive::Triangles => (
                (0..n as u32 / 3 * 3).collect::<Vec<_>>(),
                wgpu::PrimitiveTopology::TriangleList,
            ),
            Primitive::Quads => (
                (0..n as u32 / 4)
                    .flat_map(|q| [4 * q, 4 * q + 1, 4 * q + 2, 4 * q, 4 * q + 2, 4 * q + 3])
                    .collect(),
                wgpu::PrimitiveTopology::TriangleList,
            ),
            Primitive::TriangleStrip => (
                (2..n as u32)
                    .flat_map(|i| {
                        if i % 2 == 0 {
                            [i - 2, i - 1, i]
                        } else {
                            [i - 1, i - 2, i]
                        }
                    })
                    .collect(),
                wgpu::PrimitiveTopology::TriangleList,
            ),
            Primitive::TriangleFan => (
                (2..n as u32).flat_map(|i| [0, i - 1, i]).collect(),
                wgpu::PrimitiveTopology::TriangleList,
            ),
            Primitive::Lines => (
                (0..n as u32 / 2 * 2).collect(),
                wgpu::PrimitiveTopology::LineList,
            ),
            Primitive::LineStrip => (
                (1..n as u32).flat_map(|i| [i - 1, i]).collect(),
                wgpu::PrimitiveTopology::LineList,
            ),
            Primitive::Points => ((0..n as u32).collect(), wgpu::PrimitiveTopology::PointList),
        };
        if indices.is_empty() {
            return;
        }
        self.draws += 1;
        if self.trace_draws {
            let v = &d.vertices;
            eprintln!(
                "gx: draw {:?} {} verts, tev {} stages, texgens {}, colors {}, vp {:?}, sc {:?}, z {:#x}, blend {:#x}, cupd {} aupd {}\n    v0 pos {:?} col {:?} uv {:?}\n    v1 pos {:?}",
                d.prim, n, d.state.num_tev_stages, d.state.num_tex_gens, d.state.num_colors, d.state.viewport, d.state.scissor, d.state.z_mode, d.state.blend_mode, d.state.color_update, d.state.alpha_update,
                v[0].pos, v[0].col[0], v[0].uv[0], v.get(1).map(|x| x.pos)
            );
            eprintln!(
                "    stage0 {:?}\n    tex0 {:?}",
                d.state.tev[0], d.state.textures[0]
            );
        }
        let verts: Vec<GpuVertex> = d
            .vertices
            .iter()
            .map(|v| GpuVertex {
                pos: v.pos,
                col: v.col,
                uv: std::array::from_fn(|i| [v.uv[i][0], v.uv[i][1], v.uv[i][2], 0.0]),
            })
            .collect();
        let state = d.state;
        let src = tev::wgsl(&state);
        let shader_hash = hash_str(&src);
        let pipeline = self.pipeline(&state, shader_hash, &src, topology);

        let vbuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let ibuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let ubuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&tev::uniforms(&state)),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let ubind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            }],
        });

        // textures: only the maps a stage reads are fetched
        let mut used = [false; 8];
        for s in &state.tev[..state.num_tev_stages as usize] {
            if s.tex_enable {
                used[s.texmap as usize & 7] = true;
            }
        }
        let views: Vec<Rc<wgpu::TextureView>> = (0..8)
            .map(|m| {
                if used[m] {
                    self.texture(&state.textures[m], ram, tluts)
                } else {
                    self.white.clone()
                }
            })
            .collect();
        let samplers: Vec<wgpu::Sampler> =
            (0..8).map(|m| self.sampler(&state.textures[m])).collect();
        let mut entries = Vec::new();
        for m in 0..8 {
            entries.push(wgpu::BindGroupEntry {
                binding: m as u32,
                resource: wgpu::BindingResource::TextureView(&views[m]),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 8 + m as u32,
                resource: wgpu::BindingResource::Sampler(&samplers[m]),
            });
        }
        let tbind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.texture_layout,
            entries: &entries,
        });

        let clear = self.pending_clear.take();
        let (efb_view, depth_view) = (&self.efb_view, &self.depth_view);
        let enc = self
            .encoder
            .get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gx"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: efb_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: match clear {
                        Some((c, _)) => wgpu::LoadOp::Clear(wgpu::Color {
                            r: c[0],
                            g: c[1],
                            b: c[2],
                            a: c[3],
                        }),
                        None => wgpu::LoadOp::Load,
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: match clear {
                        Some((_, z)) => wgpu::LoadOp::Clear(z),
                        None => wgpu::LoadOp::Load,
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let [vx, vy, vw, vh, _, _] = state.viewport;
        let (x0, y0) = (vx.max(0.0), vy.max(0.0));
        let (x1, y1) = (
            (vx + vw).min(EFB_WIDTH as f32),
            (vy + vh).min(EFB_HEIGHT as f32),
        );
        if x1 > x0 && y1 > y0 {
            pass.set_viewport(x0, y0, x1 - x0, y1 - y0, 0.0, 1.0);
        }
        let [sx, sy, sw, sh] = state.scissor;
        let (sx0, sy0) = (
            sx.clamp(0, EFB_WIDTH as i32),
            sy.clamp(0, EFB_HEIGHT as i32),
        );
        let (sx1, sy1) = (
            (sx + sw).clamp(0, EFB_WIDTH as i32),
            (sy + sh).clamp(0, EFB_HEIGHT as i32),
        );
        if sx1 > sx0 && sy1 > sy0 {
            pass.set_scissor_rect(
                sx0 as u32,
                sy0 as u32,
                (sx1 - sx0) as u32,
                (sy1 - sy0) as u32,
            );
        }
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &ubind, &[]);
        pass.set_bind_group(1, &tbind, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
    }

    fn clear_pass(&mut self, color: [f64; 4], z: f32, color_update: bool, z_update: bool) {
        let efb_view = &self.efb_view;
        let depth_view = &self.depth_view;
        let enc = self
            .encoder
            .get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
        let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: efb_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if color_update {
                        wgpu::LoadOp::Clear(wgpu::Color {
                            r: color[0],
                            g: color[1],
                            b: color[2],
                            a: color[3],
                        })
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: if z_update {
                        wgpu::LoadOp::Clear(z)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }

    /// Read back a region of the EFB as RGBA8.
    pub fn read_efb(&mut self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        self.flush();
        let (w, h) = (
            w.min(EFB_WIDTH - x.min(EFB_WIDTH)),
            h.min(EFB_HEIGHT - y.min(EFB_HEIGHT)),
        );
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.efb,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for r in 0..h {
            out.extend_from_slice(&data[(r * row) as usize..(r * row + w * 4) as usize]);
        }
        out
    }

    fn present(&mut self, c: &CopyEfb) {
        self.frame += 1;
        if self.trace {
            eprintln!(
                "gx: frame {} ({}x{} at {},{}): {} draws",
                self.frame, c.width, c.height, c.x, c.y, self.draws
            );
        }
        self.draws = 0;
        if let Some(p) = &mut self.presenter {
            p.show(&self.device, &self.queue, &self.efb_view, c);
        }
        let Some(d) = &self.dump else { return };
        if self.frame % d.every != 0 {
            return;
        }
        let path = d.dir.join(format!("frame_{:05}.png", self.frame));
        let (w, h) = (c.width.min(EFB_WIDTH), c.height.min(EFB_HEIGHT));
        let rgba = self.read_efb(c.x, c.y, w, h);
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        write_png(&path, w, h, png::ColorType::Rgb, &rgb);
    }

    fn copy(&mut self, c: CopyEfb) {
        if c.to_xfb {
            self.flush();
            self.present(&c);
        } else {
            // copy to texture: keep the pixels on the GPU, keyed by the destination address
            let (w, h) = (
                (c.width.min(EFB_WIDTH)).max(1),
                c.height.min(EFB_HEIGHT).max(1),
            );
            let (x, y) = (c.x.min(EFB_WIDTH - 1), c.y.min(EFB_HEIGHT - 1));
            let (w, h) = (w.min(EFB_WIDTH - x), h.min(EFB_HEIGHT - y));
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("efb copy"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let efb = &self.efb;
            let enc = self
                .encoder
                .get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: efb,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            let view = Rc::new(tex.create_view(&Default::default()));
            self.efb_textures.retain(|t| t.addr != c.dest);
            let (tw, th) = if c.half_scale { (w / 2, h / 2) } else { (w, h) };
            self.efb_textures.push(EfbTexture {
                addr: c.dest,
                width: tw,
                height: th,
                view,
            });
            if self.efb_textures.len() > 64 {
                self.efb_textures.remove(0);
            }
        }
        if c.clear {
            let cc = c.clear_color;
            self.clear_pass(
                [
                    cc[0] as f64 / 255.0,
                    cc[1] as f64 / 255.0,
                    cc[2] as f64 / 255.0,
                    cc[3] as f64 / 255.0,
                ],
                c.clear_z as f32 / 16_777_215.0,
                c.color_update || c.alpha_update,
                c.z_update,
            );
        }
    }
}

impl Sink for Renderer {
    fn event(&mut self, event: Event, ram: &[u8], tluts: &Tluts) {
        match event {
            Event::Draw(d) => self.draw(d, ram, tluts),
            Event::Copy(c) => self.copy(c),
            Event::Finish => self.flush(),
            Event::Token(_) => {}
        }
    }
}
