//! wgpu backend for the GX front end: draws into an EFB-sized render target, handles EFB copies
//! (to the display and to textures) and can dump presented frames as PNG.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fepor_core::settings::SettingsHandle;
use fepor_gx::state::{CopyEfb, DrawCall, Event, Primitive, RenderState, Sink, TextureDesc, Tluts};
use fepor_gx::{tev, texture};
use wgpu::util::DeviceExt;

mod icons;
mod present;
mod sysstat;

pub const EFB_WIDTH: u32 = 640;
pub const EFB_HEIGHT: u32 = 528;

/// A multiply-rotate hasher (the maps here are keyed by small plain values hashed several times per draw;
/// SipHash was 4 % of the guest thread in a battle).
#[derive(Default, Clone, Copy)]
struct Fx(u64);

impl Fx {
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut v = 0u64;
            for (i, b) in c.iter().enumerate() {
                v |= (*b as u64) << (8 * i);
            }
            self.add(v);
        }
    }
    fn write_u8(&mut self, v: u8) {
        self.add(v as u64);
    }
    fn write_u32(&mut self, v: u32) {
        self.add(v as u64);
    }
    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }
    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
    }
}

type FastMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;

/// The state recorded into the open render pass, to skip repeating it for every draw.
#[derive(Default)]
struct PassState {
    viewport: Option<[f32; 4]>,
    scissor: Option<[u32; 4]>,
    pipeline: Option<wgpu::RenderPipeline>,
    tbind: Option<wgpu::BindGroup>,
    buffers_bound: bool,
    uoff: Option<usize>,
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
    /// Hash of every byte of the texture (a sampled hash let changes between the samples through).
    content: u64,
    /// Hash of the palette of an indexed texture.
    palette: u64,
}

struct EfbTexture {
    addr: u32,
    width: u32,
    height: u32,
    view: Rc<wgpu::TextureView>,
    /// How many bytes of main memory at `addr` the copy stands for, and their fingerprint when it was made.
    ram_len: usize,
    ram_fp: u64,
}

/// Which presented frames to write as PNG (`FEPOR_DUMP_FRAMES=dir`, `FEPOR_DUMP_EVERY=n`).
struct Dump {
    dir: PathBuf,
    every: u64,
}

const VBUF_SIZE: usize = 16 << 20;
const IBUF_SIZE: usize = 8 << 20;
const UBUF_SIZE: usize = 4 << 20;
const UNIFORM_BYTES: usize = tev::UNIFORM_FLOATS * 4;
/// Dynamic uniform offsets must be 256-aligned.
const UNIFORM_STRIDE: usize = UNIFORM_BYTES.div_ceil(256) * 256;

/// Texture bind group identity: the view pointers and the sampler state of the eight maps.
type TexBindKey = ([usize; 8], [u8; 8]);

/// Per-submit draw data: filled on the CPU while draws are recorded, uploaded once before the submit.
struct Stream {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    ubuf: wgpu::Buffer,
    ubind: wgpu::BindGroup,
    vdata: Vec<u8>,
    idata: Vec<u8>,
    udata: Vec<u8>,
    /// Offset of the uniforms recorded last, for reuse.
    last_u: Option<usize>,
}

impl Stream {
    fn new(device: &wgpu::Device, uniform_layout: &wgpu::BindGroupLayout) -> Stream {
        let buf = |size: usize, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("gx stream"),
                size: size as u64,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let ubuf = buf(UBUF_SIZE, wgpu::BufferUsages::UNIFORM);
        let ubind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tev uniforms"),
            layout: uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &ubuf,
                    offset: 0,
                    size: std::num::NonZeroU64::new(UNIFORM_BYTES as u64),
                }),
            }],
        });
        Stream {
            vbuf: buf(VBUF_SIZE, wgpu::BufferUsages::VERTEX),
            ibuf: buf(IBUF_SIZE, wgpu::BufferUsages::INDEX),
            ubuf,
            ubind,
            vdata: Vec::new(),
            idata: Vec::new(),
            udata: Vec::new(),
            last_u: None,
        }
    }
}

/// One draw as it was submitted, with everything needed to submit it again with other vertices.
struct RecDraw {
    /// Vertex stream (see `tev::vertex_stride`).
    verts: Vec<u8>,
    stride: usize,
    indices: Vec<u32>,
    uniforms: [f32; tev::UNIFORM_FLOATS],
    pipeline: wgpu::RenderPipeline,
    tbind: wgpu::BindGroup,
    viewport: [f32; 6],
    scissor: [i32; 4],
    /// Same shader, vertex/index counts and textures: the same object in the next frame.
    sig: u64,
}

/// A pipeline that draws one full-target triangle writing only the alpha channel, value 1.
fn make_alpha_one(device: &wgpu::Device) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("alpha one"),
        source: wgpu::ShaderSource::Wgsl(
            "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                 var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
                 return vec4<f32>(p[i], 0.0, 1.0);
             }
             @fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }"
                .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("alpha one"),
        layout: None,
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
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALPHA,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// What a draw derived from its render state, for the next draw to reuse while the state is the same one.
struct DrawCache {
    state: Arc<fepor_gx::state::RenderState>,
    topology: wgpu::PrimitiveTopology,
    shader_hash: u64,
    pipeline: wgpu::RenderPipeline,
    uniforms: [f32; tev::UNIFORM_FLOATS],
    used: [bool; 8],
}

enum Rec {
    Clear {
        color: [f64; 4],
        z: f32,
        color_update: bool,
        z_update: bool,
    },
    Draw(RecDraw),
}

/// The images still to be shown for one guest frame: `mult - 1` interpolated ones between `prev` and
/// `cur`, then the real frame.
struct Plan {
    prev: Rc<Vec<Rec>>,
    cur: Rc<Vec<Rec>>,
    copy: CopyEfb,
    start: Instant,
    period: Duration,
    /// The next image, 1-based.
    j: u32,
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x100_0000_01b3)
}

/// `FEPOR_TRACE=interp`: report draws the interpolation moves in a suspicious way.
fn trace_interp() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "interp"))
    })
}

/// Vertices of `cur` moved `t` of the way from where `prev` had them. Anything that jumps across the
/// screen (a cut, a teleport) or has no counterpart is left as it is in `cur`.
fn lerp_verts(prev: Option<&RecDraw>, cur: &RecDraw, t: f32) -> Vec<u8> {
    // same object: same shader/textures/counts and the very same texture coordinates (glyphs of a text
    // that grows, or sprites that swap frames, are not the same object even when the sig matches)
    let s = cur.stride;
    let Some(p) = prev.filter(|p| {
        p.sig == cur.sig
            && p.stride == s
            && p.verts.len() == cur.verts.len()
            && p.verts
                .chunks_exact(s)
                .zip(cur.verts.chunks_exact(s))
                .all(|(a, b)| a[24..] == b[24..])
    }) else {
        return cur.verts.clone();
    };
    let pos = |v: &[u8]| -> [f32; 4] {
        std::array::from_fn(|k| {
            f32::from_le_bytes([v[4 * k], v[4 * k + 1], v[4 * k + 2], v[4 * k + 3]])
        })
    };
    if trace_interp() {
        // a small draw (a sprite) should move as one piece
        let d: Vec<[f32; 2]> = p
            .verts
            .chunks_exact(s)
            .zip(cur.verts.chunks_exact(s))
            .filter_map(|(a, b)| {
                let (a, b) = (pos(a), pos(b));
                (a[3] > 0.0 && b[3] > 0.0)
                    .then(|| [b[0] / b[3] - a[0] / a[3], b[1] / b[3] - a[1] / a[3]])
            })
            .collect();
        if d.len() == cur.verts.len() / s
            && d.len() <= 6
            && d.iter()
                .any(|x| (x[0] - d[0][0]).abs() > 0.01 || (x[1] - d[0][1]).abs() > 0.01)
        {
            eprintln!(
                "interp: non-rigid {} verts, sig {:x}, NDC moves {:?}",
                d.len(),
                cur.sig,
                d
            );
        }
    }
    let mut out = cur.verts.clone();
    for (v, (a, b)) in out
        .chunks_exact_mut(s)
        .zip(p.verts.chunks_exact(s).zip(cur.verts.chunks_exact(s)))
    {
        let (a, b) = (pos(a), pos(b));
        if a[3] <= 0.0 || b[3] <= 0.0 {
            return cur.verts.clone();
        }
        if (a[0] / a[3] - b[0] / b[3]).abs() > MAX_MOVE
            || (a[1] / a[3] - b[1] / b[3]).abs() > MAX_MOVE
        {
            return cur.verts.clone();
        }
        for k in 0..4 {
            v[4 * k..4 * k + 4].copy_from_slice(&(a[k] + (b[k] - a[k]) * t).to_le_bytes());
        }
    }
    out
}

