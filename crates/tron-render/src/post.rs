//! User WGSL post-processing shaders, applied in order to the rendered terminal.
//!
//! Shaders that call `previous(uv)` read the final output of the previous frame.
//! For those, the last pass renders into one of two history textures, which is
//! then copied to the surface; the textures swap roles every frame. Without
//! such a shader no history is kept.

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};

const PRELUDE: &str = include_str!("post_prelude.wgsl");

const BLIT: &str = r"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs(v: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, source_sampler, v.uv, 0.0);
}
";

/// A user shader to compile.
#[derive(Clone, Debug)]
pub struct PostShader {
    pub name: String,
    pub source: String,
}

/// Matches `TronUniforms` in the prelude. 96 bytes, aligned to 16.
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
    pub previous_cursor: [f32; 4],
    pub cursor_change_time: f32,
    pub _padding2: [f32; 3],
}

struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

pub struct PostChain {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    cache: Option<wgpu::PipelineCache>,
    passes: Vec<wgpu::RenderPipeline>,
    /// Intermediate render targets, the first one receives the terminal.
    targets: Vec<Target>,
    /// History textures, only when a shader reads the previous frame.
    history: Vec<Target>,
    /// History texture written by the last frame.
    history_current: usize,
    /// Bind groups indexed by `[target][history read index]`.
    bind_groups: Vec<Vec<wgpu::BindGroup>>,
    blit: Option<Blit>,
    size: (u32, u32),
    animated: bool,
    uses_previous: bool,
    uses_cursor_motion: bool,
}

struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    bind_groups: Vec<wgpu::BindGroup>,
}

