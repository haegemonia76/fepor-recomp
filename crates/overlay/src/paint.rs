//! Paints egui's tessellated output with wgpu (the part egui-wgpu would do, written against our wgpu).

use std::collections::HashMap;

use egui::epaint::{ImageData, Primitive, Vertex};
use egui::{ClippedPrimitive, TextureId, TexturesDelta};
use wgpu::util::DeviceExt;

const SHADER: &str = r#"
struct Locals { screen: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: Locals;
@group(1) @binding(0) var t: texture_2d<f32>;
@group(1) @binding(1) var s: sampler;
struct VO { @builtin(position) p: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) c: vec4<f32> };
@vertex fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) c: vec4<f32>) -> VO {
    var o: VO;
    o.p = vec4<f32>(2.0 * pos.x / u.screen.x - 1.0, 1.0 - 2.0 * pos.y / u.screen.y, 0.0, 1.0);
    o.uv = uv;
    o.c = c;
    return o;
}
// colours and textures are premultiplied gamma values; the target is not sRGB, so they are written as they are
@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
    return i.c * textureSample(t, s, i.uv);
}
"#;

pub struct Painter {
    pipeline: wgpu::RenderPipeline,
    locals_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: HashMap<TextureId, (wgpu::Texture, wgpu::BindGroup, [u32; 2])>,
}

impl Painter {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Painter {
        let locals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay locals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay"),
            bind_group_layouts: &[&locals_layout, &tex_layout],
            push_constant_ranges: &[],
        });
        let attrs = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 8,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Unorm8x4,
                offset: 16,
                shader_location: 2,
            },
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attrs,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
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
        Painter {
            pipeline,
            locals_layout,
            tex_layout,
            sampler,
            textures: HashMap::new(),
        }
    }

    fn set_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: TextureId,
        delta: &egui::epaint::ImageDelta,
    ) {
        let ImageData::Color(img) = &delta.image;
        let [w, h] = [img.size[0] as u32, img.size[1] as u32];
        let bytes: &[u8] = bytemuck::cast_slice(&img.pixels);
        let origin = match delta.pos {
            Some([x, y]) => wgpu::Origin3d {
                x: x as u32,
                y: y as u32,
                z: 0,
            },
            None => {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("overlay texture"),
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
                let view = texture.create_view(&Default::default());
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.tex_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                self.textures.insert(id, (texture, bind, [w, h]));
                wgpu::Origin3d::ZERO
            }
        };
        let Some((texture, _, _)) = self.textures.get(&id) else {
            return;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Draw `prims` over `view` (kept as it is where nothing is drawn). `size_px` is the target's size and
    /// `ppp` the points-to-pixels factor the primitives were tessellated with.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        size_px: [u32; 2],
        ppp: f32,
        delta: &TexturesDelta,
        prims: &[ClippedPrimitive],
    ) {
        for (id, deltas) in &delta.set {
            for d in deltas {
                self.set_texture(device, queue, *id, d);
            }
        }
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        // (clip rect in pixels, texture, index range, base vertex)
        let mut draws = Vec::new();
        for p in prims {
            let Primitive::Mesh(m) = &p.primitive else {
                continue;
            };
            if m.indices.is_empty() || !self.textures.contains_key(&m.texture_id) {
                continue;
            }
            let clip = [
                (p.clip_rect.min.x * ppp)
                    .round()
                    .clamp(0.0, size_px[0] as f32) as u32,
                (p.clip_rect.min.y * ppp)
                    .round()
                    .clamp(0.0, size_px[1] as f32) as u32,
                (p.clip_rect.max.x * ppp)
                    .round()
                    .clamp(0.0, size_px[0] as f32) as u32,
                (p.clip_rect.max.y * ppp)
                    .round()
                    .clamp(0.0, size_px[1] as f32) as u32,
            ];
            if clip[2] <= clip[0] || clip[3] <= clip[1] {
                continue;
            }
            let first = indices.len() as u32;
            let base = vertices.len() as i32;
            vertices.extend_from_slice(&m.vertices);
            indices.extend_from_slice(&m.indices);
            draws.push((clip, m.texture_id, first..indices.len() as u32, base));
        }
        if !draws.is_empty() {
            let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            let screen = [size_px[0] as f32 / ppp, size_px[1] as f32 / ppp, 0.0, 0.0];
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay locals"),
                contents: bytemuck::cast_slice(&screen),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let locals = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.locals_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubuf.as_entire_binding(),
                }],
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &locals, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
            for (clip, tex, range, base) in draws {
                pass.set_scissor_rect(clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]);
                pass.set_bind_group(1, &self.textures[&tex].1, &[]);
                pass.draw_indexed(range, base, 0..1);
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }
}