/// Largest per-frame movement (in NDC units; the screen is 2 wide) still treated as motion.
const MAX_MOVE: f32 = 0.4;

/// The render targets, all `scale` times the GameCube's EFB.
struct Targets {
    efb: wgpu::Texture,
    efb_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// A second colour/depth target for interpolated frames, and a copy of the last real frame.
    alt: wgpu::Texture,
    alt_view: wgpu::TextureView,
    alt_depth_view: wgpu::TextureView,
    xfb: wgpu::Texture,
    xfb_view: wgpu::TextureView,
}

fn make_targets(device: &wgpu::Device, scale: u32) -> Targets {
    let tex = |label: &'static str, format: wgpu::TextureFormat, usage: wgpu::TextureUsages| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: EFB_WIDTH * scale,
                height: EFB_HEIGHT * scale,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    use wgpu::TextureFormat::{Depth32Float, Rgba8Unorm};
    use wgpu::TextureUsages as U;
    let efb = tex(
        "efb",
        Rgba8Unorm,
        U::RENDER_ATTACHMENT | U::COPY_SRC | U::TEXTURE_BINDING,
    );
    let depth = tex("efb depth", Depth32Float, U::RENDER_ATTACHMENT);
    let alt = tex(
        "efb interp",
        Rgba8Unorm,
        U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC,
    );
    let alt_depth = tex("efb interp depth", Depth32Float, U::RENDER_ATTACHMENT);
    let xfb = tex("xfb", Rgba8Unorm, U::COPY_DST | U::TEXTURE_BINDING);
    Targets {
        efb_view: efb.create_view(&Default::default()),
        depth_view: depth.create_view(&Default::default()),
        alt_view: alt.create_view(&Default::default()),
        alt_depth_view: alt_depth.create_view(&Default::default()),
        xfb_view: xfb.create_view(&Default::default()),
        efb,
        alt,
        xfb,
    }
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    efb: wgpu::Texture,
    efb_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    texture_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    shaders: FastMap<u64, wgpu::ShaderModule>,
    /// Generated WGSL by `tev::shader_key`, with its hash: most draws repeat a shader seen before.
    shader_srcs: FastMap<u64, (u64, Rc<str>)>,
    pipelines: FastMap<PipelineKey, wgpu::RenderPipeline>,
    samplers: FastMap<(u8, u8, bool, bool), wgpu::Sampler>,
    textures: FastMap<TexKey, Rc<wgpu::TextureView>>,
    efb_textures: Vec<EfbTexture>,
    white: Rc<wgpu::TextureView>,
    encoder: Option<wgpu::CommandEncoder>,
    /// The open render pass: consecutive draws share it until something else needs the encoder.
    pass: Option<wgpu::RenderPass<'static>>,
    stream: Stream,
    tex_binds: FastMap<TexBindKey, (wgpu::BindGroup, Vec<Rc<wgpu::TextureView>>)>,
    /// What the open pass already has set (see `emit`).
    pstate: PassState,
    pending_clear: Option<([f64; 4], f32)>,
    /// Images shown per guest frame (1 = every frame as it comes).
    mult: u32,
    /// Internal resolution factor (`FEPOR_RES`): render targets are `scale` times the GameCube's EFB.
    scale: u32,
    /// Draws and clears go to the interpolation target (`alt_*`) while a frame is replayed.
    use_alt: bool,
    alt_view: wgpu::TextureView,
    alt: wgpu::Texture,
    /// `read_efb` reads the interpolation target instead (frame dumps).
    read_alt: bool,
    alt_depth_view: wgpu::TextureView,
    /// The last real frame, kept so it can be shown after the interpolated ones.
    xfb: wgpu::Texture,
    xfb_view: wgpu::TextureView,
    /// The frame being drawn, and the one before it (only recorded when `mult > 1`).
    rec: Vec<Rec>,
    prev: Option<Rc<Vec<Rec>>>,
    plan: Option<Plan>,
    last_present: Option<Instant>,
    stat_frames: u32,
    stat_images: u32,
    stat_since: Instant,
    busy_events: Duration,
    busy_interp: Duration,
    busy_since: Instant,
    period: Duration,
    frame: u64,
    dump: Option<Dump>,
    cull: bool,
    /// `FEPOR_DUMP_TEXTURES=dir`: write every newly decoded texture as PNG.
    dump_textures: Option<PathBuf>,
    trace: bool,
    trace_draws: bool,
    /// `FEPOR_TRACE=icons`: print where the known button icons are drawn.
    trace_icons: bool,
    /// `FEPOR_TRACE=gxhash`: print a hash of every presented frame (how often the picture really changes).
    trace_hash: bool,
    /// Draws since the last presented frame.
    pub draws: u64,
    presenter: Option<present::Presenter>,
    overlay: Option<fepor_overlay::Overlay>,
    settings: SettingsHandle,
    /// The `generation` of `settings` last applied.
    settings_gen: u64,
    /// Whether this renderer can show interpolated images (windowed, or dumping frames).
    interpolates: bool,
    /// The vsync setting last applied to the presenter.
    vsync: bool,
    /// Guest frames since `fps_since`, to measure the game's frame rate for the overlay.
    fps_frames: u32,
    fps_since: Instant,
    /// `FEPOR_TRACE=tex`: count the textures the old sampled key would have got wrong.
    trace_tex: bool,
    /// `FEPOR_TRACE=texnew`: one line per texture decoded, with the frame number.
    trace_texnew: bool,
    /// `FEPOR_TEXWATCH=<hex address>`: print every use of a texture at that address.
    tex_watch: Option<u32>,
    /// `FEPOR_TEXWATCH_PAL=1`: the same for every palette texture.
    tex_watch_pal: bool,
    seen_sampled: FastMap<(TextureDesc, u64), (u64, u64)>,
    stale_data: u64,
    stale_palette: u64,
    stale_efb: u64,
    /// Hash of the texture data at (address, size), valid until the end of the frame.
    hash_memo: FastMap<(usize, usize), u64>,
    /// Index and vertex bytes of the draw being built, kept between draws to avoid allocating for each.
    scratch_idx: Vec<u32>,
    /// The decoded-vertex vector of the last draw, handed back to the GX parser (`Sink::recycle`).
    spare_verts: Vec<fepor_gx::state::Vertex>,
    scratch_vb: Vec<u8>,
    draw_cache: Option<DrawCache>,
    /// Writes alpha 1 over a texture and touches no colour (see `copy`).
    alpha_one: wgpu::RenderPipeline,
    stats: StatsState,
    /// Whether the statistics overlay is shown (the settings' `show_stats`, applied each frame).
    show_stats: bool,
    /// The `icon_generation` the cached textures were made for.
    icon_gen: u64,
    /// Monotonic totals of the guest thread's time in draw/copy events and in interpolated images.
    tot_events: Duration,
    tot_interp: Duration,
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
    pub fn new(
        window: Option<Arc<winit::window::Window>>,
        overlay: Option<fepor_overlay::Overlay>,
        settings: SettingsHandle,
    ) -> Result<Renderer, String> {
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
        let adapter_info = adapter.get_info();
        // Images shown per guest frame (interpolated between consecutive frames), windowed or dumped only.
        let interpolates = window.is_some() || std::env::var("FEPOR_DUMP_FRAMES").is_ok();
        let mult: u32 = if interpolates {
            settings.get().video.frame_multiplier
        } else {
            1
        };
        let presenter = surface.map(|s| {
            present::Presenter::new(
                &adapter,
                &device,
                s,
                window.clone().expect("surface implies window"),
                mult > 1,
            )
        });

        let scale = settings.get().video.resolution;
        let Targets {
            efb,
            efb_view,
            depth_view,
            alt,
            alt_view,
            alt_depth_view,
            xfb,
            xfb_view,
        } = make_targets(&device, scale);

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tev uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: std::num::NonZeroU64::new(UNIFORM_BYTES as u64),
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
        let stream = Stream::new(&device, &uniform_layout);

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
        let alpha_one = make_alpha_one(&device);
        Ok(Renderer {
            alpha_one,
            device,
            queue,
            efb,
            efb_view,
            depth_view,
            texture_layout,
            pipeline_layout,
            shaders: FastMap::default(),
            shader_srcs: FastMap::default(),
            pipelines: FastMap::default(),
            samplers: FastMap::default(),
            textures: FastMap::default(),
            efb_textures: Vec::new(),
            white,
            encoder: None,
            pass: None,
            stream,
            tex_binds: FastMap::default(),
            pstate: PassState::default(),
            mult,
            scale,
            use_alt: false,
            alt_view,
            alt_depth_view,
            xfb,
            alt,
            read_alt: false,
            xfb_view,
            rec: Vec::new(),
            prev: None,
            plan: None,
            last_present: None,
            stat_frames: 0,
            stat_images: 0,
            stat_since: Instant::now(),
            busy_events: Duration::ZERO,
            busy_interp: Duration::ZERO,
            busy_since: Instant::now(),
            period: Duration::from_micros(16_683),
            pending_clear: Some(([0.0; 4], 1.0)),
            frame: 0,
            dump,
            presenter,
            overlay,
            settings,
            settings_gen: 0,
            interpolates,
            vsync: true,
            fps_frames: 0,
            fps_since: Instant::now(),
            stats: StatsState::new(&adapter_info),
            show_stats: false,
            icon_gen: 0,
            tot_events: Duration::ZERO,
            tot_interp: Duration::ZERO,
            cull: std::env::var("FEPOR_CULL").is_ok(),
            dump_textures: std::env::var("FEPOR_DUMP_TEXTURES")
                .ok()
                .map(PathBuf::from)
                .inspect(|d| {
                    let _ = std::fs::create_dir_all(d);
                }),
            trace_hash: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "gxhash")),
            trace_draws: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "gxdraw")),
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "gx" || x == "all")),
            trace_tex: std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "tex")),
            trace_texnew: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "texnew")),
            tex_watch: std::env::var("FEPOR_TEXWATCH")
                .ok()
                .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
            tex_watch_pal: std::env::var("FEPOR_TEXWATCH_PAL").is_ok(),
            seen_sampled: FastMap::default(),
            stale_data: 0,
            stale_palette: 0,
            stale_efb: 0,
            hash_memo: FastMap::default(),
            scratch_idx: Vec::new(),
            spare_verts: Vec::new(),
            scratch_vb: Vec::new(),
            trace_icons: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "icons")),
            draws: 0,
            draw_cache: None,
        })
    }

    fn end_pass(&mut self) {
        self.pass = None;
    }

    fn flush(&mut self) {
        self.end_pass();
        if let Some(e) = self.encoder.take() {
            let s = &mut self.stream;
            self.queue.write_buffer(&s.vbuf, 0, &s.vdata);
            self.queue.write_buffer(&s.ibuf, 0, &s.idata);
            self.queue.write_buffer(&s.ubuf, 0, &s.udata);
            s.vdata.clear();
            s.idata.clear();
            s.udata.clear();
            s.last_u = None;
            self.queue.submit([e.finish()]);
        }
        self.tex_binds.clear();
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
        let a = d.addr as usize & 0x01FF_FFFF;
        if self.tex_watch == Some(d.addr) {
            let copies: Vec<(u32, u32)> = self
                .efb_textures
                .iter()
                .filter(|t| t.addr == d.addr)
                .map(|t| (t.width, t.height))
                .collect();
            eprintln!(
                "texuse: frame {} {:08x} {}x{} fmt {} efb copies there {:?}",
                self.frame, d.addr, d.width, d.height, d.format, copies
            );
        }
        if let Some(i) = self
            .efb_textures
            .iter()
            .position(|t| t.addr == d.addr && t.width == d.width && t.height == d.height)
        {
            let t = &self.efb_textures[i];
            let now = ram
                .get(a..a + t.ram_len)
                .map_or(0, |data| Self::fingerprint(data, t.ram_len));
            if now == t.ram_fp {
                return t.view.clone();
            }
            // the game put a real texture where an EFB copy used to be
            if self.trace_tex {
                eprintln!(
                    "efbstale: frame {} addr {:08x} {}x{} len {} ram fp {:x} -> {now:x}",
                    self.frame, d.addr, t.width, t.height, t.ram_len, t.ram_fp
                );
            }
            self.efb_textures.remove(i);
            self.stale_efb += 1;
        }
        let size = texture::image_size(d.format, d.width, d.height);
        let Some(data) = ram.get(a..a + size) else {
            return self.white.clone();
        };
        // The palette of an indexed texture is part of what it looks like.
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
        // C4, C8 and C14X2 index a palette
        let palette = if (8..=10).contains(&d.format) {
            tlut.map_or(0, |(p, _)| Self::full_hash(p))
        } else {
            0
        };
        // hashing every byte at every draw is too slow (a background is a megabyte): once per frame
        let content = *self
            .hash_memo
            .entry((a, size))
            .or_insert_with(|| Self::full_hash(data));
        let key = TexKey {
            desc: *d,
            content,
            palette,
        };
        if self.tex_watch == Some(d.addr) || (self.tex_watch_pal && (8..=10).contains(&d.format)) {
            eprintln!(
                "texwatch: frame {} {:08x} {}x{} tlut {:x} content {:016x} palette {:016x} cached {}",
                self.frame,
                d.addr,
                d.width,
                d.height,
                d.tlut_offset,
                key.content,
                key.palette,
                self.textures.contains_key(&key)
            );
        }
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        // the sampled fingerprint names the texture in dumps and finds the button icons
        let sample = Self::fingerprint(data, size);
        if self.trace_texnew {
            let mut loaded: Vec<(u32, usize)> =
                tluts.data.iter().map(|(k, v)| (*k, v.len() / 2)).collect();
            loaded.sort();
            eprintln!(
                "texnew: frame {} addr {:08x} {}x{} fmt {} tlut {:x} (exact {}) content {:016x} palette {:016x} loaded {:x?}",
                self.frame,
                d.addr,
                d.width,
                d.height,
                d.format,
                d.tlut_offset,
                tluts.data.contains_key(&d.tlut_offset),
                key.content,
                key.palette,
                loaded
            );
        }
        if self.trace_tex {
            // what the cache used to key on: a hit there with another content was a wrong texture
            let old = (*d, sample);
            match self.seen_sampled.get(&old) {
                Some((content, pal)) if (*content, *pal) != (key.content, key.palette) => {
                    if *content != key.content {
                        self.stale_data += 1;
                    }
                    if *pal != key.palette {
                        self.stale_palette += 1;
                    }
                }
                _ => {}
            }
            self.seen_sampled.insert(old, (key.content, key.palette));
        }
        let mut rgba = texture::decode(d.format, d.width, d.height, data, tlut);
        // the dump keeps the game's own art: it is how new icons are found
        if let Some(dir) = &self.dump_textures {
            let path = dir.join(format!(
                "tex_{:08x}_{}x{}_f{}_{:016x}.png",
                d.addr, d.width, d.height, d.format, sample
            ));
            write_png(&path, d.width, d.height, png::ColorType::Rgba, &rgba);
        }
        // a replacement may have more texels than the game's texture (it is sampled by coordinate)
        let (mut tw, mut th) = (d.width, d.height);
        if let Some(icon) = icons::find(sample, d.width, d.height) {
            let replacement = self.overlay.as_ref().and_then(|o| {
                o.shared()
                    .button_icon(icon.action, d.width, d.height, icon.shape)
            });
            if let Some(img) = replacement {
                rgba = img;
            }
        } else if let Some(rep) =
            icons::find_label(sample, d.width, d.height).and_then(|l| l.replace.as_ref())
        {
            let (w, h) = (d.width * rep.texel_scale, d.height * rep.texel_scale);
            let replacement = self.overlay.as_ref().and_then(|o| {
                o.shared()
                    .button_icon(rep.action, w, h, fepor_overlay::glyphs::Shape::default())
            });
            if let Some(img) = replacement {
                (rgba, tw, th) = (img, w, h);
            }
        }
        let tex = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("gx texture"),
                size: wgpu::Extent3d {
                    width: tw,
                    height: th,
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
            // the bind groups keep their textures alive: drop them together, or the GPU memory only grows
            self.textures.clear();
            self.tex_binds.clear();
        }
        self.textures.insert(key, view.clone());
        view
    }

    /// Hash of all of `data`, eight bytes at a time.
    fn full_hash(data: &[u8]) -> u64 {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut h = data.len() as u64;
        let mut words = data.chunks_exact(8);
        for w in &mut words {
            let v = u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
            h = (h.rotate_left(5) ^ v).wrapping_mul(K);
        }
        for &b in words.remainder() {
            h = (h.rotate_left(5) ^ b as u64).wrapping_mul(K);
        }
        h
    }

    /// Cheap content fingerprint of texture data: size plus sampled bytes. It names textures in dumps and
    /// finds the button icons; the texture cache keys on `full_hash`.
    fn fingerprint(data: &[u8], size: usize) -> u64 {
        let mut sample = size as u64;
        for i in (0..size).step_by((size / 64).max(1)) {
            sample = sample.wrapping_mul(0x100_0000_01b3) ^ data[i] as u64;
        }
        sample
    }

    /// The known button icon or caption a texture is, if any.
    fn icon_of(&self, d: &TextureDesc, ram: &[u8]) -> Option<icons::Known> {
        if !icons::could_be(d.width, d.height) {
            return None;
        }
        let size = texture::image_size(d.format, d.width, d.height);
        let a = d.addr as usize & 0x01FF_FFFF;
        let data = ram.get(a..a + size)?;
        icons::find_any(Self::fingerprint(data, size), d.width, d.height)
    }

    /// How far (in NDC) a draw of a known face-button icon must move so that the icon sits where its
    /// physical button is on the chosen family's pad: the face buttons of Xbox, Nintendo and PlayStation pads
    /// form a diamond, the GameCube's do not.
    fn icon_shift(
        &self,
        state: &RenderState,
        used: &[bool; 8],
        ram: &[u8],
        vertices: &[fepor_gx::state::Vertex],
    ) -> Option<icons::Move> {
        if self.trace_icons {
            // find the labels of the hints: every textured draw near the bottom right corner
            if let Some(m) = (0..8).find(|m| used[*m]) {
                let d = &state.textures[m];
                let size = texture::image_size(d.format, d.width, d.height);
                let a = d.addr as usize & 0x01FF_FFFF;
                let q = icons::quad_px(vertices.iter().map(|v| v.pos), state.viewport);
                if let (Some(data), Some(q)) = (ram.get(a..a + size), q) {
                    if (400.0..800.0).contains(&q.cx) && (320.0..440.0).contains(&q.cy) {
                        eprintln!(
                            "icons: tex {}x{} f{} {:016x} centre ({:.1}, {:.1}) size {:.1}x{:.1}",
                            d.width,
                            d.height,
                            d.format,
                            Self::fingerprint(data, size),
                            q.cx,
                            q.cy,
                            q.w,
                            q.h
                        );
                    }
                }
            }
        }
        let known = (0..8)
            .filter(|m| used[*m])
            .find_map(|m| self.icon_of(&state.textures[m], ram))?;
        let quad = icons::quad_px(vertices.iter().map(|v| v.pos), state.viewport)?;
        let shared = self.overlay.as_ref()?.shared();
        let (dx, dy, scale) = match known {
            icons::Known::Icon(icon) => {
                if self.trace_icons {
                    eprintln!(
                        "icons: {:?} quad centre ({:.1}, {:.1}) size {:.1}x{:.1}",
                        icon.action, quad.cx, quad.cy, quad.w, quad.h
                    );
                }
                let slot = shared.button_slot(icon.action)?;
                let origin = icon.origin?;
                (slot.0 - origin.0, slot.1 - origin.1, 1.0)
            }
            icons::Known::Label(label) => {
                let (dx, dy) = icons::label_shift(&label.anchor, |a| shared.button_slot(a))?;
                (dx, dy, label.replace.as_ref().map_or(1.0, |r| r.grow))
            }
        };
        let (vp_x, vp_y, vw, vh) = (
            state.viewport[0],
            state.viewport[1],
            state.viewport[2],
            state.viewport[3],
        );
        Some(icons::Move {
            dx: dx * 2.0 / vw,
            dy: -dy * 2.0 / vh,
            scale,
            cx: (quad.cx - vp_x) / vw * 2.0 - 1.0,
            cy: 1.0 - (quad.cy - vp_y) / vh * 2.0,
        })
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
        let n_uv = tev::tex_inputs(state);
        let mut attrs: Vec<wgpu::VertexAttribute> = vec![
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 16,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 20,
                shader_location: 2,
            },
        ];
        for i in 0..n_uv {
            attrs.push(wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 24 + 12 * i as u64,
                shader_location: 3 + i as u32,
            });
        }
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
                        array_stride: tev::vertex_stride(n_uv) as u64,
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

    fn draw(&mut self, d: &DrawCall, ram: &[u8], tluts: &Tluts) {
        let n = d.vertices.len();
        let mut indices = std::mem::take(&mut self.scratch_idx);
        indices.clear();
        let topology = match d.prim {
            Primitive::Triangles => {
                indices.extend(0..n as u32 / 3 * 3);
                wgpu::PrimitiveTopology::TriangleList
            }
            Primitive::Quads => {
                indices.extend(
                    (0..n as u32 / 4)
                        .flat_map(|q| [4 * q, 4 * q + 1, 4 * q + 2, 4 * q, 4 * q + 2, 4 * q + 3]),
                );
                wgpu::PrimitiveTopology::TriangleList
            }
            Primitive::TriangleStrip => {
                indices.extend((2..n as u32).flat_map(|i| {
                    if i % 2 == 0 {
                        [i - 2, i - 1, i]
                    } else {
                        [i - 1, i - 2, i]
                    }
                }));
                wgpu::PrimitiveTopology::TriangleList
            }
            Primitive::TriangleFan => {
                indices.extend((2..n as u32).flat_map(|i| [0, i - 1, i]));
                wgpu::PrimitiveTopology::TriangleList
            }
            Primitive::Lines => {
                indices.extend(0..n as u32 / 2 * 2);
                wgpu::PrimitiveTopology::LineList
            }
            Primitive::LineStrip => {
                indices.extend((1..n as u32).flat_map(|i| [i - 1, i]));
                wgpu::PrimitiveTopology::LineList
            }
            Primitive::Points => {
                indices.extend(0..n as u32);
                wgpu::PrimitiveTopology::PointList
            }
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
        let nverts = d.vertices.len();
        let state = d.state.clone();
        // vertex stream: position, two 8-bit colours, and only the texture coordinate sets a stage reads
        let n_uv = tev::tex_inputs(&state);
        let stride = tev::vertex_stride(n_uv);
        let quant = |c: [f32; 4]| c.map(|x| (x * 255.0 + 0.5).clamp(0.0, 255.0) as u8);
        let mut vb = std::mem::take(&mut self.scratch_vb);
        vb.clear();
        vb.resize(nverts * stride, 0);
        for (v, out) in d.vertices.iter().zip(vb.chunks_exact_mut(stride)) {
            out[0..16].copy_from_slice(bytemuck::cast_slice(&v.pos));
            out[16..20].copy_from_slice(&quant(v.col[0]));
            out[20..24].copy_from_slice(&quant(v.col[1]));
            for (i, uv) in v.uv[..n_uv].iter().enumerate() {
                out[24 + 12 * i..36 + 12 * i].copy_from_slice(bytemuck::cast_slice(uv));
            }
        }
        // Most draws follow each other with the same render state (the GX parser keeps one snapshot until a
        // register changes): the shader, pipeline, uniforms and the maps in use come from that state alone, so
        // they are the last draw's. The textures are not: main memory behind them can change with the state
        // untouched (data that was not loaded yet at the first draw), so they are looked up every time.
        let cached = match &self.draw_cache {
            Some(c) if Arc::ptr_eq(&c.state, &state) && c.topology == topology => {
                Some((c.shader_hash, c.pipeline.clone(), c.uniforms, c.used))
            }
            _ => None,
        };
        let (shader_hash, pipeline, uniforms, used) = match cached {
            Some(c) => c,
            None => {
                let key = tev::shader_key(&state);
                let (shader_hash, src) = match self.shader_srcs.get(&key) {
                    Some((h, s)) => (*h, s.clone()),
                    None => {
                        let s = tev::wgsl(&state);
                        let h = hash_str(&s);
                        let s: Rc<str> = s.into();
                        self.shader_srcs.insert(key, (h, s.clone()));
                        (h, s)
                    }
                };
                let pipeline = self.pipeline(&state, shader_hash, &src, topology);
                let uniforms = tev::uniforms(&state);
                // textures: only the maps a stage reads are fetched
                let mut used = [false; 8];
                for s in &state.tev[..state.num_tev_stages as usize] {
                    if s.tex_enable {
                        used[s.texmap as usize & 7] = true;
                    }
                }
                self.draw_cache = Some(DrawCache {
                    state: state.clone(),
                    topology,
                    shader_hash,
                    pipeline: pipeline.clone(),
                    uniforms,
                    used,
                });
                (shader_hash, pipeline, uniforms, used)
            }
        };
        let views: [Rc<wgpu::TextureView>; 8] = std::array::from_fn(|m| {
            if used[m] {
                self.texture(&state.textures[m], ram, tluts)
            } else {
                self.white.clone()
            }
        });
        let key: TexBindKey = (
            std::array::from_fn(|m| Rc::as_ptr(&views[m]) as usize),
            std::array::from_fn(|m| {
                let t = &state.textures[m];
                t.wrap_s & 3
                    | (t.wrap_t & 3) << 2
                    | (t.mag_linear as u8) << 4
                    | (t.min_linear as u8) << 5
            }),
        );
        if !self.tex_binds.contains_key(&key) {
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
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.texture_layout,
                entries: &entries,
            });
            // the views stay alive with the bind group, so their pointers stay unique keys
            self.tex_binds.insert(key, (bind, views.to_vec()));
        }
        let tbind = self.tex_binds[&key].0.clone();
        if let Some(m) = self.icon_shift(&state, &used, ram, &d.vertices) {
            let f = |v: &[u8], i: usize| f32::from_le_bytes([v[i], v[i + 1], v[i + 2], v[i + 3]]);
            for v in vb.chunks_exact_mut(stride) {
                // clip space: scale about the quad's centre, then shift (in NDC, so times w)
                let w = f(v, 12);
                for (k, centre, delta) in [(0, m.cx, m.dx), (4, m.cy, m.dy)] {
                    let ndc = f(v, k) / w;
                    let x = (centre + (ndc - centre) * m.scale + delta) * w;
                    v[k..k + 4].copy_from_slice(&x.to_le_bytes());
                }
            }
        }

        // what makes two draws of consecutive frames "the same object" for interpolation
        let mut sig = mix(mix(shader_hash, nverts as u64), indices.len() as u64);
        for m in (0..8).filter(|m| used[*m]) {
            sig = mix(sig, state.textures[m].addr as u64);
        }
        let rec = RecDraw {
            verts: vb,
            stride,
            indices,
            uniforms,
            pipeline,
            tbind,
            viewport: state.viewport,
            scissor: state.scissor,
            sig,
        };
        self.emit(&rec, &rec.verts);
        if self.mult > 1 && !self.use_alt {
            self.rec.push(Rec::Draw(rec));
        } else {
            // not kept for interpolation: the buffers serve the next draw
            self.scratch_idx = rec.indices;
            self.scratch_vb = rec.verts;
        }
    }

    /// Record `r` (with `verts` as its vertices) into the open render pass.
    fn emit(&mut self, r: &RecDraw, vbytes: &[u8]) {
        // Draw data goes into per-submit staging vectors, uploaded once in `flush` (one buffer each, not
        // three buffers per draw: the buffer count dominated the frame time).
        let ibytes: &[u8] = bytemuck::cast_slice(&r.indices);
        if vbytes.len() > VBUF_SIZE || ibytes.len() > IBUF_SIZE {
            return;
        }
        let stride = r.stride;
        let pad = |len: usize| (stride - len % stride) % stride;
        let s = &self.stream;
        if s.vdata.len() + pad(s.vdata.len()) + vbytes.len() > VBUF_SIZE
            || s.idata.len() + ibytes.len() > IBUF_SIZE
            || s.udata.len() + UNIFORM_STRIDE > UBUF_SIZE
        {
            self.flush();
        }
        let s = &mut self.stream;
        // a vertex stream is addressed with base_vertex in units of this draw's stride: start on one
        let vpad = pad(s.vdata.len());
        s.vdata.resize(s.vdata.len() + vpad, 0);
        let (voff, ioff) = (s.vdata.len(), s.idata.len());
        s.vdata.extend_from_slice(vbytes);
        s.idata.extend_from_slice(ibytes);
        // most draws in a row share their TEV state: keep one copy of identical uniforms
        let ub: &[u8] = bytemuck::cast_slice(&r.uniforms);
        let uoff = match s.last_u {
            Some(o) if s.udata[o..o + ub.len()] == *ub => o,
            _ => {
                let o = s.udata.len();
                s.udata.extend_from_slice(ub);
                s.udata.resize(o + UNIFORM_STRIDE, 0);
                s.last_u = Some(o);
                o
            }
        };

        let clear = self.pending_clear.take();
        if clear.is_some() {
            self.pass = None; // a clear is a load op: it needs a fresh pass
        }
        if self.pass.is_none() {
            let (efb_view, depth_view) = if self.use_alt {
                (&self.alt_view, &self.alt_depth_view)
            } else {
                (&self.efb_view, &self.depth_view)
            };
            let enc = self
                .encoder
                .get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
            self.pass = Some(
                enc.begin_render_pass(&wgpu::RenderPassDescriptor {
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
                })
                .forget_lifetime(),
            );
            self.pstate = PassState::default();
        }
        let pass = self.pass.as_mut().expect("render pass");
        let st = &mut self.pstate;
        let s = self.scale as f32;
        let [vx, vy, vw, vh, _, _] = r.viewport;
        let (x0, y0) = (vx.max(0.0), vy.max(0.0));
        let (x1, y1) = (
            (vx + vw).min(EFB_WIDTH as f32),
            (vy + vh).min(EFB_HEIGHT as f32),
        );
        if x1 > x0 && y1 > y0 {
            let vp = [x0 * s, y0 * s, (x1 - x0) * s, (y1 - y0) * s];
            if st.viewport != Some(vp) {
                pass.set_viewport(vp[0], vp[1], vp[2], vp[3], 0.0, 1.0);
                st.viewport = Some(vp);
            }
        }
        let [sx, sy, sw, sh] = r.scissor;
        let (sx0, sy0) = (
            sx.clamp(0, EFB_WIDTH as i32),
            sy.clamp(0, EFB_HEIGHT as i32),
        );
        let (sx1, sy1) = (
            (sx + sw).clamp(0, EFB_WIDTH as i32),
            (sy + sh).clamp(0, EFB_HEIGHT as i32),
        );
        if sx1 > sx0 && sy1 > sy0 {
            let k = self.scale;
            let sc = [
                sx0 as u32 * k,
                sy0 as u32 * k,
                (sx1 - sx0) as u32 * k,
                (sy1 - sy0) as u32 * k,
            ];
            if st.scissor != Some(sc) {
                pass.set_scissor_rect(sc[0], sc[1], sc[2], sc[3]);
                st.scissor = Some(sc);
            }
        }
        // Most draws in a row share pipeline and textures: only changes are recorded. The vertex and index
        // buffers are bound whole, once per pass; each draw addresses its slice with first_index/base_vertex.
        if st.pipeline.as_ref() != Some(&r.pipeline) {
            pass.set_pipeline(&r.pipeline);
            st.pipeline = Some(r.pipeline.clone());
        }
        if st.uoff != Some(uoff) {
            pass.set_bind_group(0, &self.stream.ubind, &[uoff as u32]);
            st.uoff = Some(uoff);
        }
        if st.tbind.as_ref() != Some(&r.tbind) {
            pass.set_bind_group(1, &r.tbind, &[]);
            st.tbind = Some(r.tbind.clone());
        }
        if !st.buffers_bound {
            pass.set_vertex_buffer(0, self.stream.vbuf.slice(..));
            pass.set_index_buffer(self.stream.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            st.buffers_bound = true;
        }
        let first_index = (ioff / 4) as u32;
        let base_vertex = (voff / r.stride) as i32;
        pass.draw_indexed(
            first_index..first_index + r.indices.len() as u32,
            base_vertex,
            0..1,
        );
    }

    fn clear_pass(&mut self, color: [f64; 4], z: f32, color_update: bool, z_update: bool) {
        if self.mult > 1 && !self.use_alt {
            self.rec.push(Rec::Clear {
                color,
                z,
                color_update,
                z_update,
            });
        }
        self.end_pass();
        let (efb_view, depth_view) = if self.use_alt {
            (&self.alt_view, &self.alt_depth_view)
        } else {
            (&self.efb_view, &self.depth_view)
        };
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

    /// Read back a region of the EFB (coordinates in GameCube pixels) as RGBA8; the result is `scale()`
    /// times larger in each direction.
    pub fn read_efb(&mut self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        self.flush();
        let s = self.scale;
        let (w, h) = (
            w.min(EFB_WIDTH - x.min(EFB_WIDTH)) * s,
            h.min(EFB_HEIGHT - y.min(EFB_HEIGHT)) * s,
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
                texture: if self.read_alt { &self.alt } else { &self.efb },
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: x * s,
                    y: y * s,
                    z: 0,
                },
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

    /// Apply what the player changed in the settings overlay since the last frame.
    fn apply_settings(&mut self) {
        let generation = self.settings.generation();
        if generation == self.settings_gen {
            return;
        }
        self.settings_gen = generation;
        let s = self.settings.get();
        if let Some(p) = &mut self.presenter {
            p.set_aspect(s.video.aspect);
            p.set_fullscreen(s.video.fullscreen);
        }
        let mult = if self.interpolates {
            s.video.frame_multiplier
        } else {
            1
        };
        let paced_changed = mult != self.mult || s.video.vsync != self.vsync;
        if mult != self.mult {
            self.mult = mult;
            // the frames recorded so far were not kept for the new rate
            self.prev = None;
            self.plan = None;
            self.rec.clear();
        }
        self.vsync = s.video.vsync;
        self.show_stats = s.video.stats != fepor_core::settings::StatsMode::Off;
        if let (true, Some(p)) = (paced_changed, &mut self.presenter) {
            p.set_present(&self.device, mult > 1, self.vsync);
        }
        if s.video.resolution != self.scale {
            self.flush();
            self.scale = s.video.resolution;
            let t = make_targets(&self.device, self.scale);
            self.efb = t.efb;
            self.efb_view = t.efb_view;
            self.depth_view = t.depth_view;
            self.alt = t.alt;
            self.alt_view = t.alt_view;
            self.alt_depth_view = t.alt_depth_view;
            self.xfb = t.xfb;
            self.xfb_view = t.xfb_view;
            // copies already made keep their size: they are sampled by coordinate, not by pixel
            self.prev = None;
            self.plan = None;
            self.rec.clear();
            self.pending_clear = Some(([0.0; 4], 1.0));
        }
    }

    /// Frame dumps have no window to draw the overlay on: draw it on the EFB (the next frame redraws it).
    fn paint_overlay_on_efb(&mut self) {
        self.flush();
        let Some(o) = &mut self.overlay else { return };
        let mut enc = self.device.create_command_encoder(&Default::default());
        let s = self.scale;
        o.paint(
            &self.device,
            &self.queue,
            &mut enc,
            &self.efb_view,
            wgpu::TextureFormat::Rgba8Unorm,
            [EFB_WIDTH * s, EFB_HEIGHT * s],
            s as f32,
        );
        self.queue.submit([enc.finish()]);
    }

    fn present(&mut self, c: &CopyEfb) {
        self.frame += 1;
        self.hash_memo.clear();
        self.fps_frames += 1;
        self.stats_frame();
        let since = self.fps_since.elapsed();
        if since >= Duration::from_secs(1) {
            if let Some(o) = &self.overlay {
                o.shared()
                    .set_guest_fps(self.fps_frames as f32 / since.as_secs_f32());
                if let Some(mhz) = self.presenter.as_ref().and_then(|p| p.refresh_millihz()) {
                    o.shared().set_display_millihz(mhz);
                }
            }
            if self.trace_tex {
                eprintln!(
                    "tex: {} textures cached; wrongly reused by the old key since start: {} (data changed), {} (palette changed); EFB copies dropped after the game wrote there: {}",
                    self.textures.len(),
                    self.stale_data,
                    self.stale_palette,
                    self.stale_efb
                );
            }
            (self.fps_frames, self.fps_since) = (0, Instant::now());
        }
        if self.trace {
            eprintln!(
                "gx: frame {} ({}x{} at {},{}): {} draws",
                self.frame, c.width, c.height, c.x, c.y, self.draws
            );
        }
        if self.trace_hash {
            let (w, h) = (c.width.min(EFB_WIDTH), c.height.min(EFB_HEIGHT));
            let px = self.read_efb(c.x, c.y, w, h);
            let h = px.iter().fold(0xcbf2_9ce4_8422_2325u64, |a, b| {
                (a ^ *b as u64).wrapping_mul(0x100_0000_01b3)
            });
            eprintln!("gx: hash {} {h:016x}", self.frame);
        }
        self.draws = 0;
        self.show_frame(c);
        let Some(d) = &self.dump else { return };
        if self.frame % d.every != 0 {
            return;
        }
        let path = d.dir.join(format!("frame_{:05}.png", self.frame));
        let (w, h) = (c.width.min(EFB_WIDTH), c.height.min(EFB_HEIGHT));
        if self.presenter.is_none() {
            self.paint_overlay_on_efb();
        }
        let rgba = self.read_efb(c.x, c.y, w, h);
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        write_png(
            &path,
            w * self.scale,
            h * self.scale,
            png::ColorType::Rgb,
            &rgb,
        );
    }

    /// Show a finished frame: right away, or (with `mult > 1`) as the plan of interpolated images
    /// followed by the real one.
    fn show_frame(&mut self, c: &CopyEfb) {
        self.apply_settings();
        // the player changed the bindings or the button-name family: icons made before are stale
        let icon_gen = self
            .overlay
            .as_ref()
            .map_or(0, |o| o.shared().icon_generation());
        if icon_gen != self.icon_gen {
            self.icon_gen = icon_gen;
            self.textures.clear();
            self.tex_binds.clear();
        }
        if self.presenter.is_none() && self.dump.is_none() {
            return;
        }
        if self.mult <= 1 {
            if let Some(p) = &mut self.presenter {
                p.show(
                    &self.device,
                    &self.queue,
                    &self.efb_view,
                    c,
                    self.overlay.as_mut(),
                );
            }
            return;
        }
        let now = Instant::now();
        if let Some(l) = self.last_present {
            let dt = (now - l).clamp(Duration::from_millis(5), Duration::from_millis(50));
            self.period = (self.period * 7 + dt) / 8;
        }
        self.last_present = Some(now);
        self.stat_frames += 1;
        if self.stat_frames == 120 {
            if self.trace {
                eprintln!(
                    "gx: interpolation: {} images over 120 frames in {:?}",
                    self.stat_images,
                    now - self.stat_since
                );
            }
            (self.stat_frames, self.stat_images, self.stat_since) = (0, 0, now);
        }
        let cur = Rc::new(std::mem::take(&mut self.rec));
        // keep the real frame: the efb is redrawn by the next one while the interpolated images show
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_texture(
            self.efb.as_image_copy(),
            self.xfb.as_image_copy(),
            wgpu::Extent3d {
                width: EFB_WIDTH * self.scale,
                height: EFB_HEIGHT * self.scale,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        match self.prev.replace(cur.clone()) {
            Some(prev) => {
                self.plan = Some(Plan {
                    prev,
                    cur,
                    copy: c.clone(),
                    start: now,
                    period: self.period,
                    j: 1,
                });
                self.tick_plan();
            }
            None => {
                if let Some(p) = &mut self.presenter {
                    p.show(
                        &self.device,
                        &self.queue,
                        &self.xfb_view,
                        c,
                        self.overlay.as_mut(),
                    );
                }
            }
        }
    }

    /// Show the next image of the current plan once it is due (called often, from the guest's poll points).
    fn tick_plan(&mut self) {
        let Some(p) = &self.plan else { return };
        let due = p.start + p.period * (p.j - 1) / self.mult;
        if Instant::now() < due {
            return;
        }
        let p = self.plan.take().expect("plan");
        self.stat_images += 1;
        // the guest may be in the middle of a frame: hand its work to the GPU first
        self.flush();
        if p.j < self.mult {
            self.show_interp(&p.prev, &p.cur, p.j as f32 / self.mult as f32, &p.copy);
            self.plan = Some(Plan { j: p.j + 1, ..p });
        } else if let Some(pr) = &mut self.presenter {
            pr.show(
                &self.device,
                &self.queue,
                &self.xfb_view,
                &p.copy,
                self.overlay.as_mut(),
            );
        }
    }

    /// Draw the frame `cur` with every object `t` of the way from where it was in `prev`, and show it.
    fn show_interp(&mut self, prev: &[Rec], cur: &[Rec], t: f32, c: &CopyEfb) {
        self.use_alt = true;
        // a changed draw count shifts every later draw against its counterpart (typewriter text adds
        // one glyph per frame): no interpolation for such a frame
        let draws = |f: &[Rec]| f.iter().filter(|r| matches!(r, Rec::Draw(_))).count();
        let aligned = draws(prev) == draws(cur);
        let mut before = prev
            .iter()
            .filter_map(|r| match r {
                Rec::Draw(d) => Some(d),
                Rec::Clear { .. } => None,
            })
            .filter(|_| aligned);
        for r in cur {
            match r {
                Rec::Clear {
                    color,
                    z,
                    color_update,
                    z_update,
                } => self.clear_pass(*color, *z, *color_update, *z_update),
                Rec::Draw(d) => {
                    let verts = lerp_verts(before.next(), d, t);
                    self.emit(d, &verts);
                }
            }
        }
        self.flush();
        self.use_alt = false;
        if let Some(p) = &mut self.presenter {
            p.show(
                &self.device,
                &self.queue,
                &self.alt_view,
                c,
                self.overlay.as_mut(),
            );
        }
        if self
            .dump
            .as_ref()
            .is_some_and(|d| self.frame % d.every == 0)
        {
            self.read_alt = true;
            let (w, h) = (c.width.min(EFB_WIDTH), c.height.min(EFB_HEIGHT));
            let rgba = self.read_efb(c.x, c.y, w, h);
            self.read_alt = false;
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let name = format!("frame_{:05}_t{:02}.png", self.frame, (t * 100.0) as u32);
            let path = self.dump.as_ref().expect("dump").dir.join(name);
            write_png(
                &path,
                w * self.scale,
                h * self.scale,
                png::ColorType::Rgb,
                &rgb,
            );
        }
    }

    fn copy(&mut self, c: CopyEfb, ram: &[u8]) {
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
                    width: w * self.scale,
                    height: h * self.scale,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.end_pass();
            let efb = &self.efb;
            let enc = self
                .encoder
                .get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: efb,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x * self.scale,
                        y: y * self.scale,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w * self.scale,
                    height: h * self.scale,
                    depth_or_array_layers: 1,
                },
            );
            let view = Rc::new(tex.create_view(&Default::default()));
            if c.format == 4 {
                // RGB565 has no alpha channel: what the game reads back is opaque, whatever alpha the draws
                // left in the EFB
                let enc = self
                    .encoder
                    .get_or_insert_with(|| self.device.create_command_encoder(&Default::default()));
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("opaque copy"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.alpha_one);
                pass.draw(0..3, 0..1);
            }
            self.efb_textures.retain(|t| t.addr != c.dest);
            let (tw, th) = if c.half_scale { (w / 2, h / 2) } else { (w, h) };
            // what main memory holds there now: if the game later writes a texture at that address, this
            // copy is no longer what the address means
            let ram_len = texture::image_size(c.format, tw, th);
            let a = c.dest as usize & 0x01FF_FFFF;
            let ram_fp = ram
                .get(a..a + ram_len)
                .map_or(0, |d| Self::fingerprint(d, ram_len));
            if self.trace_tex {
                eprintln!(
                    "efbcopy: frame {} dest {:08x} {tw}x{th} fmt {} len {ram_len} ram fp {ram_fp:x}",
                    self.frame, c.dest, c.format
                );
            }
            self.efb_textures.push(EfbTexture {
                addr: c.dest,
                width: tw,
                height: th,
                view,
                ram_len,
                ram_fp,
            });
            if self.efb_textures.len() > 64 {
                self.efb_textures.remove(0);
            }
        }
        if self.trace_tex && self.tex_watch == Some(c.dest) {
            let px = self.read_efb(c.x, c.y, c.width, c.height);
            let n = (px.len() / 4).max(1) as u64;
            let sum = |k: usize| px.chunks_exact(4).map(|p| p[k] as u64).sum::<u64>() / n;
            eprintln!(
                "efbcontent: frame {} dest {:08x} region {},{} {}x{} average r {} g {} b {} a {}",
                self.frame,
                c.dest,
                c.x,
                c.y,
                c.width,
                c.height,
                sum(0),
                sum(1),
                sum(2),
                sum(3)
            );
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
    fn tick(&mut self) {
        if self.plan.is_some() {
            let t = Instant::now();
            self.tick_plan();
            self.busy_interp += t.elapsed();
            self.tot_interp += t.elapsed();
        }
    }

    fn recycle(&mut self) -> Vec<fepor_gx::state::Vertex> {
        std::mem::take(&mut self.spare_verts)
    }

    fn event(&mut self, event: Event, ram: &[u8], tluts: &Tluts) {
        let t = Instant::now();
        match event {
            Event::Draw(mut d) => {
                self.draw(&d, ram, tluts);
                self.spare_verts = std::mem::take(&mut d.vertices);
            }
            Event::Copy(c) => self.copy(c, ram),
            Event::Finish => {
                self.flush();
                self.hash_memo.clear();
            }
            Event::Token(_) => {}
        }
        self.busy_events += t.elapsed();
        self.tot_events += t.elapsed();
        if self.trace && self.busy_since.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "gx: guest thread per second: {} ms in draw/copy events, {} ms in interpolated images",
                self.busy_events.as_millis(),
                self.busy_interp.as_millis()
            );
            (self.busy_events, self.busy_interp, self.busy_since) =
                (Duration::ZERO, Duration::ZERO, Instant::now());
        }
    }
}

/// What the statistics overlay needs from the renderer between two publications.
struct StatsState {
    frame_ms: std::collections::VecDeque<f32>,
    last_frame: Option<Instant>,
    cpu: sysstat::CpuSampler,
    gpu_busy_path: Option<PathBuf>,
    gpu_name: String,
    backend: String,
    /// Submission-to-completion time of the last batch of GPU work, in microseconds.
    gpu_latency_us: Arc<std::sync::atomic::AtomicU32>,
    at: Instant,
    frames: u32,
    shown: u64,
    events: Duration,
    interp: Duration,
    ax: u64,
    ai: u64,
}

impl StatsState {
    fn new(info: &wgpu::AdapterInfo) -> StatsState {
        use std::sync::atomic::Ordering::Relaxed;
        StatsState {
            frame_ms: Default::default(),
            last_frame: None,
            cpu: sysstat::CpuSampler::new(),
            gpu_busy_path: sysstat::gpu_busy_path(info.vendor, info.device),
            gpu_name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            gpu_latency_us: Arc::new(Default::default()),
            at: Instant::now(),
            frames: 0,
            shown: 0,
            events: Duration::ZERO,
            interp: Duration::ZERO,
            ax: fepor_core::stats::AX_FRAMES.load(Relaxed),
            ai: fepor_core::stats::AI_BUFFERS.load(Relaxed),
        }
    }
}

impl Renderer {
    /// One guest frame finished: update the frame-time history and, four times a second, publish a
    /// snapshot for the statistics overlay.
    fn stats_frame(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let (Some(overlay), true) = (&self.overlay, self.show_stats) else {
            return;
        };
        let now = Instant::now();
        let st = &mut self.stats;
        if let Some(l) = st.last_frame {
            if st.frame_ms.len() == 120 {
                st.frame_ms.pop_front();
            }
            st.frame_ms.push_back((now - l).as_secs_f32() * 1000.0);
        }
        st.last_frame = Some(now);
        st.frames += 1;
        // how long the GPU takes to finish what was submitted so far
        let (lat, t0) = (st.gpu_latency_us.clone(), now);
        self.queue.on_submitted_work_done(move || {
            lat.store(
                t0.elapsed().as_micros().min(u32::MAX as u128) as u32,
                Relaxed,
            )
        });
        let dt = st.at.elapsed();
        if dt < Duration::from_millis(250) {
            return;
        }
        let secs = dt.as_secs_f32();
        let shown = self.presenter.as_ref().map_or(0, |p| p.shown);
        let (ax, ai) = (
            fepor_core::stats::AX_FRAMES.load(Relaxed),
            fepor_core::stats::AI_BUFFERS.load(Relaxed),
        );
        let cpu = st.cpu.sample();
        let ms = |d: Duration| d.as_secs_f32() * 1000.0 / secs;
        overlay.shared().set_stats(fepor_overlay::StatsSnapshot {
            guest_fps: st.frames as f32 / secs,
            shown_fps: (shown - st.shown) as f32 / secs,
            display_hz: overlay.shared().display_hz(),
            frame_ms: st.frame_ms.iter().copied().collect(),
            draw_ms_per_s: ms(self.tot_events - st.events),
            interp_ms_per_s: ms(self.tot_interp - st.interp),
            draws_per_frame: self.draws as u32,
            cpu_process: cpu.cpu_process,
            rss_mib: cpu.rss_mib,
            threads: cpu.threads,
            gpu_busy: st.gpu_busy_path.as_deref().and_then(sysstat::read_gpu_busy),
            gpu_latency_ms: st.gpu_latency_us.load(Relaxed) as f32 / 1000.0,
            gpu_name: st.gpu_name.clone(),
            backend: st.backend.clone(),
            target: (EFB_WIDTH * self.scale, EFB_HEIGHT * self.scale),
            ax_per_s: (ax - st.ax) as f32 / secs,
            ai_per_s: (ai - st.ai) as f32 / secs,
        });
        (
            st.at, st.frames, st.shown, st.events, st.interp, st.ax, st.ai,
        ) = (
            Instant::now(),
            0,
            shown,
            self.tot_events,
            self.tot_interp,
            ax,
            ai,
        );
    }
}