impl PostChain {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, cache: Option<&wgpu::PipelineCache>) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
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
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(3),
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
            cache: cache.cloned(),
            passes: Vec::new(),
            targets: Vec::new(),
            history: Vec::new(),
            history_current: 0,
            bind_groups: Vec::new(),
            blit: None,
            size: (0, 0),
            animated: false,
            uses_previous: false,
            uses_cursor_motion: false,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.passes.is_empty()
    }

    /// Whether the chain must redraw every frame.
    pub fn is_animated(&self) -> bool {
        self.is_active() && self.animated
    }

    /// Whether a shader reads the cursor change uniforms, and so animates for a
    /// while after the cursor moves.
    pub fn uses_cursor_motion(&self) -> bool {
        self.is_active() && self.uses_cursor_motion
    }

    /// Compiles `shaders`, replacing the current chain. Shaders that fail to
    /// compile are skipped. Returns one message per failure.
    ///
    /// `animation` forces continuous redraws on or off. `None` animates when a
    /// shader reads `tron.time` or `tron.frame`. Shaders that read
    /// `tron.cursor_change_time` only animate after cursor moves.
    pub fn set_shaders(
        &mut self,
        device: &wgpu::Device,
        shaders: &[PostShader],
        animation: Option<bool>,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        let mut passes = Vec::new();
        let mut uses_time = false;
        let mut uses_previous = false;
        let mut uses_cursor_motion = false;
        let prelude_lines = PRELUDE.lines().count();
        for shader in shaders {
            let source = format!("{PRELUDE}{}", shader.source);
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&shader.name),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let pipeline = self.pipeline(device, &shader.name, &module, "tron_vs", "tron_fs", &self.pipeline_layout);
            if let Some(error) = pollster::block_on(scope.pop()) {
                errors.push(format!(
                    "shader `{}` failed to compile (line numbers include {prelude_lines} prelude lines):\n{error}",
                    shader.name
                ));
                continue;
            }
            let reads = |name: &str| shader.source.contains(name);
            if reads("tron.cursor_change_time") || reads("tron.previous_cursor") {
                uses_cursor_motion = true;
            } else if reads("tron.time") || reads("tron.frame") {
                uses_time = true;
            }
            uses_previous |= reads("previous(");
            passes.push(pipeline);
        }
        self.passes = passes;
        self.animated = animation.unwrap_or(uses_time);
        self.uses_cursor_motion = animation.is_none() && uses_cursor_motion;
        self.uses_previous = uses_previous;
        if uses_previous && self.blit.is_none() {
            self.blit = Some(self.create_blit(device));
        }
        let size = self.size;
        self.size = (0, 0);
        self.resize(device, size.0, size.1);
        errors
    }

    fn pipeline(
        &self,
        device: &wgpu::Device,
        label: &str,
        module: &wgpu::ShaderModule,
        vertex: &str,
        fragment: &str,
        layout: &wgpu::PipelineLayout,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vertex),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(fragment),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: self.cache.as_ref(),
        })
    }

    fn create_blit(&self, device: &wgpu::Device) -> Blit {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post blit"),
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT.into()),
        });
        let pipeline = self.pipeline(device, "post blit", &module, "vs", "fs", &pipeline_layout);
        Blit { pipeline, layout, bind_groups: Vec::new() }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if !self.is_active() {
            self.targets.clear();
            self.history.clear();
            self.bind_groups.clear();
            self.size = size;
            return;
        }
        if size == self.size && !self.targets.is_empty() {
            return;
        }
        self.size = size;
        let texture = |label| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            Target { _texture: texture, view }
        };
        let count = if self.passes.len() > 1 { 2 } else { 1 };
        self.targets = (0..count).map(|i| texture(if i == 0 { "post target a" } else { "post target b" })).collect();
        // New history textures start zeroed: the previous frame reads as transparent.
        self.history =
            if self.uses_previous { vec![texture("post history a"), texture("post history b")] } else { Vec::new() };
        self.history_current = 0;

        let bind_group = |input: &wgpu::TextureView, previous: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(input) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(previous) },
                ],
            })
        };
        self.bind_groups = self
            .targets
            .iter()
            .map(|target| {
                if self.history.is_empty() {
                    // Nothing reads binding 3; the input view fills it at no cost.
                    vec![bind_group(&target.view, &target.view)]
                } else {
                    self.history.iter().map(|history| bind_group(&target.view, &history.view)).collect()
                }
            })
            .collect();
        if let Some(blit) = &mut self.blit {
            blit.bind_groups = self
                .history
                .iter()
                .map(|history| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("post blit"),
                        layout: &blit.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&history.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                        ],
                    })
                })
                .collect();
        }
    }

    /// Where the terminal is rendered when the chain is active.
    pub fn input_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.first().map(|t| &t.view)
    }

    pub fn run(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        uniforms: &PostUniforms,
    ) {
        if self.targets.is_empty() {
            return;
        }
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(uniforms));
        let with_history = self.history.len() == 2;
        let read = self.history_current;
        let write = 1 - read;
        let last = self.passes.len() - 1;
        for (i, pipeline) in self.passes.iter().enumerate() {
            let input = i % self.targets.len();
            let view = match (i == last, with_history) {
                (true, true) => &self.history[write].view,
                (true, false) => output,
                (false, _) => &self.targets[(i + 1) % self.targets.len()].view,
            };
            let bind_group = &self.bind_groups[input][if with_history { read } else { 0 }];
            draw(encoder, view, pipeline, bind_group);
        }
        if with_history && let Some(blit) = &self.blit {
            draw(encoder, output, &blit.pipeline, &blit.bind_groups[write]);
            self.history_current = write;
        }
    }
}

fn draw(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("post"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniforms_match_the_prelude_layout() {
        assert_eq!(size_of::<PostUniforms>(), 96);
        assert_eq!(std::mem::offset_of!(PostUniforms, background), 48);
        assert_eq!(std::mem::offset_of!(PostUniforms, previous_cursor), 64);
        assert_eq!(std::mem::offset_of!(PostUniforms, cursor_change_time), 80);
        assert!(PRELUDE.contains("previous_cursor: vec4<f32>") && PRELUDE.contains("fn previous("));
    }
}
