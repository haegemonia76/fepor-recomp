//! AMD FidelityFX Super Resolution 1 (ADR 0042): EASU, the edge-adaptive spatial upscale, runs here as a
//! render pass from the (lower resolution) copied region to a target of the output's size; RCAS, the
//! sharpening, is `RCAS` below, called by the scaling pass of `present.rs` on that target, ahead of the
//! render filters.
//!
//! Ported to WGSL from AMD's FidelityFX FSR 1.0 reference (`ffx_fsr1.h`, plain float path: the taps are
//! `textureLoad`s clamped to the copied region instead of gathers, and RCAS has no denoise), which is
//! distributed under the MIT licence:
//!
//! ```text
//! Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.
//! ```

use fepor_gx::state::CopyEfb;
use wgpu::util::DeviceExt;

use crate::present::PassTimer;
use crate::{EFB_HEIGHT, EFB_WIDTH};

/// RCAS (robust contrast-adaptive sharpening) for the scaling pass's fragment shader: the image `t` is the
/// EASU output at the output's size; `con` is `exp2(-sharpness in stops)`. Expects the module's `t`.
pub const RCAS: &str = r#"
fn rcas(pix: vec2<i32>, con: f32) -> vec3<f32> {
    let hi = vec2<i32>(textureDimensions(t)) - vec2<i32>(1);
    let b = textureLoad(t, clamp(pix + vec2<i32>(0, -1), vec2<i32>(0), hi), 0).rgb;
    let d = textureLoad(t, clamp(pix + vec2<i32>(-1, 0), vec2<i32>(0), hi), 0).rgb;
    let e = textureLoad(t, pix, 0).rgb;
    let f = textureLoad(t, clamp(pix + vec2<i32>(1, 0), vec2<i32>(0), hi), 0).rgb;
    let h = textureLoad(t, clamp(pix + vec2<i32>(0, 1), vec2<i32>(0), hi), 0).rgb;
    let mn4 = min(min(min(b, d), f), h);
    let mx4 = max(max(max(b, d), f), h);
    // the largest sharpening that cannot push any colour channel out of the ring's range
    let hit_min = min(mn4, e) / max(4.0 * mx4, vec3<f32>(1e-6));
    let hit_max = (vec3<f32>(1.0) - max(mx4, e)) / min(4.0 * mn4 - vec3<f32>(4.0), vec3<f32>(-1e-6));
    let lobe3 = max(-hit_min, hit_max);
    let lobe = max(-0.1875, min(max(lobe3.x, max(lobe3.y, lobe3.z)), 0.0)) * con;
    return (lobe * (b + d + h + f) + e) / (4.0 * lobe + 1.0);
}
"#;

