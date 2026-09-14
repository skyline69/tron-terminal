//! User WGSL post-processing shaders, applied in order to the rendered terminal.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

const PRELUDE: &str = include_str!("post_prelude.wgsl");

/// A user shader to compile.
#[derive(Clone, Debug)]
pub struct PostShader {
    pub name: String,
    pub source: String,
}

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Default)]
pub struct PostUniforms {
    pub resolution: [f32; 2],
    pub time: f32,
    pub frame: u32,
    pub cursor: [f32; 4],
    pub cell_size: [f32; 2],
    pub focused: f32,
    pub _padding: f32,
    pub background: [f32; 4],
}

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

pub struct PostChain {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    passes: Vec<wgpu::RenderPipeline>,
    targets: Vec<Target>,
    size: (u32, u32),
    animated: bool,
}

impl PostChain {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post uniforms"),
            size: size_of::<PostUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            format,
            layout,
            pipeline_layout,
            sampler,
            uniforms,
            passes: Vec::new(),
            targets: Vec::new(),
            size: (0, 0),
            animated: false,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.passes.is_empty()
    }

    pub fn is_animated(&self) -> bool {
        self.is_active() && self.animated
    }

    /// Compiles `shaders`, replacing the current chain. Shaders that fail to
    /// compile are skipped. Returns one message per failure.
    ///
    /// `animation` forces continuous redraws on or off. `None` animates when a
    /// shader reads `tron.time` or `tron.frame`.
    pub fn set_shaders(&mut self, device: &wgpu::Device, shaders: &[PostShader], animation: Option<bool>) -> Vec<String> {
        let mut errors = Vec::new();
        let mut passes = Vec::new();
        let mut uses_time = false;
        let prelude_lines = PRELUDE.lines().count();
        for shader in shaders {
            let source = format!("{PRELUDE}{}", shader.source);
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&shader.name),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&shader.name),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("tron_vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("tron_fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
            if let Some(error) = pollster::block_on(scope.pop()) {
                errors.push(format!(
                    "shader `{}` failed to compile (line numbers include {prelude_lines} prelude lines):\n{error}",
                    shader.name
                ));
                continue;
            }
            uses_time |= shader.source.contains("tron.time") || shader.source.contains("tron.frame");
            passes.push(pipeline);
        }
        self.passes = passes;
        self.animated = animation.unwrap_or(uses_time);
        let size = self.size;
        self.size = (0, 0);
        self.resize(device, size.0, size.1);
        errors
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if !self.is_active() {
            self.targets.clear();
            self.size = size;
            return;
        }
        if size == self.size && !self.targets.is_empty() {
            return;
        }
        self.size = size;
        let count = if self.passes.len() > 1 { 2 } else { 1 };
        self.targets = (0..count)
            .map(|i| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(if i == 0 { "post target a" } else { "post target b" }),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("post"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                });
                Target { _texture: texture, view, bind_group }
            })
            .collect();
    }

    /// Where the terminal is rendered when the chain is active.
    pub fn input_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.first().map(|t| &t.view)
    }

    pub fn run(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, output: &wgpu::TextureView, uniforms: &PostUniforms) {
        if self.targets.is_empty() {
            return;
        }
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(uniforms));
        let last = self.passes.len() - 1;
        for (i, pipeline) in self.passes.iter().enumerate() {
            let input = &self.targets[i % self.targets.len()];
            let view = if i == last { output } else { &self.targets[(i + 1) % self.targets.len()].view };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &input.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}
