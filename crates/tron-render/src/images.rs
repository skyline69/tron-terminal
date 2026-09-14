//! Images: kitty graphics protocol placements, Unicode placeholders and Sixel.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use foldhash::HashMap;
use tron_core::graphics::{BELOW_BACKGROUND_Z, placeholder_cell};
use tron_core::{Image, Snapshot};
use tron_font::CellMetrics;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct ImageInstance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
}

const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
    0 => Float32x2,
    1 => Float32x2,
    2 => Float32x4,
];

/// Where images are drawn relative to cells.
#[derive(Copy, Clone)]
pub enum Layer {
    BelowBackground = 0,
    BelowText = 1,
    AboveText = 2,
}

struct GpuImage {
    generation: u64,
    size: (u32, u32),
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

pub struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    uniform_bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    textures: HashMap<u32, GpuImage>,
    instances: Vec<ImageInstance>,
    layers: [Vec<(u32, u32)>; 3],
    buffer: wgpu::Buffer,
    capacity: usize,
    scratch: Vec<u8>,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("image.wgsl"));
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image uniforms"),
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
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image texture"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("images"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("images"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<ImageInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ATTRIBUTES,
                })],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("images"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let capacity = 16;
        Self {
            pipeline,
            texture_layout,
            uniform_bind_group,
            uniforms,
            sampler,
            textures: HashMap::default(),
            instances: Vec::new(),
            layers: Default::default(),
            buffer: create_buffer(device, capacity),
            capacity,
            scratch: Vec::new(),
        }
    }

    /// Computes visible placements and uploads new or changed images.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snapshot: &Snapshot,
        metrics: CellMetrics,
        padding: [f32; 2],
        srgb: bool,
    ) {
        self.instances.clear();
        for layer in &mut self.layers {
            layer.clear();
        }
        let images = &snapshot.images;
        self.textures.retain(|id, _| images.contains_key(id));
        if snapshot.placements.is_empty() {
            return;
        }
        let (cell_w, cell_h) = (metrics.width as f32, metrics.height as f32);

        // Placements drawn at a grid position.
        let top = snapshot.top_line;
        let rows = snapshot.rows() as i64;
        let mut placements: Vec<_> = snapshot
            .placements
            .iter()
            .filter(|p| !p.virtual_placement && p.alt_screen == snapshot.alt_screen)
            .filter(|p| p.line + i64::from(p.rows) > top && p.line < top + rows)
            .collect();
        placements.sort_by_key(|p| p.z);
        for placement in placements {
            let Some(image) = images.get(&placement.image_id) else { continue };
            self.ensure_texture(device, queue, image, srgb);
            let [sx, sy, sw, sh] = placement.source;
            let (width, height) = if placement.scaled {
                (
                    placement.cols as f32 * cell_w - placement.offset_x as f32,
                    placement.rows as f32 * cell_h - placement.offset_y as f32,
                )
            } else {
                (sw as f32, sh as f32)
            };
            let row = (placement.line - top) as f32;
            let (iw, ih) = (image.width.max(1) as f32, image.height.max(1) as f32);
            let layer = if placement.z < BELOW_BACKGROUND_Z {
                Layer::BelowBackground
            } else if placement.z < 0 {
                Layer::BelowText
            } else {
                Layer::AboveText
            };
            self.push(
                layer,
                image.id,
                ImageInstance {
                    pos: [
                        padding[0] + placement.col as f32 * cell_w + placement.offset_x as f32,
                        padding[1] + row * cell_h + placement.offset_y as f32,
                    ],
                    size: [width, height],
                    uv: [sx as f32 / iw, sy as f32 / ih, (sx + sw) as f32 / iw, (sy + sh) as f32 / ih],
                },
            );
        }

        // Unicode placeholders: each cell shows one tile of a virtual placement.
        if !snapshot.placements.iter().any(|p| p.virtual_placement) {
            return;
        }
        for (y, row) in snapshot.rows.iter().enumerate() {
            let mut previous = None;
            for (x, cell) in row.cells.iter().enumerate() {
                let Some(tile) = placeholder_cell(cell, row.combining(x), previous) else {
                    previous = None;
                    continue;
                };
                previous = Some(tile);
                let Some(image) = images.get(&tile.image_id) else { continue };
                let Some(placement) = snapshot.placements.iter().find(|p| {
                    p.virtual_placement
                        && p.image_id == tile.image_id
                        && (tile.placement_id == 0 || p.placement_id == tile.placement_id)
                }) else {
                    continue;
                };
                // Fit the image into the placement's cell box, centered, keeping its aspect ratio.
                let [sx, sy, sw, sh] = placement.source;
                let box_w = placement.cols as f32 * cell_w;
                let box_h = placement.rows as f32 * cell_h;
                let scale = (box_w / sw.max(1) as f32).min(box_h / sh.max(1) as f32);
                let (fit_w, fit_h) = (sw as f32 * scale, sh as f32 * scale);
                let (origin_x, origin_y) = ((box_w - fit_w) / 2.0, (box_h - fit_h) / 2.0);
                let (cell_x, cell_y) = (tile.col as f32 * cell_w, tile.row as f32 * cell_h);
                let x0 = cell_x.max(origin_x);
                let y0 = cell_y.max(origin_y);
                let x1 = (cell_x + cell_w).min(origin_x + fit_w);
                let y1 = (cell_y + cell_h).min(origin_y + fit_h);
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                self.ensure_texture(device, queue, image, srgb);
                let (iw, ih) = (image.width.max(1) as f32, image.height.max(1) as f32);
                let u = |px: f32| (sx as f32 + (px - origin_x) / scale) / iw;
                let v = |py: f32| (sy as f32 + (py - origin_y) / scale) / ih;
                self.push(
                    Layer::AboveText,
                    image.id,
                    ImageInstance {
                        pos: [
                            padding[0] + x as f32 * cell_w + (x0 - cell_x),
                            padding[1] + y as f32 * cell_h + (y0 - cell_y),
                        ],
                        size: [x1 - x0, y1 - y0],
                        uv: [u(x0), v(y0), u(x1), v(y1)],
                    },
                );
            }
        }
    }

    fn push(&mut self, layer: Layer, image: u32, instance: ImageInstance) {
        self.layers[layer as usize].push((image, self.instances.len() as u32));
        self.instances.push(instance);
    }

    /// Uploads an image, reusing its texture when only the pixels changed (video frames).
    fn ensure_texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, image: &Image, srgb: bool) {
        let size = (image.width, image.height);
        let reuse = match self.textures.get(&image.id) {
            Some(gpu) if gpu.generation == image.generation => return,
            Some(gpu) => gpu.size == size,
            None => false,
        };
        if !reuse {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("image"),
                size: wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("image"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.textures.insert(image.id, GpuImage { generation: 0, size, texture, bind_group });
        }
        // Textures hold premultiplied alpha. Opaque images need no conversion.
        let pixels: &[u8] = if image.opaque {
            &image.rgba
        } else {
            self.scratch.clear();
            self.scratch.extend(image.rgba.as_chunks::<4>().0.iter().flat_map(|p| {
                let a = u16::from(p[3]);
                let scale = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
                [scale(p[0]), scale(p[1]), scale(p[2]), p[3]]
            }));
            &self.scratch
        };
        let gpu = self.textures.get_mut(&image.id).expect("texture inserted above");
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * image.width),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 },
        );
        gpu.generation = image.generation;
    }

    pub fn upload_instances(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, viewport: [f32; 2]) {
        if self.instances.is_empty() {
            return;
        }
        if self.instances.len() > self.capacity {
            self.capacity = self.instances.len().next_power_of_two();
            self.buffer = create_buffer(device, self.capacity);
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.instances));
        queue.write_buffer(&self.uniforms, 0, bytemuck::cast_slice(&[viewport[0], viewport[1], 0.0, 0.0]));
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, layer: Layer) {
        let items = &self.layers[layer as usize];
        if items.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        let mut start = 0;
        // Consecutive instances of the same image share one draw call.
        while start < items.len() {
            let image = items[start].0;
            let mut end = start + 1;
            while end < items.len() && items[end].0 == image && items[end].1 == items[end - 1].1 + 1 {
                end += 1;
            }
            if let Some(gpu) = self.textures.get(&image) {
                pass.set_bind_group(1, &gpu.bind_group, &[]);
                pass.draw(0..4, items[start].1..items[end - 1].1 + 1);
            }
            start = end;
        }
    }
}

fn create_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image instances"),
        size: (capacity * size_of::<ImageInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
