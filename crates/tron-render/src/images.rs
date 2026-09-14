//! Kitty graphics protocol images.

use foldhash::HashMap;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use tron_core::Snapshot;
use tron_core::graphics::BELOW_BACKGROUND_Z;
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
    _texture: wgpu::Texture,
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
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
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
        self.textures
            .retain(|id, gpu| images.get(id).is_some_and(|image| image.generation == gpu.generation));
        if snapshot.placements.is_empty() {
            return;
        }

        let top = snapshot.top_line;
        let rows = snapshot.rows() as i64;
        let (cell_w, cell_h) = (metrics.width as f32, metrics.height as f32);
        let mut placements: Vec<_> = snapshot
            .placements
            .iter()
            .filter(|p| p.alt_screen == snapshot.alt_screen)
            .filter(|p| p.line + i64::from(p.rows) > top && p.line < top + rows)
            .collect();
        placements.sort_by_key(|p| p.z);

        for placement in placements {
            let Some(image) = images.get(&placement.image_id) else { continue };
            if !self.textures.contains_key(&image.id) {
                let gpu = self.upload(device, queue, image, srgb);
                self.textures.insert(image.id, gpu);
            }
            let [sx, sy, sw, sh] = placement.source;
            let (width, height) = if placement.scaled {
                // Kitty fills the given cells, keeping the other dimension natural
                // when only one of columns or rows was given.
                let fill_w = placement.cols as f32 * cell_w - placement.offset_x as f32;
                let fill_h = placement.rows as f32 * cell_h - placement.offset_y as f32;
                (fill_w, fill_h)
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
            self.layers[layer as usize].push((image.id, self.instances.len() as u32));
            self.instances.push(ImageInstance {
                pos: [
                    padding[0] + placement.col as f32 * cell_w + placement.offset_x as f32,
                    padding[1] + row * cell_h + placement.offset_y as f32,
                ],
                size: [width, height],
                uv: [sx as f32 / iw, sy as f32 / ih, (sx + sw) as f32 / iw, (sy + sh) as f32 / ih],
            });
        }
    }

    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, image: &tron_core::Image, srgb: bool) -> GpuImage {
        let size = wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let premultiplied: Vec<u8> = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = u16::from(p[3]);
                let scale = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
                [scale(p[0]), scale(p[1]), scale(p[2]), p[3]]
            })
            .collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &premultiplied,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * image.width), rows_per_image: Some(image.height) },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        GpuImage { generation: image.generation, _texture: texture, bind_group }
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
        for &(image, index) in items {
            if let Some(gpu) = self.textures.get(&image) {
                pass.set_bind_group(1, &gpu.bind_group, &[]);
                pass.draw(0..4, index..index + 1);
            }
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