const EASU: &str = r#"
// u.rect: the region to upscale in uv of t; u.out.xy: output size in pixels
struct U { rect: vec4<f32>, out: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var t: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(corners[i], 0.0, 1.0);
}
fn tap(p: vec2<i32>) -> vec3<f32> {
    let dims = vec2<f32>(textureDimensions(t));
    let lo = vec2<i32>(round(u.rect.xy * dims));
    let hi = vec2<i32>(round(u.rect.zw * dims)) - vec2<i32>(1);
    return textureLoad(t, clamp(p, lo, hi), 0).rgb;
}
fn luma(c: vec3<f32>) -> f32 {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
}
// Direction (xy) and length (z) of the edge around the centre texel c, weighted by the bilinear weight of
// that texel: a above, b left, d right, e below.
fn easu_set(acc: vec3<f32>, w: f32, la: f32, lb: f32, lc: f32, ld: f32, le: f32) -> vec3<f32> {
    let len_x = 1.0 / max(max(abs(ld - lc), abs(lc - lb)), 1e-8);
    let dir_x = ld - lb;
    var lx = clamp(abs(dir_x) * len_x, 0.0, 1.0);
    lx = lx * lx;
    let len_y = 1.0 / max(max(abs(le - lc), abs(lc - la)), 1e-8);
    let dir_y = le - la;
    var ly = clamp(abs(dir_y) * len_y, 0.0, 1.0);
    ly = ly * ly;
    return acc + vec3<f32>(dir_x * w, dir_y * w, (lx + ly) * w);
}
// One Lanczos-like tap: off is the texel's offset from the pixel, rotated onto the edge.
fn easu_tap(acc: ptr<function, vec4<f32>>, off: vec2<f32>, dir: vec2<f32>, len: vec2<f32>, lob: f32, clp: f32, c: vec3<f32>) {
    var v = vec2<f32>(off.x * dir.x + off.y * dir.y, off.x * -dir.y + off.y * dir.x);
    v = v * len;
    let d2 = min(v.x * v.x + v.y * v.y, clp);
    var wb = 0.4 * d2 - 1.0;
    var wa = lob * d2 - 1.0;
    wb = wb * wb;
    wa = wa * wa;
    wb = 25.0 / 16.0 * wb - (25.0 / 16.0 - 1.0);
    let w = wb * wa;
    *acc = *acc + vec4<f32>(c * w, w);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(t));
    let org = round(u.rect.xy * dims);
    let size = round(u.rect.zw * dims) - org;
    // the input position of this output pixel, in texels
    let ratio = size / u.out.xy;
    var pp = floor(p.xy) * ratio + 0.5 * ratio - 0.5;
    let fpf = floor(pp);
    pp = pp - fpf;
    let fp = vec2<i32>(fpf) + vec2<i32>(org);
    //    b c
    //  e f g h
    //  i j k l
    //    n o
    let cb = tap(fp + vec2<i32>(0, -1));
    let cc = tap(fp + vec2<i32>(1, -1));
    let ce = tap(fp + vec2<i32>(-1, 0));
    let cf = tap(fp);
    let cg = tap(fp + vec2<i32>(1, 0));
    let ch = tap(fp + vec2<i32>(2, 0));
    let ci = tap(fp + vec2<i32>(-1, 1));
    let cj = tap(fp + vec2<i32>(0, 1));
    let ck = tap(fp + vec2<i32>(1, 1));
    let cl = tap(fp + vec2<i32>(2, 1));
    let cn = tap(fp + vec2<i32>(0, 2));
    let co = tap(fp + vec2<i32>(1, 2));
    let lb = luma(cb); let lc = luma(cc); let le = luma(ce); let lf = luma(cf);
    let lg = luma(cg); let lh = luma(ch); let li = luma(ci); let lj = luma(cj);
    let lk = luma(ck); let ll = luma(cl); let ln = luma(cn); let lo = luma(co);
    var dl = vec3<f32>(0.0);
    dl = easu_set(dl, (1.0 - pp.x) * (1.0 - pp.y), lb, le, lf, lg, lj);
    dl = easu_set(dl, pp.x * (1.0 - pp.y), lc, lf, lg, lh, lk);
    dl = easu_set(dl, (1.0 - pp.x) * pp.y, lf, li, lj, lk, ln);
    dl = easu_set(dl, pp.x * pp.y, lg, lj, lk, ll, lo);
    var dir = dl.xy;
    var len = dl.z;
    let dir_r2 = dot(dir, dir);
    let zro = dir_r2 < 1.0 / 32768.0;
    let dir_r = select(inverseSqrt(dir_r2), 1.0, zro);
    dir.x = select(dir.x, 1.0, zro);
    dir = dir * dir_r;
    len = len * 0.5;
    len = len * len;
    // stretch the kernel from 1 (axis-aligned edge) to sqrt(2) (diagonal)
    let stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    let len2 = vec2<f32>(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    let clp = 1.0 / lob;
    let mn = min(min(cf, cg), min(cj, ck));
    let mx = max(max(cf, cg), max(cj, ck));
    var acc = vec4<f32>(0.0);
    easu_tap(&acc, vec2<f32>(0.0, -1.0) - pp, dir, len2, lob, clp, cb);
    easu_tap(&acc, vec2<f32>(1.0, -1.0) - pp, dir, len2, lob, clp, cc);
    easu_tap(&acc, vec2<f32>(-1.0, 1.0) - pp, dir, len2, lob, clp, ci);
    easu_tap(&acc, vec2<f32>(0.0, 1.0) - pp, dir, len2, lob, clp, cj);
    easu_tap(&acc, vec2<f32>(0.0, 0.0) - pp, dir, len2, lob, clp, cf);
    easu_tap(&acc, vec2<f32>(-1.0, 0.0) - pp, dir, len2, lob, clp, ce);
    easu_tap(&acc, vec2<f32>(1.0, 1.0) - pp, dir, len2, lob, clp, ck);
    easu_tap(&acc, vec2<f32>(2.0, 1.0) - pp, dir, len2, lob, clp, cl);
    easu_tap(&acc, vec2<f32>(2.0, 0.0) - pp, dir, len2, lob, clp, ch);
    easu_tap(&acc, vec2<f32>(1.0, 0.0) - pp, dir, len2, lob, clp, cg);
    easu_tap(&acc, vec2<f32>(1.0, 2.0) - pp, dir, len2, lob, clp, co);
    easu_tap(&acc, vec2<f32>(0.0, 2.0) - pp, dir, len2, lob, clp, cn);
    // no overshoot beyond the four nearest texels
    return vec4<f32>(min(mx, max(mn, acc.rgb / acc.w)), 1.0);
}
"#;

/// The EASU pass and its output, kept between frames while the output size stays.
pub struct Easu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    target: Option<((u32, u32), wgpu::TextureView)>,
}

/// The target format: a plain 8-bit image, as the EFB holds.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Easu {
    pub fn new(device: &wgpu::Device) -> Easu {
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fsr easu"),
            entries: &[
                entry(
                    0,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fsr easu"),
            source: wgpu::ShaderSource::Wgsl(EASU.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fsr easu"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fsr easu"),
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
                    format: FORMAT,
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
        Easu {
            layout,
            pipeline,
            target: None,
        }
    }

    /// Upscale the copied region `c` of `src` to `out` pixels, into `output()`.
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        c: &CopyEfb,
        out: (u32, u32),
        timer: Option<&PassTimer>,
    ) {
        if self.target.as_ref().is_none_or(|(size, _)| *size != out) {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fsr easu"),
                size: wgpu::Extent3d {
                    width: out.0,
                    height: out.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.target = Some((out, tex.create_view(&Default::default())));
        }
        let (_, view) = self.target.as_ref().expect("target");
        let (w, h) = (c.width.clamp(1, EFB_WIDTH), c.height.clamp(1, EFB_HEIGHT));
        let (x, y) = (c.x.min(EFB_WIDTH - 1), c.y.min(EFB_HEIGHT - 1));
        let u: [f32; 8] = [
            x as f32 / EFB_WIDTH as f32,
            y as f32 / EFB_HEIGHT as f32,
            (x + w).min(EFB_WIDTH) as f32 / EFB_WIDTH as f32,
            (y + h).min(EFB_HEIGHT) as f32 / EFB_HEIGHT as f32,
            out.0 as f32,
            out.1 as f32,
            0.0,
            0.0,
        ];
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&u),
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
                    resource: wgpu::BindingResource::TextureView(src),
                },
            ],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fsr easu"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timer.map(|t| t.writes(true, false)),
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The image `run` made last.
    pub fn output(&self) -> &wgpu::TextureView {
        &self.target.as_ref().expect("run made it").1
    }
}

/// RCAS sharpening constant for a sharpness of 0.0 ..= 1.0. FSR's own unit is stops of attenuation (0 =
/// sharpest, 2 = the weakest the reference offers), so sharpness 0.9 is the reference's default of 0.2 stops.
pub fn rcas_constant(sharpness: f32) -> f32 {
    (-2.0 * (1.0 - sharpness)).exp2()
}
